//! HLS preview and recording verification helpers for mixed scenarios.

use super::*;

fn mixed_recording_scenario_token(cfg: &str) -> String {
    cfg.replace('.', "_")
}

fn mixed_recording_name_matches_cfg(name: &str, cfg: &str) -> bool {
    name.ends_with(".mp4")
        && !name.ends_with(".tmp.mp4")
        && name.contains(&mixed_recording_scenario_token(cfg))
}

fn media_recording_play_name(entry: &Value) -> Option<&str> {
    entry["playName"]
        .as_str()
        .or_else(|| entry["name"].as_str())
        .filter(|name| name.ends_with(".mp4") && !name.ends_with(".tmp.mp4"))
}

fn media_recording_identity(entry: &Value) -> Option<String> {
    entry["recordingId"]
        .as_str()
        .map(str::to_string)
        .or_else(|| media_recording_play_name(entry).map(str::to_string))
}

async fn api_media_recording_identities(api: &RampApi) -> Result<HashSet<String>, String> {
    let media = api.get_json("/api/v1/media").await?;
    let files = media["files"]
        .as_array()
        .ok_or("/api/v1/media response missing files")?;
    Ok(files
        .iter()
        .filter(|file| media_recording_play_name(file).is_some())
        .filter_map(media_recording_identity)
        .collect())
}

async fn wait_for_api_recording_for_pipeline(
    api: &RampApi,
    pipeline_id: &str,
    before: &HashSet<String>,
    cfg: &str,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let media = api.get_json("/api/v1/media").await?;
        let files = media["files"]
            .as_array()
            .ok_or("/api/v1/media response missing files")?;
        if let Some(entry) = files.iter().find(|entry| {
            entry["pipelineId"].as_str() == Some(pipeline_id)
                && media_recording_play_name(entry).is_some()
                && media_recording_identity(entry)
                    .as_ref()
                    .is_some_and(|identity| !before.contains(identity))
        }) {
            return Ok(entry.clone());
        }
        if let Some(entry) = files.iter().find(|entry| {
            media_recording_play_name(entry).is_some_and(|name| {
                mixed_recording_name_matches_cfg(name, cfg)
                    && media_recording_identity(entry)
                        .as_ref()
                        .is_none_or(|identity| !before.contains(identity))
            })
        }) {
            return Ok(entry.clone());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no new recording for pipeline {pipeline_id} / {cfg} appeared in /api/v1/media within {}s",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn mixed_hls_preview_timeout(case: MixedInputCase) -> Duration {
    match (case.codec(), case.is_multi_track()) {
        (MixedVideoCodec::H265, true) => Duration::from_secs(60),
        _ => Duration::from_secs(30),
    }
}

pub(crate) async fn verify_mixed_hls_preview(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    expected_dimensions: &str,
    case: MixedInputCase,
    resume: &mut MixedResume,
) -> Result<Value, String> {
    let id = mixed_scenario_check_id(cfg, "hls_preview");
    if !resume.allows(&id) {
        return Ok(json!({"skipped": true}));
    }
    let started = Instant::now();
    let (_status, playlist_body) =
        wait_for_hls_playlist_ready(api, pipeline_id, mixed_hls_preview_timeout(case)).await?;
    let expected_audio_tracks = case.expected_audio_tracks();
    let audio_renditions = playlist_body.matches("#EXT-X-MEDIA:TYPE=AUDIO").count();
    if audio_renditions != expected_audio_tracks {
        let message = format!(
            "hls-preview {cfg}: expected {expected_audio_tracks} audio renditions, got {audio_renditions}"
        );
        emit_mixed_result(
            env,
            cfg,
            &id,
            "fail",
            started.elapsed(),
            Some(json!({
                "message": message,
                "expectedAudioRenditions": expected_audio_tracks,
                "audioRenditions": audio_renditions,
                "playlist": playlist_body,
            })),
        )?;
        return Err(message);
    }
    let preview =
        wait_for_api_hls_preview_state(api, pipeline_id, true, Duration::from_secs(10)).await?;
    let playlist_url = format!(
        "http://127.0.0.1:{}/hls/{pipeline_id}/master.m3u8",
        env.restream_http
    );
    match probe_dims_ramp_with_cookie(&playlist_url, api.cookie.as_deref()).await {
        Ok(dimensions) if dimensions == expected_dimensions => {
            let summary = json!({
                "inputCase": case.scenario_id(),
                "codec": case.codec_name(),
                "trackLayout": case.track_layout_name(),
                "playlistReady": playlist_body.contains("#EXTM3U"),
                "expectedAudioRenditions": expected_audio_tracks,
                "audioRenditions": audio_renditions,
                "preview": preview,
                "expected": expected_dimensions,
                "got": dimensions,
                "url": playlist_url,
            });
            emit_mixed_result(
                env,
                cfg,
                &id,
                "pass",
                started.elapsed(),
                Some(summary.clone()),
            )?;
            emit_mixed_timing(
                env,
                cfg,
                "check.hls_preview",
                "pass",
                started.elapsed(),
                Some(json!({
                    "expected": expected_dimensions,
                    "got": dimensions,
                })),
            )?;
            log_mixed_ok(env, &format!("hls-preview: {cfg} -> {dimensions}"))?;
            Ok(summary)
        }
        Ok(dimensions) => {
            let message =
                format!("hls-preview {cfg}: expected {expected_dimensions}, got {dimensions}");
            emit_mixed_result(
                env,
                cfg,
                &id,
                "fail",
                started.elapsed(),
                Some(json!({
                    "message": message,
                    "expected": expected_dimensions,
                    "got": dimensions,
                    "url": playlist_url,
                })),
            )?;
            emit_mixed_timing(
                env,
                cfg,
                "check.hls_preview",
                "fail",
                started.elapsed(),
                Some(json!({
                    "expected": expected_dimensions,
                    "got": dimensions,
                })),
            )?;
            Err(message)
        }
        Err(error) => {
            let message = format!("hls-preview {cfg}: ffprobe failed: {error}");
            emit_mixed_result(
                env,
                cfg,
                &id,
                "fail",
                started.elapsed(),
                Some(json!({
                    "message": message,
                    "error": error,
                    "url": playlist_url,
                })),
            )?;
            emit_mixed_timing(
                env,
                cfg,
                "check.hls_preview",
                "fail",
                started.elapsed(),
                Some(json!({"error": error})),
            )?;
            Err(message)
        }
    }
}

pub(crate) async fn verify_optional_mixed_hls_preview(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    case: MixedInputCase,
    resume: &mut MixedResume,
) -> Result<Option<Value>, String> {
    if env.check_selected("hls") {
        verify_mixed_hls_preview(
            env,
            api,
            cfg,
            pipeline_id,
            case.hls_preview_expected_dimensions(),
            case,
            resume,
        )
        .await
        .map(Some)
    } else {
        Ok(None)
    }
}

pub(crate) async fn verify_mixed_recording(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    case: MixedInputCase,
    resume: &mut MixedResume,
) -> Result<Value, String> {
    if !env.check_selected("recording") {
        return Ok(json!({"skipped": true}));
    }
    let id = mixed_scenario_check_id(cfg, "recording");
    if !resume.allows(&id) {
        return Ok(json!({"skipped": true}));
    }

    let started = Instant::now();
    let before_recordings = api_media_recording_identities(api).await?;
    api.post_empty(&format!("/api/v1/pipelines/{pipeline_id}/recording/start"))
        .await?;
    wait_for_api_recording_state(api, pipeline_id, true, Duration::from_secs(10)).await?;
    tokio::time::sleep(Duration::from_secs(6)).await;
    api.post_empty(&format!("/api/v1/pipelines/{pipeline_id}/recording/stop"))
        .await?;
    wait_for_api_recording_state(api, pipeline_id, false, Duration::from_secs(20)).await?;

    let recording_entry = wait_for_api_recording_for_pipeline(
        api,
        pipeline_id,
        &before_recordings,
        cfg,
        Duration::from_secs(30),
    )
    .await?;
    let recording_name =
        media_recording_play_name(&recording_entry).ok_or("recording file missing file name")?;
    let recording_path = env.media_dir.join(recording_name);
    if !recording_path.exists() {
        return Err(format!(
            "recording listed by API but missing on disk: {}",
            recording_path.display()
        ));
    }

    let probe = ffprobe(recording_path.to_string_lossy().as_ref()).await?;
    let streams = normalized_streams(&probe)?;
    let stream_array = streams
        .as_array()
        .ok_or("recording normalized stream list missing array")?;
    let video_codec = stream_array
        .iter()
        .find(|stream| stream["type"] == "video")
        .and_then(|stream| stream["codec"].as_str())
        .unwrap_or_default();
    let audio_tracks = stream_array
        .iter()
        .filter(|stream| stream["type"] == "audio")
        .count();
    let expected_video_codec = case.expected_video_codec();
    let expected_audio_tracks = case.expected_audio_tracks();
    let passed = video_codec == expected_video_codec && audio_tracks == expected_audio_tracks;
    let summary = json!({
        "inputCase": case.scenario_id(),
        "recordingFile": recording_path,
        "expectedVideoCodec": expected_video_codec,
        "videoCodec": video_codec,
        "expectedAudioTracks": expected_audio_tracks,
        "audioTracks": audio_tracks,
        "entry": recording_entry,
        "normalizedStreams": streams,
        "probe": probe,
    });
    emit_mixed_result(
        env,
        cfg,
        &id,
        if passed { "pass" } else { "fail" },
        started.elapsed(),
        Some(summary.clone()),
    )?;
    emit_mixed_timing(
        env,
        cfg,
        "check.recording",
        if passed { "pass" } else { "fail" },
        started.elapsed(),
        Some(json!({
            "expectedVideoCodec": expected_video_codec,
            "videoCodec": video_codec,
            "expectedAudioTracks": expected_audio_tracks,
            "audioTracks": audio_tracks,
        })),
    )?;
    if passed {
        log_mixed_ok(
            env,
            &format!("recording: {cfg} -> {video_codec}, audio_tracks={audio_tracks}"),
        )?;
        Ok(summary)
    } else {
        Err(format!(
            "recording {cfg}: expected {expected_video_codec} with {expected_audio_tracks} audio tracks, got {video_codec} with {audio_tracks}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_recording_name_match_requires_exact_scenario_token() {
        assert!(mixed_recording_name_matches_cfg(
            "recording_20260707T012755_mixed_asset_file_h265_a1_bf2.mp4",
            "mixed.asset.file.h265.a1.bf2"
        ));
        assert!(!mixed_recording_name_matches_cfg(
            "recording_20260707T012755_mixed_asset_file_h265_a1_bf0.mp4",
            "mixed.asset.file.h265.a1.bf2"
        ));
    }

    #[test]
    fn mixed_recording_name_match_rejects_temporary_outputs() {
        assert!(!mixed_recording_name_matches_cfg(
            "recording_20260707T012755_mixed_asset_file_h265_a1_bf0.tmp.mp4",
            "mixed.asset.file.h265.a1.bf0"
        ));
    }

    #[test]
    fn media_recording_identity_prefers_recording_id() {
        let entry = json!({
            "name": "recording_20260707T012755_pipe.mp4",
            "playName": "recording_20260707T012755_pipe.mp4",
            "recordingId": "rec-123",
        });

        assert_eq!(media_recording_identity(&entry).as_deref(), Some("rec-123"));
    }

    #[test]
    fn media_recording_play_name_rejects_temporary_outputs() {
        let entry = json!({
            "name": "recording_20260707T012755_pipe.tmp.mp4",
            "playName": "recording_20260707T012755_pipe.tmp.mp4",
        });

        assert_eq!(media_recording_play_name(&entry), None);
    }
}
