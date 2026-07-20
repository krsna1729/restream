use super::maybe_publish_probe;
use super::parse_start_time_ms;
use super::spawn_internal_file_ingest;
use super::startup::{
    prime_container_metadata as prime_input_container_metadata,
    prime_video_from_packet as prime_input_video_startup_state_from_packet,
};
use super::{ContinuousTimestampState, LoopStartupGate, LoopTimestampState};
use crate::media::engine::MediaEngine;
use crate::media::mpegts::TsDemuxer;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::RingBuffer;
use bytes::Bytes;
use ffmpeg_next::format;
use std::sync::Arc;
use tokio::time::{Duration, sleep};

#[test]
fn internal_file_ingest_flag_uses_typed_config() {
    let disabled = crate::AppConfig {
        use_internal_file_ingest: false,
        ..crate::AppConfig::default()
    };
    let enabled = crate::AppConfig {
        use_internal_file_ingest: true,
        ..crate::AppConfig::default()
    };

    assert!(!super::use_internal_file_ingest(&disabled));
    assert!(super::use_internal_file_ingest(&enabled));
}

#[test]
fn empty_start_time_is_none() {
    assert_eq!(parse_start_time_ms("").unwrap(), None);
    assert_eq!(parse_start_time_ms("   ").unwrap(), None);
}

#[test]
fn parses_seconds_start_time() {
    assert_eq!(parse_start_time_ms("5").unwrap(), Some(5_000));
    assert_eq!(parse_start_time_ms("1.25").unwrap(), Some(1_250));
}

#[test]
fn parses_colon_delimited_start_time() {
    assert_eq!(parse_start_time_ms("00:00:05").unwrap(), Some(5_000));
    assert_eq!(parse_start_time_ms("01:02:03.5").unwrap(), Some(3_723_500));
    assert_eq!(parse_start_time_ms("02:03.25").unwrap(), Some(123_250));
}

#[test]
fn rejects_invalid_start_time() {
    assert!(parse_start_time_ms("-1").is_err());
    assert!(parse_start_time_ms("1:two").is_err());
    assert!(parse_start_time_ms("1:2:3:4").is_err());
}

#[test]
fn rejects_non_finite_plain_seconds() {
    assert!(parse_start_time_ms("NaN").is_err());
    assert!(parse_start_time_ms("nan").is_err());
    assert!(parse_start_time_ms("inf").is_err());
    assert!(parse_start_time_ms("infinity").is_err());
    assert!(parse_start_time_ms("-inf").is_err());
}

#[test]
fn rejects_non_finite_colon_delimited_seconds_component() {
    assert!(parse_start_time_ms("00:nan").is_err());
    assert!(parse_start_time_ms("00:00:inf").is_err());
}

#[test]
fn rejects_float_to_millisecond_overflow() {
    assert!(parse_start_time_ms("1e30").is_err());
    assert!(parse_start_time_ms("00:00:1e30").is_err());
}

#[test]
fn rejects_colon_delimited_integer_overflow() {
    // Individually parseable i64 components whose hours*3600 or
    // minutes*60 scaling overflows i64 before any float arithmetic runs.
    assert!(parse_start_time_ms("9223372036854775807:00:00").is_err());
    assert!(parse_start_time_ms("00:9223372036854775807:00").is_err());
    assert!(parse_start_time_ms("9223372036854775807:9223372036854775807:00").is_err());
}

proptest::proptest! {
    #[test]
    fn parse_start_time_ms_never_panics_on_arbitrary_input(s in ".{0,64}") {
        let _ = parse_start_time_ms(&s);
    }

    #[test]
    fn parse_start_time_ms_plain_seconds_matches_seconds_to_ms(seconds in 0.0f64..1_000_000.0) {
        // f64's Display/FromStr round-trip exactly, so parsing the
        // printed value must agree with feeding `seconds` straight in.
        let expected = super::seconds_to_ms(seconds);
        let actual = parse_start_time_ms(&seconds.to_string());
        proptest::prop_assert_eq!(actual, expected);
    }

    #[test]
    fn parse_start_time_ms_rejects_negative_plain_seconds(seconds in -1_000_000.0f64..-0.0001) {
        proptest::prop_assert!(parse_start_time_ms(&seconds.to_string()).is_err());
    }

    #[test]
    fn parse_start_time_ms_colon_delimited_matches_total_seconds(
        hours in 0i64..1000,
        minutes in 0i64..60,
        whole_seconds in 0i64..60,
        millis in 0i64..1000,
    ) {
        let input = format!("{hours:02}:{minutes:02}:{whole_seconds:02}.{millis:03}");
        let total_seconds =
            (hours * 3600 + minutes * 60 + whole_seconds) as f64 + (millis as f64) / 1000.0;
        let expected = super::seconds_to_ms(total_seconds);
        let actual = parse_start_time_ms(&input);
        proptest::prop_assert_eq!(actual, expected);
    }
}

#[test]
fn pace_packet_does_not_sleep_for_timestamps_behind_the_anchor() {
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut anchor = None;
    super::pace_packet(&cancel, &mut anchor, 1_433);

    let start = std::time::Instant::now();
    super::pace_packet(&cancel, &mut anchor, 1_400);
    assert!(
        start.elapsed() < std::time::Duration::from_millis(200),
        "packet behind the pace anchor must pass through without sleeping"
    );
}

#[test]
fn pace_packet_ignores_negative_timestamps_without_arming_the_anchor() {
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut anchor = None;
    super::pace_packet(&cancel, &mut anchor, -1);
    assert!(
        anchor.is_none(),
        "a negative timestamp must not arm the pacing anchor"
    );

    super::pace_packet(&cancel, &mut anchor, 1_000);
    assert!(
        anchor.is_some(),
        "the next non-negative packet must still arm the anchor normally"
    );
}

#[test]
fn pace_packet_sleeps_toward_the_target_then_cancellation_cuts_it_short() {
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut anchor = None;
    super::pace_packet(&cancel, &mut anchor, 0);

    let cancel_for_thread = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        cancel_for_thread.cancel();
    });

    let start = std::time::Instant::now();
    super::pace_packet(&cancel, &mut anchor, 2_000);
    let elapsed = start.elapsed();
    canceller.join().unwrap();

    assert!(
        elapsed < std::time::Duration::from_millis(2_000),
        "cancellation must cut a long pacing sleep short, elapsed={elapsed:?}"
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(40),
        "cancellation should not fire before the sleep even begins, elapsed={elapsed:?}"
    );
}

#[test]
fn loop_timestamp_state_keeps_replayed_packets_monotonic() {
    let mut timestamps = LoopTimestampState::default();

    timestamps.begin_pass();
    let mut first = test_packet(MediaType::Video, 0, 0, 0);
    let mut second = test_packet(MediaType::Video, 0, 33, 33);
    timestamps.apply(&mut first);
    timestamps.apply(&mut second);
    timestamps.finish_pass();

    assert_eq!(first.pts, 0);
    assert_eq!(second.pts, 33);
    assert_eq!(timestamps.pass_packet_count(), 2);

    timestamps.begin_pass();
    let mut looped_first = test_packet(MediaType::Video, 0, 0, 0);
    let mut looped_second = test_packet(MediaType::Video, 0, 33, 33);
    timestamps.apply(&mut looped_first);
    timestamps.apply(&mut looped_second);
    timestamps.finish_pass();

    assert_eq!(looped_first.pts, 34);
    assert_eq!(looped_first.dts, 34);
    assert_eq!(looped_second.pts, 67);
    assert_eq!(looped_second.dts, 67);
    assert_eq!(timestamps.pass_packet_count(), 2);
}

#[test]
fn loop_timestamp_state_reports_empty_passes() {
    let mut timestamps = LoopTimestampState::default();
    timestamps.begin_pass();
    timestamps.finish_pass();

    assert_eq!(timestamps.pass_packet_count(), 0);
}

#[test]
fn loop_timestamp_state_normalizes_nonzero_file_offsets() {
    let mut timestamps = LoopTimestampState::default();

    timestamps.begin_pass();
    let mut first = test_packet(MediaType::Video, 0, 1_467, 1_400);
    let mut second = test_packet(MediaType::Audio, 0, 1_445, 1_445);
    let mut third = test_packet(MediaType::Video, 0, 1_500, 1_433);
    timestamps.apply(&mut first);
    timestamps.apply(&mut second);
    timestamps.apply(&mut third);
    timestamps.finish_pass();

    assert_eq!(first.pts, 67);
    assert_eq!(first.dts, 0);
    assert_eq!(second.pts, 45);
    assert_eq!(second.dts, 45);
    assert_eq!(third.pts, 100);
    assert_eq!(third.dts, 33);

    timestamps.begin_pass();
    let mut replayed = test_packet(MediaType::Video, 0, 1_467, 1_400);
    timestamps.apply(&mut replayed);
    timestamps.finish_pass();

    assert_eq!(replayed.dts, 101);
}

#[test]
fn continuous_timestamp_state_offsets_replayed_subprocess_packets() {
    let mut timestamps = ContinuousTimestampState::default();

    let mut first = test_packet(MediaType::Video, 0, 0, 0);
    let mut second = test_packet(MediaType::Video, 0, 40, 40);
    timestamps.apply(&mut first);
    timestamps.apply(&mut second);

    let mut replayed_first = test_packet(MediaType::Video, 0, 0, 0);
    let mut replayed_second = test_packet(MediaType::Video, 0, 40, 40);
    timestamps.apply(&mut replayed_first);
    timestamps.apply(&mut replayed_second);

    assert_eq!(first.pts, 0);
    assert_eq!(second.pts, 40);
    assert_eq!(replayed_first.pts, 41);
    assert_eq!(replayed_first.dts, 41);
    assert_eq!(replayed_second.pts, 81);
    assert_eq!(replayed_second.dts, 81);
}

#[test]
fn continuous_timestamp_state_preserves_interleaved_audio_video_timestamps() {
    let mut timestamps = ContinuousTimestampState::default();

    let mut video0 = test_packet(MediaType::Video, 0, 0, 0);
    let mut audio0 = test_packet(MediaType::Audio, 0, 0, 0);
    let mut audio1 = test_packet(MediaType::Audio, 0, 21, 21);
    let mut video1 = test_packet(MediaType::Video, 0, 33, 33);

    timestamps.apply(&mut video0);
    timestamps.apply(&mut audio0);
    timestamps.apply(&mut audio1);
    timestamps.apply(&mut video1);

    assert_eq!(video0.pts, 0);
    assert_eq!(audio0.pts, 0);
    assert_eq!(audio1.pts, 21);
    assert_eq!(video1.pts, 33);
}

#[test]
fn continuous_timestamp_state_uses_dts_for_reordered_video_packets() {
    let mut timestamps = ContinuousTimestampState::default();

    let mut anchor = test_packet(MediaType::Video, 0, 0, 0);
    let mut reordered_p = test_packet(MediaType::Video, 0, 100, 33);
    let mut reordered_b = test_packet(MediaType::Video, 0, 66, 66);

    timestamps.apply(&mut anchor);
    timestamps.apply(&mut reordered_p);
    timestamps.apply(&mut reordered_b);

    assert_eq!(anchor.pts, 0);
    assert_eq!(reordered_p.pts, 100);
    assert_eq!(reordered_p.dts, 33);
    assert_eq!(reordered_b.pts, 66);
    assert_eq!(reordered_b.dts, 66);
}

#[test]
fn loop_startup_gate_waits_for_keyframe_before_releasing_packets() {
    let ring = Arc::new(RingBuffer::new(64));
    let registration = crate::media::engine::IngestRegistration {
        cancel_token: tokio_util::sync::CancellationToken::new(),
        attempt_id: 1,
        input_id: "input".to_string(),
        gate: Arc::new(crate::media::input_gate::InputPacketGate::active()),
        last_forwarded_dts: Arc::new(std::sync::atomic::AtomicI64::new(i64::MIN)),
        preview_ring: Arc::new(arc_swap::ArcSwapOption::empty()),
    };
    let mut gate = LoopStartupGate::new(true);
    let delta_video = MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0xDD]),
    };
    let mut keyframe_video = test_packet(MediaType::Video, 0, 33, 33);
    keyframe_video.is_keyframe = true;
    let audio = test_packet(MediaType::Audio, 0, 10, 10);

    assert!(
        !gate.filter_packet(&audio, &ring, &registration),
        "audio must stay gated until the loop reaches a clean video boundary"
    );
    assert!(
        !gate.filter_packet(&delta_video, &ring, &registration),
        "delta video must not start a fresh file-ingest loop"
    );
    assert!(gate.filter_packet(&keyframe_video, &ring, &registration));
    assert!(
        gate.filter_packet(&audio, &ring, &registration),
        "once a loop starts on a keyframe, audio may flow again"
    );
}

#[test]
fn loop_startup_gate_without_video_never_gates_packets() {
    let ring = Arc::new(RingBuffer::new(64));
    let registration = crate::media::engine::IngestRegistration {
        cancel_token: tokio_util::sync::CancellationToken::new(),
        attempt_id: 1,
        input_id: "input".to_string(),
        gate: Arc::new(crate::media::input_gate::InputPacketGate::active()),
        last_forwarded_dts: Arc::new(std::sync::atomic::AtomicI64::new(i64::MIN)),
        preview_ring: Arc::new(arc_swap::ArcSwapOption::empty()),
    };
    let mut gate = LoopStartupGate::new(false);
    let audio = test_packet(MediaType::Audio, 0, 10, 10);
    let delta_video = test_packet(MediaType::Video, 0, 0, 0);

    assert!(
        gate.filter_packet(&audio, &ring, &registration),
        "audio-only ingest must never wait on a video keyframe that will never arrive"
    );
    assert!(
        gate.filter_packet(&delta_video, &ring, &registration),
        "a stream with no video startup gate must pass delta frames through immediately"
    );
}

fn test_packet(media_type: MediaType, track_index: u32, pts: i64, dts: i64) -> MediaPacket {
    MediaPacket {
        media_type,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index,
        pts,
        dts,
        payload: Bytes::from_static(b"packet"),
    }
}

#[tokio::test]
async fn internal_file_ingest_pushes_packets_and_stays_registered() {
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "pipe-file-ingest-test";
    let ingest_id = "ing-file-ingest-test";
    let stream_key = "file-ingest-test-key";
    let ring_buffer = engine.get_or_create_pipeline(pipeline_id).await;
    let registration = engine
        .try_register_ingest_attempt(pipeline_id, stream_key, "file")
        .await
        .expect("register ingest");

    engine.mark_file_ingest_running(ingest_id).await;
    spawn_internal_file_ingest(
        engine.clone(),
        tokio::runtime::Handle::current(),
        ingest_id.to_string(),
        pipeline_id.to_string(),
        crate::test_fixtures::canonical_h264_ts_fixture().expect("checked-in transport fixture"),
        String::new(),
        false,
        ring_buffer.clone(),
        registration.clone(),
    )
    .expect("spawn internal ingest");

    sleep(Duration::from_secs(2)).await;

    assert!(
        engine.ingests.active.read().await.contains_key(pipeline_id),
        "internal ingest should still be registered while streaming"
    );
    assert!(
        ring_buffer.get_write_idx() > 0,
        "internal ingest should have produced media packets after startup"
    );

    registration.cancel_token.cancel();
    sleep(Duration::from_millis(250)).await;
}

#[tokio::test]
async fn internal_bf0_file_ingest_caches_video_startup_state() {
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "pipe-file-ingest-bf0-state";
    let ingest_id = "ing-file-ingest-bf0-state";
    let stream_key = "file-ingest-bf0-state-key";
    let ring_buffer = engine.get_or_create_pipeline(pipeline_id).await;
    let registration = engine
        .try_register_ingest_attempt(pipeline_id, stream_key, "file")
        .await
        .expect("register ingest");

    engine.mark_file_ingest_running(ingest_id).await;
    spawn_internal_file_ingest(
        engine.clone(),
        tokio::runtime::Handle::current(),
        ingest_id.to_string(),
        pipeline_id.to_string(),
        crate::test_fixtures::av_marker_transport_fixture_for_bframes(
            "h264",
            false,
            crate::test_fixtures::AvMarkerBframeMode::Bf0,
        )
        .expect("checked-in bf0 transport fixture"),
        String::new(),
        false,
        ring_buffer.clone(),
        registration.clone(),
    )
    .expect("spawn internal ingest");

    sleep(Duration::from_secs(2)).await;

    let (cached_video, _) = engine.get_sequence_headers(pipeline_id).await;
    let ring_parameter_sets = ring_buffer.video_parameter_sets();
    let ring_sequence_header = ring_parameter_sets
        .as_deref()
        .and_then(crate::media::codec::build_avcc_sequence_header);
    assert!(
        cached_video.is_some(),
        "internal BF0 file ingest should cache a startup video sequence header (ring parameter sets present: {}, ring startup header present: {})",
        ring_parameter_sets.is_some(),
        ring_sequence_header.is_some(),
    );

    registration.cancel_token.cancel();
    sleep(Duration::from_millis(250)).await;
}

#[test]
fn prime_input_video_startup_state_from_packet_sets_ring_parameter_sets_for_hevc() {
    // HEVC has no AVCC/FLV sequence header (build_avcc_sequence_header
    // only understands H.264 NALs, so it returns None for HEVC VPS/SPS/
    // PPS), but wait_for_stage_metadata's eager-parameter-sets gate for
    // VideoPreset transcoder stages only needs
    // `ring_buffer.video_parameter_sets()` — that must still get set from
    // the first video packet even when no AVCC header can be built.
    let runtime = tokio::runtime::Runtime::new().expect("create runtime");
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "pipe-file-hevc-paramsets-direct";
    let registration = runtime
        .block_on(engine.try_register_ingest_attempt(pipeline_id, "hevc-key", "file"))
        .expect("register ingest");
    let ring_buffer = runtime.block_on(engine.get_or_create_pipeline(pipeline_id));

    let video_payload = [
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB, 0x00,
        0x00, 0x00, 0x01, 0x44, 0x01, 0xCC, 0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xDD,
    ];

    assert!(
        crate::media::codec::build_avcc_sequence_header(
            &crate::media::codec::annexb_parameter_sets(&video_payload)
                .expect("hevc payload should carry VPS/SPS/PPS")
        )
        .is_none(),
        "sanity check: HEVC parameter sets must not build an AVCC header"
    );

    let primed = prime_input_video_startup_state_from_packet(
        &engine,
        runtime.handle(),
        &ring_buffer,
        &registration,
        &video_payload,
    );

    assert!(primed, "priming should report success for HEVC packets");
    assert!(
        ring_buffer.video_parameter_sets().is_some(),
        "HEVC video packet should prime ring buffer parameter sets even without an AVCC header"
    );
}

#[test]
fn prime_input_container_metadata_populates_ingest_before_any_packet_read() {
    let runtime = tokio::runtime::Runtime::new().expect("create runtime");
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "pipe-file-ingest-eager-meta";
    let registration = runtime
        .block_on(engine.try_register_ingest_attempt(pipeline_id, "eager-meta-key", "file"))
        .expect("register ingest");

    let fixture = crate::test_fixtures::av_marker_transport_fixture("h265", true)
        .expect("checked-in 2-audio-track transport fixture");
    let ictx = format::input(&fixture).expect("open fixture container");

    // No packets have been read or paced yet: this proves metadata comes
    // from container stream headers, not from the packet-paced probe.
    prime_input_container_metadata(&engine, runtime.handle(), pipeline_id, &registration, &ictx);

    runtime.block_on(async {
        let ingests = engine.ingests.active.read().await;
        let ingest = ingests.get(pipeline_id).expect("ingest registered");
        let metadata = ingest.metadata();
        let video = metadata.video.as_ref().expect("video meta primed eagerly");
        assert_eq!(video.codec, "hevc");
        assert!(video.width > 0 && video.height > 0);

        let audio_tracks = ingest
            .audio_tracks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(audio_tracks.len(), 2, "both audio tracks should be primed");
        for track in audio_tracks.iter() {
            assert!(track.sample_rate > 0 && track.channels > 0);
        }
    });
}

#[test]
fn maybe_publish_probe_caches_h264_sequence_header() {
    let runtime = tokio::runtime::Runtime::new().expect("create runtime");
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "pipe-file-probe-seqhdr";
    let registration = runtime
        .block_on(engine.try_register_ingest_attempt(pipeline_id, "stream-key", "file"))
        .expect("register ingest");

    let mut demuxer = TsDemuxer::new();
    let fixture =
        crate::test_fixtures::canonical_h264_ts_fixture().expect("checked-in transport fixture");
    let fixture_bytes = std::fs::read(fixture).expect("read checked-in transport fixture");
    demuxer.feed(&fixture_bytes);
    demuxer.flush();

    let mut probe_sent = false;
    maybe_publish_probe(
        &engine,
        runtime.handle(),
        pipeline_id,
        &registration,
        &mut demuxer,
        &mut probe_sent,
    );

    let (cached_video, _) = runtime.block_on(engine.get_sequence_headers(pipeline_id));
    let cached_video = cached_video.expect("probe should cache an H.264 startup header");
    assert_eq!(cached_video[0], 0x17);
    assert_eq!(cached_video[1], 0x00);
}
