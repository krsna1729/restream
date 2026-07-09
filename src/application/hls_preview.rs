//! Application-layer HLS preview orchestration.
//!
//! Owns application policy for HLS preview requests. The media runtime owns
//! preview graph planning, fMP4 store creation, cancellation, and segmenter task
//! lifecycle.

use std::sync::Arc;

use bytes::Bytes;

use crate::media::engine::{AudioMeta, MediaEngine, VideoMeta};
use crate::media::hls_fmp4::{Fmp4HlsStore, parse_fmp4_segment_name};

#[derive(Debug)]
pub enum HlsPreviewError {
    NoStream,
}

impl std::fmt::Display for HlsPreviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoStream => f.write_str("No HLS stream"),
        }
    }
}

/// Ensure the HLS preview segmenter is running for the given pipeline.
///
/// If an active ingest exists, this asks the media runtime to create or reuse
/// the preview runtime. The runtime handles graph planning and segmenter task
/// ownership.
///
/// If no active ingest exists, returns the existing store if one is available,
/// or an error if no preview has been created yet.
pub async fn ensure_hls_preview(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
) -> Result<Arc<Fmp4HlsStore>, HlsPreviewError> {
    let has_ingest = engine.ingests.active.read().await.contains_key(pipeline_id);

    if has_ingest {
        return Ok(engine.ensure_hls_preview_runtime(pipeline_id).await);
    }

    let Some(store) = engine.get_hls_preview_store(pipeline_id).await else {
        return Err(HlsPreviewError::NoStream);
    };
    engine.touch_hls_preview(pipeline_id).await;
    Ok(store)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlsPreviewBlockedCause {
    pub stage: String,
    pub phase: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HlsPreviewReadError {
    NoStream,
    NoSegments {
        blocked_by: Option<HlsPreviewBlockedCause>,
    },
    AudioTrackNotFound,
    InvalidSegmentName,
    SegmentNotFound,
    InitSegmentNotFound,
}

pub async fn primary_playlist(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
) -> Result<String, HlsPreviewReadError> {
    let store = preview_store(engine.clone(), pipeline_id).await?;
    match store.get_primary_playlist() {
        Some(playlist) => Ok(playlist),
        None => Err(no_segments_error(engine, pipeline_id).await),
    }
}

pub async fn master_playlist(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
) -> Result<String, HlsPreviewReadError> {
    let store = preview_store(engine.clone(), pipeline_id).await?;
    if !store.has_video_playlist() && store.get_primary_playlist().is_none() {
        return Err(no_segments_error(engine, pipeline_id).await);
    }
    let (video, audio_tracks) = store.stream_metadata();
    Ok(build_hls_master_playlist(video.as_ref(), &audio_tracks))
}

pub async fn video_playlist(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
) -> Result<String, HlsPreviewReadError> {
    let store = preview_store(engine.clone(), pipeline_id).await?;
    match store
        .get_video_playlist()
        .or_else(|| store.get_primary_playlist())
    {
        Some(playlist) => Ok(playlist),
        None => Err(no_segments_error(engine, pipeline_id).await),
    }
}

pub async fn audio_playlist(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
    track_index: u32,
) -> Result<String, HlsPreviewReadError> {
    let store = preview_store(engine.clone(), pipeline_id).await?;
    let (_, audio_tracks) = store.stream_metadata();
    if !audio_tracks
        .iter()
        .any(|track| track.track_index == track_index)
    {
        return Err(HlsPreviewReadError::AudioTrackNotFound);
    }
    match store.get_audio_playlist(track_index) {
        Some(playlist) => Ok(playlist),
        None => Err(no_segments_error(engine, pipeline_id).await),
    }
}

pub async fn video_segment(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
    segment: &str,
) -> Result<Bytes, HlsPreviewReadError> {
    let store = existing_preview_store(engine, pipeline_id).await?;
    let index = parse_fmp4_segment_name(segment).ok_or(HlsPreviewReadError::InvalidSegmentName)?;
    store
        .get_video_segment(index)
        .ok_or(HlsPreviewReadError::SegmentNotFound)
}

pub async fn video_init_segment(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
) -> Result<Bytes, HlsPreviewReadError> {
    let store = existing_preview_store(engine, pipeline_id).await?;
    store
        .get_video_init_segment()
        .ok_or(HlsPreviewReadError::InitSegmentNotFound)
}

pub async fn audio_segment(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
    track_index: u32,
    segment: &str,
) -> Result<Bytes, HlsPreviewReadError> {
    let store = existing_preview_store(engine, pipeline_id).await?;
    let index = parse_fmp4_segment_name(segment).ok_or(HlsPreviewReadError::InvalidSegmentName)?;
    store
        .get_audio_segment(track_index, index)
        .ok_or(HlsPreviewReadError::SegmentNotFound)
}

pub async fn audio_init_segment(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
    track_index: u32,
) -> Result<Bytes, HlsPreviewReadError> {
    let store = existing_preview_store(engine, pipeline_id).await?;
    store
        .get_audio_init_segment(track_index)
        .ok_or(HlsPreviewReadError::InitSegmentNotFound)
}

async fn preview_store(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
) -> Result<Arc<Fmp4HlsStore>, HlsPreviewReadError> {
    ensure_hls_preview(engine, pipeline_id)
        .await
        .map_err(|err| match err {
            HlsPreviewError::NoStream => HlsPreviewReadError::NoStream,
        })
}

async fn existing_preview_store(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
) -> Result<Arc<Fmp4HlsStore>, HlsPreviewReadError> {
    engine.touch_hls_preview(pipeline_id).await;
    engine
        .get_hls_preview_store(pipeline_id)
        .await
        .ok_or(HlsPreviewReadError::NoStream)
}

async fn no_segments_error(engine: Arc<MediaEngine>, pipeline_id: &str) -> HlsPreviewReadError {
    let blocked_by = engine
        .preview_blocked_by_snapshot(pipeline_id)
        .await
        .map(|cause| HlsPreviewBlockedCause {
            stage: cause.key.to_string(),
            phase: crate::runtime::stage::phase_name(&cause.phase).to_string(),
        });
    HlsPreviewReadError::NoSegments { blocked_by }
}

pub fn quote_hls_attr(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn build_hls_master_playlist(video: Option<&VideoMeta>, audio_tracks: &[AudioMeta]) -> String {
    let mut playlist = "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-INDEPENDENT-SEGMENTS\n".to_string();
    if !audio_tracks.is_empty() {
        for (ordinal, track) in audio_tracks.iter().enumerate() {
            let mut media_attrs = vec![
                "TYPE=AUDIO".to_string(),
                format!("GROUP-ID={}", quote_hls_attr("audio")),
                format!(
                    "NAME={}",
                    quote_hls_attr(&build_hls_audio_track_name(track, ordinal))
                ),
                format!("DEFAULT={}", if ordinal == 0 { "YES" } else { "NO" }),
                "AUTOSELECT=YES".to_string(),
                format!(
                    "URI={}",
                    quote_hls_attr(&format!("audio/{}/index.m3u8", track.track_index))
                ),
            ];
            if let Some(language) = track
                .language
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                media_attrs.push(format!("LANGUAGE={}", quote_hls_attr(language)));
            }
            if track.channels > 0 {
                media_attrs.push(format!(
                    "CHANNELS={}",
                    quote_hls_attr(&track.channels.to_string())
                ));
            }
            playlist.push_str(&format!("#EXT-X-MEDIA:{}\n", media_attrs.join(",")));
        }
    }

    let bandwidth = estimate_hls_master_bandwidth(video, audio_tracks);
    let mut stream_attrs = vec![
        format!("BANDWIDTH={bandwidth}"),
        format!("AVERAGE-BANDWIDTH={bandwidth}"),
    ];
    if let Some(video) = video {
        if video.width > 0 && video.height > 0 {
            stream_attrs.push(format!("RESOLUTION={}x{}", video.width, video.height));
        }
        if video.fps.is_finite() && video.fps > 0.0 {
            stream_attrs.push(format!("FRAME-RATE={:.3}", video.fps));
        }
    }
    if let Some(codecs) = build_hls_codec_list(video, audio_tracks) {
        stream_attrs.push(format!("CODECS={}", quote_hls_attr(&codecs)));
    }
    if !audio_tracks.is_empty() {
        stream_attrs.push(format!("AUDIO={}", quote_hls_attr("audio")));
    }
    playlist.push_str(&format!("#EXT-X-STREAM-INF:{}\n", stream_attrs.join(",")));
    if video.is_some() {
        playlist.push_str("video/index.m3u8\n");
    } else if let Some(track) = audio_tracks.first() {
        playlist.push_str(&format!("audio/{}/index.m3u8\n", track.track_index));
    } else {
        playlist.push_str("index.m3u8\n");
    }
    playlist
}

pub fn build_hls_audio_track_name(track: &AudioMeta, ordinal: usize) -> String {
    if let Some(title) = track
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return title.to_string();
    }
    let base = format!("Track {}", ordinal + 1);
    match track
        .language
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(language) => format!("{base} ({language})"),
        None => base,
    }
}

pub fn estimate_hls_master_bandwidth(video: Option<&VideoMeta>, audio_tracks: &[AudioMeta]) -> u64 {
    let video_bw = video
        .and_then(|meta| meta.bw)
        .filter(|bw| bw.is_finite() && *bw > 0.0)
        .map(|bw| bw.round() as u64);
    let audio_bw = audio_tracks
        .iter()
        .map(estimate_audio_bandwidth)
        .sum::<u64>();
    let fallback_bw = 8_000_000u64.saturating_add(audio_bw);
    video_bw
        .map(|bw| bw.saturating_add(audio_bw))
        .unwrap_or(fallback_bw)
        .max(1)
}

pub fn estimate_audio_bandwidth(track: &AudioMeta) -> u64 {
    match track.codec.to_ascii_lowercase().as_str() {
        "aac" => match track.channels {
            0 | 1 => 96_000,
            2 => 128_000,
            _ => 192_000,
        },
        "mp3" => 128_000,
        "opus" => match track.channels {
            0 | 1 => 64_000,
            2 => 128_000,
            _ => 160_000,
        },
        _ => 128_000,
    }
}

pub fn build_hls_codec_list(
    video: Option<&VideoMeta>,
    audio_tracks: &[AudioMeta],
) -> Option<String> {
    let mut codecs = Vec::new();
    if let Some(video) = video.and_then(build_hls_video_codec) {
        codecs.push(video);
    }
    for codec in audio_tracks.iter().filter_map(build_hls_audio_codec) {
        if !codecs.iter().any(|existing| existing == &codec) {
            codecs.push(codec);
        }
    }
    (!codecs.is_empty()).then(|| codecs.join(","))
}

pub fn build_hls_video_codec(video: &VideoMeta) -> Option<String> {
    let codec = video.codec.trim().to_ascii_lowercase();
    match codec.as_str() {
        "h264" | "avc" => build_h264_codec_string(video),
        "hevc" | "h265" => Some(build_hevc_codec_string(video)),
        "av1" => Some("av01.0.08M.08".to_string()),
        _ => None,
    }
}

pub fn build_h264_codec_string(video: &VideoMeta) -> Option<String> {
    let profile_idc = match video.profile.as_deref().map(str::trim) {
        Some("Baseline") => 66u8,
        Some("Main") => 77u8,
        Some("Extended") => 88u8,
        Some("High") => 100u8,
        Some("High 10") => 110u8,
        Some("High 4:2:2") => 122u8,
        Some("High 4:4:4 Predictive") => 244u8,
        _ => 100u8,
    };
    let level = parse_h264_level_idc(video.level.as_deref())
        .unwrap_or_else(|| estimate_h264_level_idc(video));
    Some(format!("avc1.{profile_idc:02x}00{level:02x}"))
}

pub fn estimate_h264_level_idc(video: &VideoMeta) -> u8 {
    let width = video.width.max(1);
    let height = video.height.max(1);
    let fps = if video.fps.is_finite() && video.fps > 0.0 {
        video.fps
    } else {
        30.0
    };
    let macroblocks_per_frame = width.div_ceil(16).saturating_mul(height.div_ceil(16));
    let macroblocks_per_second = macroblocks_per_frame as f64 * fps;

    if width > 1280 || height > 720 || macroblocks_per_second > 216_000.0 {
        40
    } else if macroblocks_per_second > 108_000.0 {
        32
    } else {
        31
    }
}

pub fn parse_h264_level_idc(level: Option<&str>) -> Option<u8> {
    let level = level?.trim();
    if level.is_empty() {
        return None;
    }
    let (major, minor) = match level.split_once('.') {
        Some((major, minor)) => (major.trim(), minor.trim()),
        None => (level, "0"),
    };
    let major: u8 = major.parse().ok()?;
    let minor: u8 = minor.parse().ok()?;
    Some(major.saturating_mul(10).saturating_add(minor))
}

pub fn build_hevc_codec_string(video: &VideoMeta) -> String {
    let profile = match video.profile.as_deref().map(str::trim) {
        Some("Main") => 1u8,
        Some("Main 10") => 2u8,
        Some("Main Still Picture") => 3u8,
        _ => 1u8,
    };
    let level_tenths = video
        .level
        .as_deref()
        .and_then(parse_h265_level_tenths)
        .unwrap_or(120);
    let general_level_idc = level_tenths.saturating_mul(3);
    format!("hvc1.{profile}.6.L{general_level_idc}.B0")
}

pub fn parse_h265_level_tenths(level: &str) -> Option<u8> {
    let level = level.trim();
    if level.is_empty() {
        return None;
    }
    let (major, minor) = match level.split_once('.') {
        Some((major, minor)) => (major.trim(), minor.trim()),
        None => (level, "0"),
    };
    let major: u8 = major.parse().ok()?;
    let minor: u8 = minor.parse().ok()?;
    Some(major.saturating_mul(10).saturating_add(minor))
}

pub fn build_hls_audio_codec(track: &AudioMeta) -> Option<String> {
    let codec = track.codec.trim().to_ascii_lowercase();
    match codec.as_str() {
        "aac" => Some(match track.profile.as_deref().map(str::trim) {
            Some("Main") => "mp4a.40.1".to_string(),
            Some("SSR") => "mp4a.40.3".to_string(),
            Some("LTP/Reserved") => "mp4a.40.4".to_string(),
            _ => "mp4a.40.2".to_string(),
        }),
        "mp3" => Some("mp4a.40.34".to_string()),
        "opus" => Some("opus".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::application::hls_preview::{HlsPreviewReadError, primary_playlist, video_segment};
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::{MediaEngine, VideoMeta};
    use crate::media::ring_buffer::RingBuffer;
    use crate::media::stage_lifecycle::{StageBackendKind, StagePhase};
    use crate::media::stage_runtime::StageRuntimeManager;

    #[tokio::test]
    async fn primary_playlist_reports_graph_planned_blocked_stage_cause() {
        let engine = Arc::new(MediaEngine::new());
        let pipeline_id = "app-hls-preview-blocked";
        engine
            .try_register_ingest(pipeline_id, "stream-key", "rtmp")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                pipeline_id,
                Some(VideoMeta {
                    codec: "hevc".to_string(),
                    ..Default::default()
                }),
                None,
                None,
            )
            .await;
        engine.ensure_hls_preview_segmenter(pipeline_id).await;

        let stage_key = StageKey::new(pipeline_id, StageKind::preview("720p", StageKind::source()));
        let manager = StageRuntimeManager::new(engine.clone());
        let (handle, _) = manager
            .ensure_stage(stage_key.clone(), Arc::new(RingBuffer::new(16)), None)
            .await;
        handle.lifecycle.transition(StagePhase::WaitingForCapacity {
            backend: StageBackendKind::ExternalFfmpeg,
        });

        let err = primary_playlist(engine, pipeline_id).await.unwrap_err();

        assert_eq!(
            err,
            HlsPreviewReadError::NoSegments {
                blocked_by: Some(crate::application::hls_preview::HlsPreviewBlockedCause {
                    stage: stage_key.to_string(),
                    phase: "waitingForCapacity".to_string(),
                })
            }
        );
    }

    #[tokio::test]
    async fn video_segment_rejects_invalid_segment_name_in_application_service() {
        let engine = Arc::new(MediaEngine::new());
        let pipeline_id = "app-hls-preview-invalid-segment";
        engine.ensure_hls_preview_segmenter(pipeline_id).await;

        let err = video_segment(engine, pipeline_id, "init.mp4")
            .await
            .unwrap_err();

        assert_eq!(err, HlsPreviewReadError::InvalidSegmentName);
    }
}
