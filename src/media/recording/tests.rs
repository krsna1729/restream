use super::*;
use crate::media::avio::MemoryQueue;
use crate::media::mpegts::TsDemuxer;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use std::sync::Arc;
use tokio::process::Command as TokioCommand;
use tokio_util::sync::CancellationToken;

#[test]
fn run_ts_writer_exits_on_closed_queue() {
    let queue = Arc::new(MemoryQueue::new());
    queue.close();
    let token = CancellationToken::new();
    let temp_dir = std::env::temp_dir();
    let file_path = temp_dir.join("test_recording.ts");
    let path_str = file_path.to_string_lossy().to_string();

    let res = run_ts_writer(queue, &path_str, token);
    assert!(res.is_ok());
    let _ = std::fs::remove_file(file_path);
}

#[test]
fn sanitize_name_replaces_path_chars() {
    assert_eq!(
        sanitize_name("a/b\\c:d*e?f\"g<h>i|j"),
        "a_b_c_d_e_f_g_h_i_j"
    );
}

#[test]
fn sanitize_name_preserves_alphanumeric_and_dashes() {
    assert_eq!(sanitize_name("My-Pipeline_v2"), "My-Pipeline_v2");
}

#[test]
fn sanitize_name_collapses_spaces_for_filenames() {
    assert_eq!(sanitize_name("Main Program  01"), "Main_Program_01");
}

#[test]
fn sanitize_name_empty_string() {
    assert_eq!(sanitize_name(""), "");
}

#[test]
fn build_filename_has_ts_extension() {
    let name = build_filename("test-pipe", "recording_0000000000000001");
    assert!(name.ends_with(".ts"));
    assert!(name.starts_with("recording_"));
    assert!(!name.contains(' '));
}

#[test]
fn build_filename_contains_sanitized_name() {
    let name = build_filename("My Pipe?", "recording_0000000000000001");
    assert!(
        name.contains("My_Pipe"),
        "expected sanitized name in: {name}"
    );
}

#[test]
fn build_filename_differs_across_recordings_with_same_pipeline_name() {
    // Two pipelines can share a display name and start recording in the
    // same wall-clock second; without a per-recording token in the
    // filename they'd resolve to the same path and race on a
    // truncating File::create, corrupting/losing one recording.
    let a = build_filename("Same Name", "recording_aaaaaaaaaaaaaaaa");
    let b = build_filename("Same Name", "recording_bbbbbbbbbbbbbbbb");
    assert_ne!(a, b);
}

#[test]
fn build_mp4_path_replaces_ts_extension() {
    let ts = Path::new("/tmp/recording_20260629_demo.ts");
    assert_eq!(
        build_mp4_path(ts),
        PathBuf::from("/tmp/recording_20260629_demo.mp4")
    );
}

#[test]
fn build_mp4_temp_path_preserves_mp4_extension_for_muxer_inference() {
    let mp4 = Path::new("/tmp/recording_20260629_demo.mp4");
    assert_eq!(
        build_mp4_temp_path(mp4),
        PathBuf::from("/tmp/recording_20260629_demo.tmp.mp4")
    );
}

#[test]
fn build_conversion_state_path_adds_sidecar_suffix() {
    let ts = Path::new("/tmp/recording_20260629_demo.ts");
    assert_eq!(
        build_conversion_state_path(ts),
        PathBuf::from("/tmp/recording_20260629_demo.ts.conversion.json")
    );
}

#[tokio::test]
async fn recording_settings_default_to_deleting_source_ts() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite should connect");
    crate::db::setup_database_schema(&pool)
        .await
        .expect("schema should initialize");
    let meta_store = crate::infrastructure::sqlite_ports::SqliteMetaStore::new(pool.clone());

    assert_eq!(
        crate::application::recording::load_recording_settings(&meta_store).await,
        RecordingSettings {
            retain_source_ts: false
        }
    );
}

#[test]
fn build_recording_remux_args_targets_faststart_mp4_copy() {
    let input = Path::new("/tmp/input.ts");
    let output = Path::new("/tmp/output.tmp.mp4");
    let args = build_recording_remux_args(input, output, 3);

    assert!(args.windows(2).any(|pair| pair == ["-i", "/tmp/input.ts"]));
    assert!(args.windows(2).any(|pair| pair == ["-threads", "3"]));
    assert!(args.windows(2).any(|pair| pair == ["-c", "copy"]));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-movflags", "+faststart"])
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-bsf:a", "aac_adtstoasc"])
    );
    assert!(args.windows(2).any(|pair| pair == ["-f", "mov"]));
    assert_eq!(args.last().map(String::as_str), Some("/tmp/output.tmp.mp4"));
}

#[test]
fn ffmpeg_muxers_include_mp4_detects_mov_muxer_aliases() {
    let listing = "Formats:\n D.. = Demuxing supported\n .E. = Muxing supported\n ---\n  E mov,mp4,m4a,3gp,3g2,mj2 QuickTime / MOV\n  E mpegts MPEG-TS\n";
    assert!(ffmpeg_muxers_include_mp4(listing));
}

#[test]
fn ffmpeg_muxers_include_mp4_detects_plain_mov_muxer_name() {
    let listing = "Formats:\n D.. = Demuxing supported\n .E. = Muxing supported\n ---\n  E mov             QuickTime / MOV\n  E mpegts          MPEG-TS (MPEG-2 Transport Stream)\n";
    assert!(ffmpeg_muxers_include_mp4(listing));
}

#[test]
fn ffmpeg_muxers_include_mp4_rejects_missing_mov_muxer() {
    let listing =
        "Formats:\n .E. = Muxing supported\n ---\n  E matroska Matroska\n  E mpegts MPEG-TS\n";
    assert!(!ffmpeg_muxers_include_mp4(listing));
}

async fn remux_recording_fixture(
    settings: RecordingSettings,
) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    assert!(
        ffmpeg_supports_mp4_muxer(),
        "bundled ffmpeg must expose mp4 muxing for recording remux"
    );

    let fixture = crate::test_fixtures::canonical_h264_ts_fixture()
        .expect("checked-in TS fixture should exist");
    let temp_dir =
        std::env::temp_dir().join(format!("recording-remux-test-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
    let source = temp_dir.join("recording_fixture.ts");
    std::fs::copy(&fixture, &source).expect("fixture should copy");

    write_conversion_state(&source, RecordingConversionStatus::Converting, None).await;
    remux_recording_to_mp4(
        "recording-fixture".to_string(),
        source.clone(),
        settings,
        2,
        None,
    )
    .await;

    let mp4_path = build_mp4_path(&source);
    let state_path = build_conversion_state_path(&source);

    (temp_dir, source, mp4_path, state_path)
}

#[tokio::test]
async fn remux_recording_to_mp4_deletes_source_ts_when_retention_disabled() {
    let (temp_dir, source, mp4_path, state_path) = remux_recording_fixture(RecordingSettings {
        retain_source_ts: false,
    })
    .await;

    assert!(
        !source.exists(),
        "source TS should be deleted after successful remux by default"
    );
    assert!(mp4_path.exists(), "remux should create an MP4 sibling");
    assert!(
        !state_path.exists(),
        "conversion state should be removed once source retention is disabled"
    );

    let roundtrip_ts = temp_dir.join("roundtrip.ts");
    let status = TokioCommand::new(crate::ffmpeg_extract::ffmpeg_bin_path())
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &mp4_path.display().to_string(),
            "-map",
            "0",
            "-c",
            "copy",
            "-f",
            "mpegts",
            &roundtrip_ts.display().to_string(),
        ])
        .status()
        .await
        .expect("bundled ffmpeg should validate remuxed mp4");
    assert!(
        status.success(),
        "remuxed mp4 should be readable by bundled ffmpeg"
    );
    assert!(
        roundtrip_ts.exists() && std::fs::metadata(&roundtrip_ts).unwrap().len() > 0,
        "round-trip remux should produce TS output"
    );

    let _ = std::fs::remove_file(roundtrip_ts);
    let _ = std::fs::remove_file(mp4_path);
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn remux_recording_to_mp4_keeps_source_ts_when_retention_enabled() {
    let (temp_dir, source, mp4_path, state_path) = remux_recording_fixture(RecordingSettings {
        retain_source_ts: true,
    })
    .await;

    assert!(
        source.exists(),
        "source TS should remain after successful remux when retention is enabled"
    );
    assert!(mp4_path.exists(), "remux should create an MP4 sibling");

    let state = load_conversion_state(&source).expect("conversion state should exist");
    assert_eq!(state.status, RecordingConversionStatus::Ready);
    assert!(
        state.error.is_none(),
        "successful remux should not persist an error"
    );

    let roundtrip_ts = temp_dir.join("roundtrip.ts");
    let status = TokioCommand::new(crate::ffmpeg_extract::ffmpeg_bin_path())
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &mp4_path.display().to_string(),
            "-map",
            "0",
            "-c",
            "copy",
            "-f",
            "mpegts",
            &roundtrip_ts.display().to_string(),
        ])
        .status()
        .await
        .expect("bundled ffmpeg should validate remuxed mp4");
    assert!(
        status.success(),
        "remuxed mp4 should be readable by bundled ffmpeg"
    );
    assert!(
        roundtrip_ts.exists() && std::fs::metadata(&roundtrip_ts).unwrap().len() > 0,
        "round-trip remux should produce TS output"
    );

    let _ = std::fs::remove_file(roundtrip_ts);
    let _ = std::fs::remove_file(mp4_path);
    let _ = std::fs::remove_file(state_path);
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_dir_all(temp_dir);
}

fn demux_ts_file(path: &Path) -> Vec<MediaPacket> {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|e| panic!("failed to read TS file {}: {e}", path.display()));
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&bytes);
    demuxer.flush();
    demuxer.drain()
}

/// Asserts DTS is monotonically non-decreasing within each stream and that
/// no gap between consecutive packets exceeds 1s (which would indicate a
/// dropped GOP or dropped audio frames introduced by the remux).
fn assert_continuous_monotonic_dts(packets: &[MediaPacket], label: &str) {
    let mut last_video_dts: Option<i64> = None;
    let mut last_audio_dts: Option<i64> = None;
    let mut video_count = 0usize;
    let mut audio_count = 0usize;

    for packet in packets {
        let (last_dts, count) = match packet.media_type {
            MediaType::Video => (&mut last_video_dts, &mut video_count),
            MediaType::Audio => (&mut last_audio_dts, &mut audio_count),
        };
        *count += 1;
        if let Some(previous) = *last_dts {
            let gap = packet.dts - previous;
            assert!(
                gap >= 0,
                "{label}: {:?} DTS must be non-decreasing: {previous} -> {}",
                packet.media_type,
                packet.dts
            );
            assert!(
                gap < 1000,
                "{label}: {:?} DTS gap {gap}ms between {previous} and {} is too large, \
                 suggests dropped frames across the remux",
                packet.media_type,
                packet.dts
            );
        }
        *last_dts = Some(packet.dts);
    }

    assert!(video_count > 1, "{label}: expected multiple video packets");
    assert!(audio_count > 1, "{label}: expected multiple audio packets");
}

fn stream_span_ms(packets: &[MediaPacket], media_type: MediaType) -> i64 {
    let mut min = i64::MAX;
    let mut max = i64::MIN;
    for packet in packets.iter().filter(|p| p.media_type == media_type) {
        min = min.min(packet.dts);
        max = max.max(packet.dts);
    }
    assert!(
        min <= max,
        "expected at least one packet for {media_type:?}"
    );
    max - min
}

/// Proof: TS -> MP4 -> TS remux preserves DTS monotonicity and timeline
/// span for both video and audio streams, regardless of whether the
/// source TS is retained or deleted after the remux completes.
async fn assert_remux_preserves_timestamp_continuity(retain_source_ts: bool) {
    let (temp_dir, source, mp4_path, state_path) =
        remux_recording_fixture(RecordingSettings { retain_source_ts }).await;

    let fixture = crate::test_fixtures::canonical_h264_ts_fixture()
        .expect("checked-in TS fixture should exist");
    let source_packets = demux_ts_file(&fixture);
    assert_continuous_monotonic_dts(&source_packets, "source TS");
    let source_video_span = stream_span_ms(&source_packets, MediaType::Video);
    let source_audio_span = stream_span_ms(&source_packets, MediaType::Audio);

    let roundtrip_ts = temp_dir.join("roundtrip.ts");
    let status = TokioCommand::new(crate::ffmpeg_extract::ffmpeg_bin_path())
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &mp4_path.display().to_string(),
            "-map",
            "0",
            "-c",
            "copy",
            "-f",
            "mpegts",
            &roundtrip_ts.display().to_string(),
        ])
        .status()
        .await
        .expect("bundled ffmpeg should remux mp4 back to ts");
    assert!(status.success(), "mp4 -> ts round trip should succeed");

    let roundtrip_packets = demux_ts_file(&roundtrip_ts);
    assert_continuous_monotonic_dts(&roundtrip_packets, "TS -> MP4 -> TS roundtrip");

    let roundtrip_video_span = stream_span_ms(&roundtrip_packets, MediaType::Video);
    let roundtrip_audio_span = stream_span_ms(&roundtrip_packets, MediaType::Audio);

    assert!(
        (source_video_span - roundtrip_video_span).abs() <= 40,
        "video timeline span should survive TS -> MP4 -> TS within rounding: \
         source={source_video_span}ms roundtrip={roundtrip_video_span}ms"
    );
    assert!(
        (source_audio_span - roundtrip_audio_span).abs() <= 40,
        "audio timeline span should survive TS -> MP4 -> TS within rounding: \
         source={source_audio_span}ms roundtrip={roundtrip_audio_span}ms"
    );

    let _ = std::fs::remove_file(roundtrip_ts);
    let _ = std::fs::remove_file(mp4_path);
    let _ = std::fs::remove_file(state_path);
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_disabled() {
    assert_remux_preserves_timestamp_continuity(false).await;
}

#[tokio::test]
async fn remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_enabled() {
    assert_remux_preserves_timestamp_continuity(true).await;
}

#[test]
fn recording_service_metadata_describes_pipeline_source_and_time() {
    let metadata = build_recording_service_metadata(
        "Main Program",
        "pipe_123",
        Some("file:clip.mp4"),
        "2026-06-27T12:34:56Z",
    );
    assert_eq!(metadata.provider_name, "Restream pipeline_id=pipe_123");
    assert!(metadata.service_name.contains("pipeline=Main Program"));
    assert!(metadata.service_name.contains("source=file:clip.mp4"));
    assert!(
        metadata
            .service_name
            .contains("recorded_at=2026-06-27T12:34:56Z")
    );

    let publisher =
        build_recording_service_metadata("Live", "pipe_live", None, "2026-06-27T12:34:56Z");
    assert!(publisher.service_name.contains("source=publisher"));
}

#[test]
fn ts_writer_writes_data_to_file() {
    let queue = Arc::new(MemoryQueue::new());
    let token = CancellationToken::new();
    let temp = std::env::temp_dir().join("test_write.ts");
    let path = temp.to_string_lossy().to_string();
    queue.write_sync(b"hello world");
    queue.close();
    let res = run_ts_writer(queue, &path, token);
    assert!(res.is_ok());
    let content = std::fs::read(&temp).unwrap();
    assert_eq!(content, b"hello world");
    let _ = std::fs::remove_file(&temp);
}

#[test]
fn ts_writer_empty_closed_queue_creates_empty_file() {
    let queue = Arc::new(MemoryQueue::new());
    queue.close();
    let token = CancellationToken::new();
    let temp = std::env::temp_dir().join("test_empty.ts");
    let path = temp.to_string_lossy().to_string();
    assert!(run_ts_writer(queue, &path, token).is_ok());
    assert_eq!(std::fs::read(&temp).unwrap().len(), 0);
    let _ = std::fs::remove_file(&temp);
}

#[test]
fn ts_writer_fails_on_invalid_path() {
    let queue = Arc::new(MemoryQueue::new());
    let token = CancellationToken::new();
    assert!(run_ts_writer(queue, "/nonexistent_dir/should/fail.ts", token).is_err());
}

// H5: QueueCloseGuard must unblock the writer thread even if the queue is
// never explicitly closed by the caller (e.g., async fn cancelled/panicked).
// Simulate by dropping the guard and verifying the writer exits.
#[test]
fn sanitize_name_trims_leading_trailing_underscores() {
    assert_eq!(sanitize_name("///name///"), "name");
    assert_eq!(sanitize_name("   name   "), "name");
}

#[test]
fn sanitize_name_all_special_chars_becomes_empty_or_underscore() {
    // All slashes collapse to a single underscore, then get trimmed
    let result = sanitize_name("///");
    assert!(result.is_empty() || result == "_");
}

#[test]
fn build_recording_service_metadata_uses_publisher_when_source_empty() {
    let meta = build_recording_service_metadata("Test", "pid", Some(""), "2026-06-27");
    assert!(meta.service_name.contains("source=publisher"));
}

#[test]
fn build_recording_service_metadata_trims_whitespace_from_source() {
    let meta = build_recording_service_metadata("Test", "pid", Some("  "), "2026-06-27");
    assert!(meta.service_name.contains("source=publisher"));
}

#[test]
fn ts_writer_drains_data_written_before_close() {
    let queue = Arc::new(MemoryQueue::new());
    let token = CancellationToken::new();
    let temp = std::env::temp_dir().join("test_drain.ts");
    let path = temp.to_string_lossy().to_string();

    // Write multiple chunks then close
    queue.write_sync(b"chunk-one-");
    queue.write_sync(b"chunk-two");
    queue.close();

    let res = run_ts_writer(queue, &path, token);
    assert!(res.is_ok());
    let content = std::fs::read(&temp).unwrap();
    assert_eq!(content, b"chunk-one-chunk-two");
    let _ = std::fs::remove_file(&temp);
}

#[test]
fn queue_close_guard_unblocks_writer_thread() {
    let queue = Arc::new(MemoryQueue::new());

    // Start the writer thread on an open queue.
    let queue_for_thread = queue.clone();
    let temp_dir = std::env::temp_dir();
    let file_path = temp_dir.join("test_guard_recording.ts");
    let path_str = file_path.to_string_lossy().to_string();
    let token = CancellationToken::new();
    let thread = std::thread::spawn(move || run_ts_writer(queue_for_thread, &path_str, token));

    // Simulate the guard drop (async fn drop) by closing the queue directly.
    // In production this is done by QueueCloseGuard::drop.
    queue.close();

    // Writer thread must exit within 1 second — no hang.
    let result = thread.join().expect("writer thread panicked");
    assert!(result.is_ok());
    let _ = std::fs::remove_file(temp_dir.join("test_guard_recording.ts"));
}

fn drain_test_packet() -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0; 4]),
    }
}

#[allow(clippy::too_many_arguments)]
async fn drain_ready_bursts_test_call(
    reader: &mut Reader,
    cancel_token: &CancellationToken,
    engine: &MediaEngine,
) -> usize {
    let mut packets: Vec<Arc<MediaPacket>> = Vec::new();
    let mut feeder: Option<TsPacketFeeder> = None;
    let video_sequence_header: Option<bytes::Bytes> = None;
    let service_metadata = TsServiceMetadata {
        provider_name: "test".to_string(),
        service_name: "test".to_string(),
    };
    let mut ts_batch: Vec<u8> = Vec::new();
    let queue = Arc::new(MemoryQueue::new());
    let stage_metrics = Arc::new(StageMetrics::new());

    drain_ready_bursts(
        reader,
        cancel_token,
        &mut packets,
        MEDIA_PULL_BURST_PACKETS,
        engine,
        "test-pipeline",
        &mut feeder,
        &video_sequence_header,
        &service_metadata,
        &mut ts_batch,
        &queue,
        &stage_metrics,
    )
    .await
}

// Regression proof for the CI flake fixed alongside this test: a recorder
// task descheduled while the ring accumulated a backlog (CI CPU contention)
// used to drain the *entire* backlog on resume even after a stop had already
// been requested, extending recordings well past the requested stop point.
// The drain loop now checks cancellation between bursts, so this must stop
// after at most one in-flight burst.
#[tokio::test]
async fn drain_ready_bursts_stops_within_one_burst_after_cancellation() {
    let ring = Arc::new(RingBuffer::new(512));
    let mut reader = Reader::new("test-drain-cancel".to_string(), ring.clone());

    let backlog = 10 * MEDIA_PULL_BURST_PACKETS;
    for _ in 0..backlog {
        ring.push(drain_test_packet());
    }

    let cancel_token = CancellationToken::new();
    cancel_token.cancel();
    let engine = Arc::new(MediaEngine::new());

    let drained = drain_ready_bursts_test_call(&mut reader, &cancel_token, &engine).await;

    assert!(
        drained <= MEDIA_PULL_BURST_PACKETS,
        "cancelled drain consumed {drained} packets from a {backlog}-packet backlog, \
         expected at most one burst ({MEDIA_PULL_BURST_PACKETS})"
    );
}

// Regression guard for the opposite failure mode: without cancellation,
// draining must still consume the entire available backlog so normal
// recording doesn't lose data or stop prematurely.
#[tokio::test]
async fn drain_ready_bursts_drains_full_backlog_when_not_cancelled() {
    let ring = Arc::new(RingBuffer::new(512));
    let mut reader = Reader::new("test-drain-full".to_string(), ring.clone());

    let backlog = 10 * MEDIA_PULL_BURST_PACKETS;
    for _ in 0..backlog {
        ring.push(drain_test_packet());
    }

    let cancel_token = CancellationToken::new();
    let engine = Arc::new(MediaEngine::new());

    let drained = drain_ready_bursts_test_call(&mut reader, &cancel_token, &engine).await;

    assert_eq!(drained, backlog);
}
