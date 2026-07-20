//! In-memory HLS segmenter — muxes to MPEG-TS via in-house `TsMuxer`, splits on
//! keyframe boundaries, and stores segments in `HlsStore`. No disk I/O, no FFmpeg,
//! no OS threads on the hot path. Segments are served directly from memory by the
//! Axum API.
//!
//! # Segment Lifecycle
//!
//! ```text
//! RingBuffer → TsMuxer (inline) → segment accumulator → HlsStore
//! ```

pub mod fmp4;
pub mod preview;
pub mod preview_graph;
mod segmenter;
mod store;
pub mod upload;

#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use bytes::Bytes;

use crate::domain::stage::StageKey;
#[cfg(test)]
use crate::domain::stage::StageKind;
#[cfg(test)]
use crate::media::engine::MediaEngine;
use crate::media::metadata::VideoMeta;
#[cfg(test)]
use crate::media::packet::MediaType;
#[cfg(test)]
use crate::media::ring_buffer::RingBuffer;

pub use segmenter::start_hls_segmenter;
pub use store::{HlsSegmentSnapshot, HlsSegmentVariant, HlsStore, HlsStoreSnapshot};

const MIN_SEGMENT_SECS: f64 = 1.0;
const SEGMENT_CAPACITY: usize = 8 * 1024 * 1024;
// Keep a longer live window so preview clients can still fetch segments that are
// still referenced by the playlist while the stream is moving forward.
const MAX_SEGMENTS: usize = 20;

#[derive(Debug, Clone, Default)]
pub struct HlsSegmenterStart {
    pub video_meta_override: Option<VideoMeta>,
    pub planned_stage_key: Option<StageKey>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HlsConfig {
    pub min_segment_secs: f64,
    pub segment_capacity: usize,
    pub max_segments: usize,
}

impl Default for HlsConfig {
    fn default() -> Self {
        Self {
            min_segment_secs: MIN_SEGMENT_SECS,
            segment_capacity: SEGMENT_CAPACITY,
            max_segments: MAX_SEGMENTS,
        }
    }
}

impl HlsConfig {
    pub fn from_app_config(config: &crate::AppConfig) -> Self {
        Self {
            min_segment_secs: config.hls_min_segment_ms,
            segment_capacity: config.hls_segment_capacity_bytes,
            max_segments: config.hls_max_segments,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio_util::sync::CancellationToken;

    fn test_store() -> HlsStore {
        HlsStore::with_config(HlsConfig::default())
    }

    #[test]
    fn playlist_references_stored_segments() {
        let store = test_store();
        store.push_segment(2.25, Bytes::from_static(b"segment-zero"));
        store.push_segment(3.5, Bytes::from_static(b"segment-one"));

        let playlist = store.get_playlist().expect("playlist");
        assert!(playlist.contains("#EXT-X-TARGETDURATION:6"));
        assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:0"));
        assert!(playlist.contains("#EXTINF:2.250,\nseg0.ts"));
        assert!(playlist.contains("#EXTINF:3.500,\nseg1.ts"));
        assert_eq!(
            store.get_segment(1).as_deref(),
            Some(b"segment-one".as_slice())
        );
    }

    #[test]
    fn playlist_can_reference_variant_segment_paths() {
        let store = test_store();
        store.push_segment(2.25, Bytes::from_static(b"segment-zero"));
        store.push_segment(3.5, Bytes::from_static(b"segment-one"));

        let playlist = store
            .get_playlist_with_segment_uri(|index| format!("audio/15/seg{index}.ts"))
            .expect("playlist");

        assert!(playlist.contains("#EXTINF:2.250,\naudio/15/seg0.ts"));
        assert!(playlist.contains("#EXTINF:3.500,\naudio/15/seg1.ts"));
    }

    #[test]
    fn variant_cache_evicts_with_source_segments() {
        let store = HlsStore::with_config(HlsConfig {
            max_segments: 1,
            ..HlsConfig::default()
        });
        store.push_segment(2.0, Bytes::from_static(b"source-zero"));
        store.put_variant_segment(
            0,
            HlsSegmentVariant::Audio(3),
            Bytes::from_static(b"variant-zero"),
        );
        assert_eq!(
            store
                .get_variant_segment(0, HlsSegmentVariant::Audio(3))
                .as_deref(),
            Some(b"variant-zero".as_slice())
        );

        store.push_segment(2.0, Bytes::from_static(b"source-one"));

        assert!(store.get_segment(0).is_none());
        assert!(
            store
                .get_variant_segment(0, HlsSegmentVariant::Audio(3))
                .is_none()
        );
    }

    #[test]
    fn target_duration_tracks_longest_segment() {
        let store = test_store();
        store.push_segment(7.2, Bytes::from_static(b"long-segment"));

        let playlist = store.get_playlist().expect("playlist");
        assert!(playlist.contains("#EXT-X-TARGETDURATION:8"));
    }

    #[test]
    fn playlist_window_is_bounded_and_advances_media_sequence() {
        let store = test_store();
        for index in 0..(MAX_SEGMENTS as u64 + 2) {
            store.push_segment(2.0, Bytes::from(index.to_be_bytes().to_vec()));
        }

        let playlist = store.get_playlist().expect("playlist");
        assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:2"));
        assert!(!playlist.contains("seg0.ts"));
        assert!(!playlist.contains("seg1.ts"));
        assert!(playlist.contains("seg11.ts"));
        assert!(store.get_segment(0).is_none());
        assert!(store.get_segment(2).is_some());
    }

    #[test]
    fn custom_max_segments_controls_live_window() {
        let store = HlsStore::with_config(HlsConfig {
            max_segments: 3,
            ..HlsConfig::default()
        });
        for index in 0..5u64 {
            store.push_segment(2.0, Bytes::from(index.to_be_bytes().to_vec()));
        }

        let playlist = store.get_playlist().expect("playlist");
        assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:2"));
        assert!(store.get_segment(0).is_none());
        assert!(store.get_segment(1).is_none());
        assert!(store.get_segment(2).is_some());
        assert_eq!(playlist.matches(".ts").count(), 3);
    }

    #[test]
    fn keeps_a_longer_live_window_for_preview_clients() {
        let store = test_store();
        for index in 0..14u64 {
            store.push_segment(2.0, Bytes::from(format!("segment-{index}").into_bytes()));
        }

        assert!(store.get_segment(3).is_some());
        assert!(store.get_segment(13).is_some());
    }

    #[test]
    fn clear_resets_segments_and_index() {
        let store = test_store();
        store.push_segment(2.0, Bytes::from_static(b"data"));
        store.clear();
        assert!(store.get_playlist().is_none());
        assert!(store.get_segment(0).is_none());
    }

    #[test]
    fn empty_store_playlist_returns_none() {
        assert!(test_store().get_playlist().is_none());
    }

    #[test]
    fn get_segment_nonexistent_returns_none() {
        let store = test_store();
        store.push_segment(2.0, Bytes::from_static(b"data"));
        assert!(store.get_segment(999).is_none());
    }

    #[test]
    fn get_segment_finds_by_exact_index() {
        let store = test_store();
        store.push_segment(2.0, Bytes::from_static(b"first"));
        store.push_segment(3.0, Bytes::from_static(b"second"));
        assert_eq!(store.get_segment(1).as_deref(), Some(b"second".as_slice()));
    }

    #[test]
    fn target_duration_never_decreases() {
        let store = test_store();
        store.push_segment(8.0, Bytes::from_static(b"long"));
        store.push_segment(2.0, Bytes::from_static(b"short"));
        let playlist = store.get_playlist().unwrap();
        assert!(playlist.contains("#EXT-X-TARGETDURATION:8"));
        assert!(!playlist.contains("#EXT-X-TARGETDURATION:2"));
    }

    #[test]
    fn exact_max_segments_does_not_evict_first() {
        let store = test_store();
        for i in 0..MAX_SEGMENTS as u64 {
            store.push_segment(2.0, Bytes::from(i.to_be_bytes().to_vec()));
        }
        assert!(store.get_segment(0).is_some());
    }

    #[test]
    fn new_store_has_empty_initial_state() {
        let store = test_store();
        assert!(store.get_playlist().is_none());
        assert!(store.get_segment(0).is_none());
    }

    #[test]
    fn push_segment_assigns_sequential_indices() {
        let store = test_store();
        store.push_segment(1.0, Bytes::from_static(b"a"));
        store.push_segment(1.0, Bytes::from_static(b"b"));
        store.push_segment(1.0, Bytes::from_static(b"c"));
        assert!(store.get_segment(0).is_some());
        assert!(store.get_segment(1).is_some());
        assert!(store.get_segment(2).is_some());
    }

    #[test]
    fn playlist_exact_extinf_format() {
        let store = test_store();
        store.push_segment(2.25, Bytes::new());
        let playlist = store.get_playlist().unwrap();
        assert!(playlist.contains("#EXTINF:2.250,"));
        assert!(playlist.contains("seg0.ts"));
    }

    #[test]
    fn get_segment_returns_none_before_first_index() {
        let store = test_store();
        // Start at index 5
        for _ in 0..5 {
            store.push_segment(2.0, Bytes::new());
        }
        // Clear sets next_index=0, so push 2 more starting at index 0
        store.clear();
        store.push_segment(1.0, Bytes::from_static(b"a"));
        store.push_segment(1.0, Bytes::from_static(b"b"));
        // Now get_segment(5) should be None since it was cleared
        assert!(store.get_segment(5).is_none());
        // And get_segment(0) should exist
        assert!(store.get_segment(0).is_some());
    }

    #[test]
    fn media_sequence_advances_after_eviction() {
        let store = test_store();
        // Fill beyond MAX_SEGMENTS to trigger eviction
        for _ in 0..(MAX_SEGMENTS as u64 + 5) {
            store.push_segment(2.0, Bytes::new());
        }
        let playlist = store.get_playlist().unwrap();
        assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:5"));
        // Oldest segment should be gone
        assert!(store.get_segment(0).is_none());
    }

    #[test]
    fn playlist_range_covers_entire_window() {
        let store = test_store();
        let n = MAX_SEGMENTS as u64;
        for _ in 0..n {
            store.push_segment(2.0, Bytes::new());
        }
        let playlist = store.get_playlist().unwrap();
        assert!(playlist.contains("seg0.ts"));
        assert!(playlist.contains(&format!("seg{}.ts", n - 1)));
    }

    #[test]
    fn snapshot_returns_none_when_empty() {
        let store = test_store();
        assert!(store.snapshot().is_none());
    }

    #[test]
    fn snapshot_contains_playlist_and_all_segments() {
        let store = test_store();
        store.push_segment(2.0, Bytes::from_static(b"data0"));
        store.push_segment(3.0, Bytes::from_static(b"data1"));

        let snap = store.snapshot().expect("snapshot");
        assert!(snap.playlist.contains("seg0.ts"));
        assert!(snap.playlist.contains("seg1.ts"));
        assert_eq!(snap.segments.len(), 2);
        assert_eq!(snap.segments[0].data.as_ref(), b"data0");
        assert_eq!(snap.segments[1].data.as_ref(), b"data1");
    }

    #[test]
    fn stream_metadata_roundtrip() {
        let store = test_store();

        let (v, a) = store.stream_metadata();
        assert!(v.is_none());
        assert!(a.is_empty());

        let video = crate::media::metadata::VideoMeta {
            codec: "h264".into(),
            width: 1920,
            height: 1080,
            fps: 30.0,
            bw: None,
            pid: None,
            language: None,
            title: None,
            profile: None,
            level: None,
            pixel_format: None,
        };
        let audio = crate::media::metadata::AudioMeta {
            codec: "aac".into(),
            sample_rate: 48000,
            channels: 2,
            track_index: 0,
            ..Default::default()
        };
        store.set_stream_metadata(Some(video.clone()), vec![audio.clone()]);

        let (v2, a2) = store.stream_metadata();
        assert!(v2.is_some());
        assert_eq!(v2.unwrap().codec, "h264");
        assert_eq!(a2.len(), 1);
        assert_eq!(a2[0].codec, "aac");
    }

    #[test]
    fn hls_config_maps_from_app_config() {
        let app_config = crate::AppConfig {
            hls_min_segment_ms: 0.5,
            hls_segment_capacity_bytes: 524_288,
            hls_max_segments: 9,
            ..crate::AppConfig::default()
        };

        let cfg = HlsConfig::from_app_config(&app_config);

        assert_eq!(cfg.min_segment_secs, 0.5);
        assert_eq!(cfg.segment_capacity, 524_288);
        assert_eq!(cfg.max_segments, 9);
    }

    #[test]
    fn put_variant_segment_ignored_for_unknown_source_index() {
        let store = test_store();
        store.push_segment(2.0, Bytes::from_static(b"seg0"));
        // index 99 doesn't exist — the put should be silently dropped
        store.put_variant_segment(99, HlsSegmentVariant::Audio(0), Bytes::from_static(b"v"));
        assert!(
            store
                .get_variant_segment(99, HlsSegmentVariant::Audio(0))
                .is_none()
        );
    }

    #[tokio::test]
    async fn preview_ring_uses_alternate_audio_tracks_when_primary_audio_is_empty() {
        let store = Arc::new(test_store());
        let engine = Arc::new(MediaEngine::new());
        let source_ring = Arc::new(RingBuffer::new(2048));
        let preview_ring = Arc::new(RingBuffer::new(2048));
        let cancel = CancellationToken::new();

        let (video, audio_tracks, packets) =
            crate::test_fixtures::primary_av_packets_for_codec("h264").expect("fixture packets");
        let video_packets = packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Video)
            .cloned()
            .collect::<Vec<_>>();

        source_ring.set_codec_hint("h264");
        source_ring.set_audio_tracks(audio_tracks.clone());
        preview_ring.set_codec_hint("h264");
        preview_ring.set_audio_tracks(Vec::new());

        let task = tokio::spawn(start_hls_segmenter(
            "preview-audio-fallback".to_string(),
            store.clone(),
            preview_ring.clone(),
            Some(source_ring.clone()),
            engine,
            cancel.clone(),
            HlsSegmenterStart {
                video_meta_override: Some(video.clone()),
                planned_stage_key: None,
            },
        ));

        let reader_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let preview_ready = preview_ring
                .reader_snapshots()
                .iter()
                .any(|snapshot| snapshot.name == "hls:preview-audio-fallback");
            let audio_ready = source_ring
                .reader_snapshots()
                .iter()
                .any(|snapshot| snapshot.name == "hls-audio:preview-audio-fallback");
            if preview_ready && audio_ready {
                break;
            }
            assert!(
                tokio::time::Instant::now() < reader_deadline,
                "hls preview readers did not attach in time"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        source_ring.push_batch(packets);
        preview_ring.push_batch(video_packets);

        let metadata_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let (_, store_audio_tracks) = store.stream_metadata();
            if !store_audio_tracks.is_empty() {
                assert_eq!(store_audio_tracks.len(), audio_tracks.len());
                break;
            }
            assert!(
                tokio::time::Instant::now() < metadata_deadline,
                "hls preview store never adopted alternate audio-ring metadata"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        cancel.cancel();
        task.await.expect("segmenter task should stop cleanly");
    }

    #[tokio::test]
    async fn hls_segmenter_registers_planned_protocol_stage_key() {
        let store = Arc::new(HlsStore::with_config(HlsConfig {
            min_segment_secs: 0.0,
            segment_capacity: 2 * 1024 * 1024,
            max_segments: 8,
        }));
        let engine = Arc::new(MediaEngine::new());
        let source_ring = Arc::new(RingBuffer::new(256));
        let cancel = CancellationToken::new();
        let planned_key = StageKey::new(
            "hls-planned-key",
            StageKind::hls_segmenter(StageKind::source()),
        );

        let task = tokio::spawn(start_hls_segmenter(
            "hls-planned-key".to_string(),
            store,
            source_ring,
            None,
            engine.clone(),
            cancel.clone(),
            HlsSegmenterStart {
                video_meta_override: Some(VideoMeta {
                    codec: "h264".to_string(),
                    ..Default::default()
                }),
                planned_stage_key: Some(planned_key.clone()),
            },
        ));

        let snapshot_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(snapshot) = engine.stage_runtime_snapshot(&planned_key).await {
                assert_eq!(snapshot.key, planned_key);
                assert_eq!(
                    snapshot.backend,
                    crate::media::stage_lifecycle::StageBackendKind::HlsSegmenter
                );
                let runtime = engine
                    .stages
                    .runtimes
                    .read()
                    .await
                    .get(&planned_key)
                    .cloned()
                    .expect("planned HLS stage should be runtime-backed");
                assert!(
                    runtime.ring.is_none(),
                    "HLS segmenters are non-ring protocol stages"
                );
                assert!(
                    !engine
                        .stages
                        .metrics
                        .read()
                        .await
                        .contains_key(&planned_key),
                    "HLS metrics should be owned by StageRuntime, not the side map"
                );
                assert!(
                    !engine
                        .stages
                        .lifecycles
                        .read()
                        .await
                        .contains_key(&planned_key),
                    "HLS lifecycle should be owned by StageRuntime, not the side map"
                );
                break;
            }
            assert!(
                tokio::time::Instant::now() < snapshot_deadline,
                "HLS segmenter did not register the planned graph stage key"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        cancel.cancel();
        task.await.expect("segmenter task should stop cleanly");
        assert!(
            !engine
                .stages
                .runtimes
                .read()
                .await
                .contains_key(&planned_key),
            "HLS runtime should be removed on shutdown"
        );
    }

    #[tokio::test]
    async fn hls_segment_boundaries_preserve_non_decreasing_dts_per_stream() {
        let store = Arc::new(HlsStore::with_config(HlsConfig {
            min_segment_secs: 0.0,
            segment_capacity: 2 * 1024 * 1024,
            max_segments: 64,
        }));
        let engine = Arc::new(MediaEngine::new());
        let source_ring = Arc::new(RingBuffer::new(4096));
        let cancel = CancellationToken::new();

        let (video, audio_tracks, packets) =
            crate::test_fixtures::primary_av_packets_for_codec("h264").expect("fixture packets");
        let keyframes = packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Video && packet.is_keyframe)
            .count();
        assert!(
            keyframes >= 2,
            "fixture needs at least two video keyframes to exercise segment boundaries"
        );

        source_ring.set_codec_hint("h264");
        source_ring.set_audio_tracks(audio_tracks);

        let task = tokio::spawn(start_hls_segmenter(
            "hls-ts-monotonic".to_string(),
            store.clone(),
            source_ring.clone(),
            None,
            engine,
            cancel.clone(),
            HlsSegmenterStart {
                video_meta_override: Some(video),
                planned_stage_key: None,
            },
        ));

        let attach_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let attached = source_ring
                .reader_snapshots()
                .iter()
                .any(|snapshot| snapshot.name == "hls:hls-ts-monotonic");
            if attached {
                break;
            }
            assert!(
                tokio::time::Instant::now() < attach_deadline,
                "hls segmenter reader did not attach in time"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        source_ring.push_batch(packets);

        let segment_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let segment_count = store
                .snapshot()
                .map(|snapshot| snapshot.segments.len())
                .unwrap_or(0);
            if segment_count >= 2 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < segment_deadline,
                "hls segmenter did not emit multiple segments in time"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        cancel.cancel();
        task.await.expect("segmenter task should stop cleanly");

        let snapshot = store.snapshot().expect("snapshot should contain segments");
        assert!(
            snapshot.segments.len() >= 2,
            "segment boundary proof requires at least two segments"
        );

        let mut global_last_dts: HashMap<(u8, u32), i64> = HashMap::new();
        let mut prev_segment_last_dts: Option<HashMap<(u8, u32), i64>> = None;

        for segment in snapshot.segments {
            let mut demuxer = crate::media::mpegts::TsDemuxer::new();
            demuxer.feed(&segment.data);
            demuxer.flush();
            let packets = demuxer.drain();
            assert!(
                !packets.is_empty(),
                "hls segment {} should contain muxed packets",
                segment.index
            );

            let mut segment_first_dts: HashMap<(u8, u32), i64> = HashMap::new();
            let mut segment_last_dts: HashMap<(u8, u32), i64> = HashMap::new();

            for packet in packets {
                let stream = (packet.media_type as u8, packet.track_index);
                if let Some(previous) = global_last_dts.get(&stream) {
                    assert!(
                        packet.dts >= *previous,
                        "stream {:?} DTS regressed across HLS segments: {} < {}",
                        stream,
                        packet.dts,
                        previous
                    );
                }
                segment_first_dts.entry(stream).or_insert(packet.dts);
                segment_last_dts.insert(stream, packet.dts);
                global_last_dts.insert(stream, packet.dts);
            }

            if let Some(previous_segment) = prev_segment_last_dts.as_ref() {
                for (stream, first_dts) in &segment_first_dts {
                    if let Some(previous_last) = previous_segment.get(stream) {
                        assert!(
                            *first_dts >= *previous_last,
                            "stream {:?} first DTS in segment {} regressed at boundary: {} < {}",
                            stream,
                            segment.index,
                            first_dts,
                            previous_last
                        );
                    }
                }
            }

            prev_segment_last_dts = Some(segment_last_dts);
        }
    }
}
