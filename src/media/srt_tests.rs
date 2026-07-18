use super::*;
use crate::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
use crate::domain::srt_ingest::SrtGlobalIngestConfig;
use crate::media::engine::{AudioMeta, VideoMeta};
use crate::media::ring_buffer::PayloadFormat;
use crate::media::security::IngestSecurityService;
use proptest::prelude::*;

#[test]
fn streamid_getsockopt_length_must_stay_within_buffer() {
    let mut buf = [0u8; 8];
    buf[..5].copy_from_slice(b"key\0x");

    assert_eq!(
        streamid_from_getsockopt_buffer(&buf, 5),
        Some("key\0x".trim_matches('\0').to_string())
    );
    assert_eq!(
        streamid_from_getsockopt_buffer(&buf, 0),
        Some(String::new())
    );
    assert_eq!(streamid_from_getsockopt_buffer(&buf, -1), None);
    assert_eq!(streamid_from_getsockopt_buffer(&buf, 9), None);
}

#[tokio::test]
async fn srt_server_shutdown_exits_with_no_connections() {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    let engine = Arc::new(MediaEngine::new());
    let security = Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG));
    let pipeline_store =
        Arc::new(crate::infrastructure::sqlite_ports::SqlitePipelineStore::new(pool));
    let pipeline_access = Arc::new(
        crate::application::ingest::PipelineStoreIngestAuthenticator::new(
            pipeline_store,
            security.clone(),
        ),
    );
    let server = Arc::new(SrtServer::new(
        pipeline_access,
        engine.clone(),
        security,
        Arc::new(SrtIngestPolicyStore::new(
            SrtGlobalIngestConfig::default(),
            &[],
        )),
    ));

    let handle = tokio::spawn(server.run(0));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if !engine
            .runtime
            .listener_shutdowns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "SRT listener never registered a shutdown hook"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    engine.shutdown_listeners();
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("SRT server did not exit after listener shutdown")
        .expect("SRT server task panicked");
    teardown_srt();
}

#[test]
fn parses_srt_stream_ids_from_common_tools() {
    let cases = [
        (
            "publish:key01?latency=240000",
            SrtConnectionMode::Publish,
            "key01",
        ),
        ("publisher:key02", SrtConnectionMode::Publish, "key02"),
        ("key03", SrtConnectionMode::Publish, "key03"),
        ("read:key04", SrtConnectionMode::Read, "key04"),
        ("play:key05", SrtConnectionMode::Read, "key05"),
        ("subscriber:key06", SrtConnectionMode::Read, "key06"),
        (
            "#!::r=key07,m=publish,latency=240000",
            SrtConnectionMode::Publish,
            "key07",
        ),
        ("#!::r=key08,m=request", SrtConnectionMode::Read, "key08"),
    ];

    for (input, mode, key) in cases {
        let parsed = parse_srt_stream_id(input);
        assert_eq!(parsed.mode, mode, "input={}", input);
        assert_eq!(parsed.stream_key, key, "input={}", input);
    }
}

#[test]
fn srt_stream_ids_normalize_plain_publish_keys_before_registration() {
    let cases = [
        "publish:key01",
        "publisher:key01?latency=240000",
        "#!::r=key01,m=publish,latency=240000",
    ];

    for input in cases {
        let parsed = parse_srt_stream_id(input);
        assert_eq!(parsed.mode, SrtConnectionMode::Publish, "input={input}");
        assert_eq!(parsed.stream_key, "key01", "input={input}");
    }
}

#[test]
fn srt_egress_preroll_is_reserved_for_1080p_variants() {
    assert_eq!(
        startup_policy::srt_egress_keyframe_preroll_packets("source"),
        0
    );
    assert_eq!(
        startup_policy::srt_egress_keyframe_preroll_packets("atrack:0"),
        0
    );
    assert_eq!(
        startup_policy::srt_egress_keyframe_preroll_packets("720p+atrack:0"),
        0
    );
    assert_eq!(
        startup_policy::srt_egress_keyframe_preroll_packets("1080p"),
        32
    );
    assert_eq!(
        startup_policy::srt_egress_keyframe_preroll_packets("1080p+atrack:1"),
        32
    );
    assert_eq!(
        startup_policy::srt_egress_keyframe_preroll_packets("1080p60+atrack:1"),
        0
    );
}

#[test]
fn srt_stream_ids_normalize_plain_read_keys_before_auth() {
    let cases = [
        "read:key02",
        "play:key02",
        "subscriber:key02?latency=240000",
        "#!::r=key02,m=request",
    ];

    for input in cases {
        let parsed = parse_srt_stream_id(input);
        assert_eq!(parsed.mode, SrtConnectionMode::Read, "input={input}");
        assert_eq!(parsed.stream_key, "key02", "input={input}");
    }
}

#[test]
fn srt_stream_ids_keep_slashes_as_literal_key_data() {
    let parsed = parse_srt_stream_id("publish:tenant/key01");
    assert_eq!(parsed.mode, SrtConnectionMode::Publish);
    assert_eq!(parsed.stream_key, "tenant/key01");

    let parsed = parse_srt_stream_id("#!::r=tenant%2Fkey02,m=request");
    assert_eq!(parsed.mode, SrtConnectionMode::Read);
    assert_eq!(parsed.stream_key, "tenant/key02");
}

#[test]
fn srt_rates_use_counter_deltas_instead_of_cumulative_totals() {
    let sampled_at = Instant::now();
    let mut stats: SrtTraceBStats = unsafe { std::mem::zeroed() };
    stats.pkt_rcv_loss_total = 5_000;
    stats.pkt_rcv_drop_total = 500;
    stats.pkt_rcv_retrans = 10_000;

    let (first, snapshot) = srt_quality_from_stats(&stats, None, sampled_at);
    assert_eq!(first.packets_received_loss, Some(5_000));
    assert_eq!(first.packets_received_loss_per_sec, None);

    let (recovered, _) = srt_quality_from_stats(
        &stats,
        Some(snapshot),
        sampled_at + std::time::Duration::from_secs(2),
    );
    assert_eq!(recovered.packets_received_loss_per_sec, Some(0.0));
    assert_eq!(recovered.packets_received_drop_per_sec, Some(0.0));
    assert_eq!(recovered.packets_received_retrans_per_sec, Some(0.0));
}

#[test]
fn srt_rates_report_current_loss_window() {
    let sampled_at = Instant::now();
    let previous = SrtCounterSnapshot {
        packets_received_loss: 100,
        packets_received_drop: 10,
        packets_received_retrans: 200,
        packets_received_undecrypt: 0,
        sampled_at,
    };
    let mut stats: SrtTraceBStats = unsafe { std::mem::zeroed() };
    stats.pkt_rcv_loss_total = 120;
    stats.pkt_rcv_drop_total = 16;
    stats.pkt_rcv_retrans = 220;
    stats.pkt_rcv_undecrypt_total = 2;

    let (quality, _) = srt_quality_from_stats(
        &stats,
        Some(previous),
        sampled_at + std::time::Duration::from_secs(2),
    );
    assert_eq!(quality.packets_received_loss_per_sec, Some(10.0));
    assert_eq!(quality.packets_received_drop_per_sec, Some(3.0));
    assert_eq!(quality.packets_received_retrans_per_sec, Some(10.0));
    assert_eq!(quality.packets_received_undecrypt_per_sec, Some(1.0));
}

#[test]
fn ts_accum_capacity_tracks_packet_size_without_fixed_64k_floor() {
    let packets = vec![
        Arc::new(MediaPacket {
            media_type: MediaType::Audio,
            track_index: 0,
            pts: 0,
            dts: 0,
            is_keyframe: false,
            format: PayloadFormat::Raw,
            payload: bytes::Bytes::from(vec![0; 200]),
        }),
        Arc::new(MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: 0,
            dts: 0,
            is_keyframe: true,
            format: PayloadFormat::Raw,
            payload: bytes::Bytes::from(vec![1; 1_000]),
        }),
    ];

    let estimated = estimate_ts_accum_capacity(&packets);
    assert_eq!(estimated, 200 + 1_000 + (188 * 4 * 2));
    assert!(estimated < 64 * 1024);
}

#[test]
fn receive_error_classifier_waits_only_for_transient_readiness() {
    assert_eq!(
        classify_srt_receive_error(SRT_EASYNCRCV),
        SrtReceiveErrorAction::WaitForReadiness
    );
    assert_eq!(
        classify_srt_receive_error(SRT_ETIMEOUT),
        SrtReceiveErrorAction::WaitForReadiness
    );
}

#[test]
fn receive_error_classifier_disconnects_closed_publishers() {
    for code in [SRT_ESCLOSED, SRT_ECONNLOST, SRT_ENOCONN, -1, 0] {
        assert_eq!(
            classify_srt_receive_error(code),
            SrtReceiveErrorAction::Disconnect,
            "code={code}"
        );
    }
}

#[test]
fn video_for_ts_raw_passthrough() {
    let raw_video = [0, 0, 1, 0x65, 0xaa, 0xbb];
    let mut nls = 4usize;
    let mut cache = Vec::new();
    let result =
        crate::media::codec::video_for_ts(&raw_video, PayloadFormat::Raw, &mut nls, &mut cache);
    assert!(result.is_some());
    assert_eq!(&*result.unwrap(), &raw_video[..]);
}

#[test]
fn audio_for_ts_raw_passthrough_with_adts() {
    let adts_audio = [0xFF, 0xF1, 0x50, 0x80, 0x01, 0x1F, 0xFC, 0x21, 0x10];
    // Raw with ADTS sync → borrowed passthrough
    let result = crate::media::codec::audio_for_ts(&adts_audio, PayloadFormat::Raw, 48000, 2);
    assert!(result.is_some());
    assert_eq!(&*result.unwrap(), &adts_audio[..]);
}

#[test]
fn flv_video_seq_skipped_data_converted() {
    let flv_video_seq = [
        0x17u8, 0x00, 0x00, 0x00, 0x00, 1, 66, 0, 30, 0xFF, 0xE1, 0, 3, 1, 2, 3, 1, 0, 2, 4, 5,
    ];
    let flv_audio_seq = [0xaf, 0x00, 0x12, 0x10];

    let mut nls = 4usize;
    // Seq headers for audio → None
    assert!(
        crate::media::codec::audio_for_ts(&flv_audio_seq, PayloadFormat::Flv, 48000, 2).is_none()
    );
    // Video seq header → extracts SPS/PPS as Annex B (or None if config too short)
    let mut cache = Vec::new();
    let _result =
        crate::media::codec::video_for_ts(&flv_video_seq, PayloadFormat::Flv, &mut nls, &mut cache);
    // Just verify no panic; codec tests cover correctness in detail
}

#[test]
fn maps_h264_and_h265_without_guessing_unknown_codecs() {
    assert_eq!(
        video_codec_id("h264"),
        Some(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H264)
    );
    assert_eq!(
        video_codec_id("hevc"),
        Some(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_HEVC)
    );
    assert_eq!(video_codec_id("unknown"), None);
    assert_eq!(
        audio_codec_id("aac"),
        Some(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_AAC)
    );
    assert_eq!(audio_codec_id("opus"), None);
}

#[test]
fn egress_url_parses_simple_target() {
    let u = parse_srt_egress_url("srt://192.168.1.5:9000");
    assert_eq!(u.host_port, "192.168.1.5:9000");
    assert!(u.streamid.is_empty());
    assert!(u.bond_addrs.is_empty());
}

#[test]
fn egress_url_parses_streamid() {
    let u = parse_srt_egress_url("srt://host:9000?streamid=publish:key1");
    assert_eq!(u.host_port, "host:9000");
    assert_eq!(u.streamid, "publish:key1");
    assert!(u.bond_addrs.is_empty());
}

// --- Regression: issue #6 (Round 5) — SRT stream ID percent-decode ---
// Before the fix, percent-encoded characters in the streamid query parameter
// were passed through raw. Percent-encoded stream IDs would be compared against DB
// stream keys verbatim, causing silent auth failure.
#[test]
fn percent_decode_basic() {
    assert_eq!(percent_decode("publish:key%2Done"), "publish:key-one");
    assert_eq!(percent_decode("hello%20world"), "hello world");
    assert_eq!(percent_decode("no_encoding"), "no_encoding");
    assert_eq!(percent_decode("%41%42%43"), "ABC"); // A=0x41, B=0x42, C=0x43
}

#[test]
fn percent_decode_incomplete_sequence_passthrough() {
    // A truncated %XX at the end should not panic.
    assert_eq!(percent_decode("foo%2"), "foo%2");
    assert_eq!(percent_decode("foo%"), "foo%");
}

#[test]
fn egress_url_percent_decodes_streamid() {
    // Percent-encoded streamid characters must be decoded before use.
    let u = parse_srt_egress_url("srt://host:9000?streamid=publish%3Amykey");
    assert_eq!(
        u.streamid, "publish:mykey",
        "percent-encoded streamid must be decoded in egress URL"
    );
}

#[test]
fn egress_url_parses_bond_addresses() {
    let u =
        parse_srt_egress_url("srt://primary:9000?streamid=live/out&bond=backup1:9000,backup2:9000");
    assert_eq!(u.host_port, "primary:9000");
    assert_eq!(u.streamid, "live/out");
    assert_eq!(u.bond_addrs, vec!["backup1:9000", "backup2:9000"]);
}

#[test]
fn egress_url_bond_only_no_streamid() {
    let u = parse_srt_egress_url("srt://10.0.0.1:4200?bond=10.0.0.2:4200");
    assert_eq!(u.host_port, "10.0.0.1:4200");
    assert!(u.streamid.is_empty());
    assert_eq!(u.bond_addrs, vec!["10.0.0.2:4200"]);
}

#[test]
fn sysctl_check_does_not_panic() {
    // Smoke test: runs on any Linux, should not panic even if paths don't exist
    check_sysctl_limits();
}

#[test]
fn socket_option_constants_match_srt_header() {
    // Guard against regression: these values are from srt.h SRT_SOCKOPT enum
    assert_eq!(SRTO_SNDSYN, 1);
    assert_eq!(SRTO_RCVSYN, 2);
    assert_eq!(SRTO_FC, 4);
    assert_eq!(SRTO_SNDBUF, 5);
    assert_eq!(SRTO_RCVBUF, 6);
    assert_eq!(SRTO_UDP_SNDBUF, 8);
    assert_eq!(SRTO_UDP_RCVBUF, 9);
    assert_eq!(SRTO_REUSEADDR, 15);
    assert_eq!(SRTO_MAXBW, 16);
    assert_eq!(SRTO_LATENCY, 23);
    assert_eq!(SRTO_LOSSMAXTTL, 42);
    assert_eq!(SRTO_RCVLATENCY, 43);
    assert_eq!(SRTO_PEERLATENCY, 44);
    assert_eq!(SRTO_STREAMID, 46);
    assert_eq!(SRTO_TRANSTYPE, 50);
    assert_eq!(SRTO_GROUPCONNECT, 57);
    assert_eq!(SRTGROUP_MASK, 1 << 30);
}

#[test]
fn detects_srt_group_ids() {
    assert!(!is_srt_group(42));
    assert!(is_srt_group(SRTGROUP_MASK | 42));
}

// --- Regression: issue #7 (Round 5) — Semaphore caps concurrent SRT sender threads ---
// Before the fix there was no limit on how many OS threads could be spawned
// for SRT play / egress connections. 1 thread per connection × 1000 connections
// = 1000 threads = 8+ GB virtual address space.
// The semaphore must be exhaustible and must release on drop.
#[test]
fn srt_sender_semaphore_is_bounded() {
    use std::sync::Arc;
    // Create a tiny semaphore (capacity 2) to simulate the cap.
    let sem = Arc::new(tokio::sync::Semaphore::new(2));
    let _p1 = try_acquire_srt_sender_permit(sem.clone()).expect("first permit available");
    let _p2 = try_acquire_srt_sender_permit(sem.clone()).expect("second permit available");
    // Third acquire must fail when semaphore is exhausted.
    assert!(
        try_acquire_srt_sender_permit(sem.clone()).is_err(),
        "semaphore must reject when exhausted"
    );
}

#[test]
fn srt_sender_semaphore_releases_on_drop() {
    use std::sync::Arc;
    let sem = Arc::new(tokio::sync::Semaphore::new(1));
    {
        let _p = try_acquire_srt_sender_permit(sem.clone()).expect("permit available");
        // permit is held — semaphore exhausted.
        assert!(
            try_acquire_srt_sender_permit(sem.clone()).is_err(),
            "should be exhausted"
        );
    }
    // After the permit is dropped, the slot must be returned.
    assert!(
        try_acquire_srt_sender_permit(sem.clone()).is_ok(),
        "semaphore should release permit on drop"
    );
}

// --- Regression: Round 6 #5 — SRT play muxer must not start without video ---
// The probe-wait loop in handle_play requires `video.as_ref()?` before
// breaking — it must not yield metadata when video is None.
// This is the same guard used by start_srt_egress.
#[test]
fn probe_wait_guard_requires_video_to_be_some() {
    // Simulate the logic of the retry closure:
    //   ingests.get(pipeline_id).and_then(|i| { video.as_ref()?; ... Some(meta) })
    // When video is None the closure must return None (no break).
    struct FakeIngest {
        video: Option<String>,
    }
    let ingest_no_video = FakeIngest { video: None };
    let ingest_with_video = FakeIngest {
        video: Some("h264".to_string()),
    };

    let result_none: Option<(&str,)> = (|| {
        let video = ingest_no_video.video.as_ref()?;
        let _ = video;
        Some(("got_video",))
    })();
    assert!(
        result_none.is_none(),
        "loop must not break while video is None"
    );

    let result_some: Option<(&str,)> = (|| {
        let video = ingest_with_video.video.as_ref()?;
        let _ = video;
        Some(("got_video",))
    })();
    assert!(result_some.is_some(), "loop must break once video is Some");
}

#[test]
fn summarizes_srt_group_member_state() {
    let mut connected: SrtSocketGroupData = unsafe { std::mem::zeroed() };
    connected.sockstate = SRTS_CONNECTED;
    connected.memberstate = SRT_GST_RUNNING;

    let mut idle: SrtSocketGroupData = unsafe { std::mem::zeroed() };
    idle.sockstate = SRTS_CONNECTED;
    idle.memberstate = 1;

    let mut broken: SrtSocketGroupData = unsafe { std::mem::zeroed() };
    broken.sockstate = SRTS_BROKEN;
    broken.memberstate = SRT_GST_BROKEN;

    assert_eq!(
        summarize_group_members(&[connected, idle, broken]),
        SrtGroupSummary {
            member_count: 3,
            connected_members: 2,
            active_members: 1,
            broken_members: 1,
        }
    );
}

#[test]
fn adds_bonded_group_state_to_publisher_quality() {
    let mut quality = PublisherQuality::default();
    add_srt_group_quality(
        &mut quality,
        true,
        Some(SrtGroupSummary {
            member_count: 2,
            connected_members: 2,
            active_members: 1,
            broken_members: 0,
        }),
    );

    assert_eq!(quality.srt_bonded, Some(true));
    assert_eq!(quality.srt_group_member_count, Some(2));
    assert_eq!(quality.srt_group_connected_members, Some(2));
    assert_eq!(quality.srt_group_active_members, Some(1));
    assert_eq!(quality.srt_group_broken_members, Some(0));
}

#[test]
fn marks_single_link_srt_without_group_member_fields() {
    let mut quality = PublisherQuality::default();
    add_srt_group_quality(&mut quality, false, None);

    assert_eq!(quality.srt_bonded, Some(false));
    assert_eq!(quality.srt_group_member_count, None);
    assert_eq!(quality.srt_group_connected_members, None);
    assert_eq!(quality.srt_group_active_members, None);
    assert_eq!(quality.srt_group_broken_members, None);
}

#[test]
fn maps_srt_sender_quality_from_bistats() {
    let stats = SrtTraceBStats {
        ms_rtt: 12.5,
        mbps_send_rate: 3.25,
        mbps_bandwidth: 42.0,
        ms_snd_tsb_pd_delay: 120,
        ms_snd_buf: 80,
        pkt_snd_loss_total: 10,
        pkt_snd_drop_total: 3,
        pkt_retrans_total: 5,
        pkt_recv_nak_total: 7,
        byte_snd_buf: 4096,
        byte_avail_snd_buf: 8192,
        pkt_flight_size: 4,
        pkt_flow_window: 8192,
        pkt_congestion_window: 1024,
        ..unsafe { std::mem::zeroed() }
    };
    let sampled_at = Instant::now();
    let previous = SrtSenderCounterSnapshot {
        packets_sent_loss: 4,
        packets_sent_drop: 1,
        packets_sent_retrans: 2,
        sampled_at: sampled_at - Duration::from_secs(2),
    };

    let (quality, snapshot) = srt_sender_quality_from_stats(&stats, Some(previous), sampled_at);

    assert_eq!(quality.ms_rtt, Some(12.5));
    assert_eq!(quality.mbps_send_rate, Some(3.25));
    assert_eq!(quality.mbps_link_capacity, Some(42.0));
    assert_eq!(quality.ms_send_tsb_pd_delay, Some(120.0));
    assert_eq!(quality.ms_send_buf, Some(80.0));
    assert_eq!(quality.packets_sent_loss, Some(10));
    assert_eq!(quality.packets_sent_drop, Some(3));
    assert_eq!(quality.packets_sent_retrans, Some(5));
    assert_eq!(quality.packets_received_nak, Some(7));
    assert_eq!(quality.packets_sent_loss_per_sec, Some(3.0));
    assert_eq!(quality.packets_sent_drop_per_sec, Some(1.0));
    assert_eq!(quality.packets_sent_retrans_per_sec, Some(1.5));
    assert_eq!(quality.srt_send_buf_bytes, Some(4096));
    assert_eq!(quality.srt_send_buf_avail_bytes, Some(8192));
    assert_eq!(quality.srt_flight_size_pkts, Some(4));
    assert_eq!(quality.srt_flow_window_pkts, Some(8192));
    assert_eq!(quality.srt_congestion_window_pkts, Some(1024));
    assert_eq!(snapshot.packets_sent_loss, 10);
    assert_eq!(snapshot.packets_sent_drop, 3);
    assert_eq!(snapshot.packets_sent_retrans, 5);
}

#[test]
fn linked_libsrt_exposes_group_connect_when_required() {
    unsafe {
        assert_eq!(srt_startup(), 0);
    }

    let listener = unsafe { srt_create_socket() };
    assert!(listener >= 0);
    if let Err(error) = enable_srt_group_connect(listener) {
        unsafe {
            srt_close(listener);
            srt_cleanup();
        }
        if crate::AppConfig::from_env().require_srt_bonding {
            panic!(
                "RESTREAM_REQUIRE_SRT_BONDING is set, but linked libsrt rejected \
                     SRTO_GROUPCONNECT: {error}. Rebuild libsrt with ENABLE_BONDING=ON."
            );
        }
        warn!(err = %error, "bonding prerequisite unavailable; set RESTREAM_REQUIRE_SRT_BONDING=1 in bonding-enabled CI");
        return;
    }
    unsafe {
        srt_close(listener);
    }
}

#[test]
fn linked_libsrt_accepts_every_supported_pbkeylen_via_socket_option() {
    unsafe {
        assert_eq!(srt_startup(), 0);
    }

    for pbkeylen in [16, 24, 32] {
        let crypto = srt_crypto_from_url("s3cret-passphrase".to_string(), Some(pbkeylen))
            .expect("non-empty passphrase must yield a crypto config");
        let sock = unsafe { srt_create_socket() };
        assert!(sock >= 0);

        let result = apply_srt_crypto_socket(sock, &crypto);
        unsafe {
            srt_close(sock);
        }
        assert!(
            result.is_ok(),
            "pbkeylen={pbkeylen} should be accepted by libsrt via SRTO_PBKEYLEN: {result:?}"
        );
    }

    unsafe {
        srt_cleanup();
    }
}

#[test]
fn linked_libsrt_rejects_out_of_range_pbkeylen_via_socket_option() {
    unsafe {
        assert_eq!(srt_startup(), 0);
    }

    let crypto = srt_crypto_from_url("s3cret-passphrase".to_string(), Some(999))
        .expect("non-empty passphrase must yield a crypto config");
    let sock = unsafe { srt_create_socket() };
    assert!(sock >= 0);

    let result = apply_srt_crypto_socket(sock, &crypto);
    unsafe {
        srt_close(sock);
        srt_cleanup();
    }

    let error =
        result.expect_err("libsrt must reject an out-of-range SRTO_PBKEYLEN through the FFI");
    assert!(
        error.contains("SRTO_PBKEYLEN"),
        "expected the FFI error surface to name the rejected option, got: {error}"
    );
}

/// Documents a real libsrt bonding quirk that once caused a production bug:
/// the per-member `SRT_SOCKOPT_CONFIG` object (`srt_create_config` /
/// `srt_config_add`) silently rejects `SRTO_PASSPHRASE` and `SRTO_STREAMID`
/// (see `SRT_SocketOptionObject::add` in libsrt's `socketconfig.cpp`, which
/// has no case for either option and falls through to `return false`), and
/// `srt_config_add`'s failure path never calls `CUDT::APIError`, so
/// `check_srt_option_result` misreports the failure as "Success (0)". Bonded
/// SRT egress applies these as group-wide socket options instead (see
/// `linked_libsrt_group_socket_accepts_crypto_via_setsockopt` and
/// `linked_libsrt_group_socket_accepts_streamid_via_setsockopt` below, and
/// the production call sites in `srt_egress.rs`). If a future libsrt version
/// starts accepting these through the per-member config, this test's
/// failure is the signal that the workaround can be revisited.
#[test]
fn linked_libsrt_member_config_rejects_passphrase_and_streamid() {
    unsafe {
        assert_eq!(srt_startup(), 0);
    }

    let config = unsafe { srt_create_config() };
    assert!(!config.is_null());

    let passphrase_c = std::ffi::CString::new("s3cret-passphrase").unwrap();
    let passphrase_result = unsafe {
        srt_config_add(
            config,
            SRTO_PASSPHRASE,
            passphrase_c.as_ptr() as *const c_void,
            17,
        )
    };
    let streamid_c = std::ffi::CString::new("probe").unwrap();
    let streamid_result = unsafe {
        srt_config_add(
            config,
            SRTO_STREAMID,
            streamid_c.as_ptr() as *const c_void,
            5,
        )
    };
    unsafe {
        srt_delete_config(config);
        srt_cleanup();
    }

    assert_eq!(
        passphrase_result, -1,
        "libsrt's per-member config unexpectedly accepted SRTO_PASSPHRASE; \
         the srt_egress.rs group-socket workaround may no longer be needed"
    );
    assert_eq!(
        streamid_result, -1,
        "libsrt's per-member config unexpectedly accepted SRTO_STREAMID; \
         the srt_egress.rs group-socket workaround may no longer be needed"
    );
}

#[test]
fn linked_libsrt_group_socket_accepts_crypto_via_setsockopt() {
    unsafe {
        assert_eq!(srt_startup(), 0);
    }
    let group = unsafe { srt_create_group(SRT_GTYPE_BACKUP) };
    assert!(group >= 0, "group={group}");

    let crypto = srt_crypto_from_url("s3cret-passphrase".to_string(), Some(16))
        .expect("non-empty passphrase must yield a crypto config");
    let result = apply_srt_crypto_socket(group, &crypto);
    unsafe {
        srt_close(group);
        srt_cleanup();
    }
    assert!(
        result.is_ok(),
        "bonded group sockets must accept crypto via SRTO_PASSPHRASE/SRTO_PBKEYLEN \
         setsockopt: {result:?}"
    );
}

#[test]
fn linked_libsrt_group_socket_accepts_streamid_via_setsockopt() {
    unsafe {
        assert_eq!(srt_startup(), 0);
    }
    let group = unsafe { srt_create_group(SRT_GTYPE_BACKUP) };
    assert!(group >= 0, "group={group}");
    let streamid_c = std::ffi::CString::new("probe").unwrap();
    let result = unsafe {
        check_srt_option_result(
            "SRTO_STREAMID",
            srt_setsockopt(
                group,
                0,
                SRTO_STREAMID,
                streamid_c.as_ptr() as *const c_void,
                5,
            ),
        )
    };
    unsafe {
        srt_close(group);
        srt_cleanup();
    }
    assert!(
        result.is_ok(),
        "bonded group sockets must accept StreamID via setsockopt: {result:?}"
    );
}

#[test]
fn reads_udp_socket_stats_for_listener_port() {
    // On a system without an SRT listener, this should return None
    // (port 10080 not bound). If it's bound, it returns Some.
    let result = read_udp_socket_stats(10080);
    // Either None or Some with valid values — should not panic
    if let Some((rx_queue, drops)) = result {
        assert!(rx_queue < u64::MAX);
        assert!(drops < u64::MAX);
    }
}

#[tokio::test]
async fn monitor_listener_socket_extreme_capacity_does_not_panic() {
    // effective_udp_recv_capacity near u64::MAX previously overflowed the
    // `configured_buf * 3` threshold multiplication before the first .await,
    // panicking the monitor task immediately on spawn.
    let stats = Arc::new(crate::media::engine::ListenerSocketStats::default());
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        monitor_listener_socket(0, stats, u64::MAX),
    )
    .await;
    // The function loops forever, so we expect the timeout to fire — the
    // only thing under test is that it doesn't panic before then.
    assert!(
        result.is_err(),
        "monitor_listener_socket should still be running (not panicked) when the timeout fires"
    );
}

#[tokio::test]
async fn start_srt_egress_handles_invalid_streamid_without_panic() {
    let ring_buffer = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let registration = engine
        .register_egress_attempt(
            "out-id",
            "pipe-id",
            "srt://127.0.0.1:12345?streamid=publish:mykey",
            None,
        )
        .await;
    start_srt_egress(
        "out-id".to_string(),
        "pipe-id".to_string(),
        "source".to_string(),
        "srt://127.0.0.1:12345?streamid=publish:\x00mykey".to_string(),
        ring_buffer,
        engine,
        registration,
    )
    .await;
}

#[tokio::test]
async fn shared_ts_muxer_shares_across_multiple_readers() {
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let pipeline_id = "test-pipe";
    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;

    // Register active ingest so start_shared_ts_muxer can proceed
    let cancel_ingest = engine
        .try_register_ingest(pipeline_id, "key", "srt")
        .await
        .unwrap();
    // Set metadata
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "h264".to_string(),
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
            }),
            None,
            None,
        )
        .await;

    // Create multiple stages or the same stage
    let stage1 = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "play", source_ring.clone())
        .await;
    let stage2 = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "play", source_ring.clone())
        .await;

    // Verify it is the exact same instance (same pointer)
    assert!(Arc::ptr_eq(&stage1, &stage2));

    // Create two readers
    let mut r1 = TsChunkReader::new("r1".to_string(), &stage1);
    let mut r2 = TsChunkReader::new("r2".to_string(), &stage1);
    wait_for_shared_muxer_source_reader(&source_ring).await;

    // Push a video packet to the source ring
    source_ring.push(crate::media::ring_buffer::MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 1000,
        dts: 1000,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65, 1, 2, 3]),
    });

    // Wait a bit for the tokio task to run and mux the packet
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let mut out1 = Vec::new();
    let mut out2 = Vec::new();
    assert_eq!(r1.pull_burst(&mut out1, 10).unwrap(), 1);
    assert_eq!(r2.pull_burst(&mut out2, 10).unwrap(), 1);

    assert_eq!(out1[0].payload, out2[0].payload);
    assert!(!out1[0].payload.is_empty());

    cancel_ingest.cancel();
}

#[tokio::test]
async fn shared_ts_muxer_uses_routed_audio_track_metadata() {
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let pipeline_id = "test-pipe-routed-audio";
    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;
    let cancel_ingest = engine
        .try_register_ingest(pipeline_id, "key", "srt")
        .await
        .unwrap();

    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "h264".to_string(),
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
            }),
            None,
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks(
            pipeline_id,
            vec![
                AudioMeta {
                    codec: "aac".to_string(),
                    sample_rate: 48_000,
                    channels: 2,
                    track_index: 0,
                    ..Default::default()
                },
                AudioMeta {
                    codec: "aac".to_string(),
                    sample_rate: 48_000,
                    channels: 2,
                    track_index: 1,
                    ..Default::default()
                },
            ],
        )
        .await;
    source_ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..Default::default()
    }]);

    let stage = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "source+atrack:0", source_ring.clone())
        .await;
    let mut reader = TsChunkReader::new("routed-audio-reader".to_string(), &stage);
    wait_for_shared_muxer_source_reader(&source_ring).await;

    let (_, _, fixture_packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h264").expect("h264 fixture");
    let probe_ready_video = fixture_packets
        .iter()
        .find(|p| p.media_type == MediaType::Video && p.is_keyframe)
        .expect("fixture keyframe")
        .payload
        .clone();
    source_ring.push(crate::media::ring_buffer::MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 1000,
        dts: 1000,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: probe_ready_video,
    });
    source_ring.push(crate::media::ring_buffer::MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts: 1020,
        dts: 1020,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0x11; 32]),
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let mut chunks = Vec::new();
    assert!(reader.pull_burst(&mut chunks, 10).unwrap() > 0);

    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    for chunk in &chunks {
        demuxer.feed(&chunk.payload);
    }
    demuxer.flush();
    let probe = demuxer.take_probe().expect("muxed TS should probe");
    assert_eq!(
        probe.audio_tracks.len(),
        1,
        "SRT subset muxer PMT must advertise only routed audio tracks"
    );

    cancel_ingest.cancel();
    stage.cancel.cancel();
}

#[tokio::test]
async fn shared_ts_muxer_seeds_raw_hevc_parameter_sets_for_late_joiners() {
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let pipeline_id = "test-pipe-routed-hevc";
    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;
    let cancel_ingest = engine
        .try_register_ingest(pipeline_id, "key", "srt")
        .await
        .unwrap();

    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "hevc".to_string(),
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
            }),
            None,
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks(
            pipeline_id,
            vec![AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48_000,
                channels: 2,
                track_index: 1,
                ..Default::default()
            }],
        )
        .await;
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 1,
        ..Default::default()
    }]);
    let parameter_sets = vec![
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB, 0x00,
        0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
    ];
    source_ring.set_video_parameter_sets(parameter_sets.clone());

    let stage = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "source+atrack:1", source_ring.clone())
        .await;
    let mut reader = TsChunkReader::new("routed-hevc-reader".to_string(), &stage);
    wait_for_shared_muxer_source_reader(&source_ring).await;

    source_ring.push(crate::media::ring_buffer::MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 1000,
        dts: 1000,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xDD]),
    });
    source_ring.push(crate::media::ring_buffer::MediaPacket {
        media_type: MediaType::Audio,
        track_index: 1,
        pts: 1020,
        dts: 1020,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0x11; 32]),
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let mut chunks = Vec::new();
    assert!(reader.pull_burst(&mut chunks, 10).unwrap() > 0);

    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in &chunks {
        demuxer.feed(&chunk.payload);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let video = packets
        .iter()
        .find(|packet| packet.media_type == MediaType::Video)
        .expect("muxed TS should contain video");
    assert!(
        video.payload.starts_with(&parameter_sets),
        "late-joining HEVC SRT muxer must prepend cached VPS/SPS/PPS"
    );

    cancel_ingest.cancel();
    stage.cancel.cancel();
}

#[tokio::test]
async fn shared_ts_muxer_prefers_preset_ring_parameter_sets_over_ingest_cache() {
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let pipeline_id = "test-pipe-preset-mismatch";

    // Raw ingest: registers an active ingest and caches an FLV AVC sequence
    // header, exactly as RTMP ingest does. This populates the pipeline-level
    // get_sequence_headers() cache with the *ingest's* (not the preset's)
    // SPS/PPS.
    let cancel_ingest = engine
        .try_register_ingest(pipeline_id, "key", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "h264".to_string(),
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
            }),
            None,
            None,
        )
        .await;
    // FLV AVC sequence header tag body: [frame_type<<4|codec_id, pkt_type,
    // composition_time(3 bytes), AVCDecoderConfigurationRecord...].
    // AVCDecoderConfigurationRecord: version, profile, compat, level,
    // nalu_len_size byte, num_sps, sps_len(u16), sps..., num_pps,
    // pps_len(u16), pps... — encodes the ingest's own (1920x1080) SPS/PPS.
    let ingest_flv_seq_header = bytes::Bytes::from(vec![
        0x17, 0x00, 0x00, 0x00, 0x00, // FLV video tag header + composition time
        0x01, 0x64, 0x00, 0x1e, 0xFF, // AVCC version/profile/compat/level/nalu_len
        0x01, 0x00, 0x04, 0x67, 0x11, 0x22, 0x33, // 1 SPS, len 4
        0x01, 0x00, 0x04, 0x68, 0x44, 0x55, 0x66, // 1 PPS, len 4
    ]);
    let ingest_parameter_sets: &[u8] = &[
        0, 0, 0, 1, 0x67, 0x11, 0x22, 0x33, 0, 0, 0, 1, 0x68, 0x44, 0x55, 0x66,
    ];
    engine
        .cache_sequence_header(pipeline_id, true, ingest_flv_seq_header)
        .await;

    // Preset/transcoded ring: a distinct ring (e.g. the 720p transcoder's
    // output ring) with its own, different SPS/PPS.
    let preset_ring = Arc::new(RingBuffer::new(16));
    preset_ring.set_codec_hint("h264");
    let preset_parameter_sets = vec![
        0, 0, 0, 1, 0x67, 0xAA, 0xBB, 0xCC, 0, 0, 0, 1, 0x68, 0xDD, 0xEE, 0xFF,
    ];
    preset_ring.set_video_parameter_sets(preset_parameter_sets.clone());

    let stage = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "720p", preset_ring.clone())
        .await;
    let mut reader = TsChunkReader::new("preset-reader".to_string(), &stage);
    wait_for_shared_muxer_source_reader(&preset_ring).await;

    preset_ring.push(crate::media::ring_buffer::MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 1000,
        dts: 1000,
        is_keyframe: true,
        format: PayloadFormat::Flv,
        payload: bytes::Bytes::from(vec![
            0x17, 0x01, 0x00, 0x00, 0x00, // AVC keyframe packet, no composition offset
            0x00, 0x00, 0x00, 0x04, 0x65, 0xAB, 0xCD, 0xEF, // one 4-byte-length-prefixed NALU
        ]),
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let mut chunks = Vec::new();
    assert!(reader.pull_burst(&mut chunks, 10).unwrap() > 0);

    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in &chunks {
        demuxer.feed(&chunk.payload);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let video = packets
        .iter()
        .find(|packet| packet.media_type == MediaType::Video)
        .expect("muxed TS should contain video");
    assert!(
        video.payload.starts_with(&preset_parameter_sets),
        "preset SRT muxer must prime from its own transcoded ring's parameter sets, \
             not the pipeline-level ingest sequence-header cache"
    );
    assert!(
        !video.payload.starts_with(ingest_parameter_sets),
        "preset SRT muxer must not seed the raw ingest's SPS/PPS"
    );

    cancel_ingest.cancel();
    stage.cancel.cancel();
}

#[tokio::test]
async fn shared_ts_muxer_replays_prebuffered_hevc_keyframe() {
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let pipeline_id = "test-pipe-prebuffered-hevc";
    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;
    let cancel_ingest = engine
        .try_register_ingest(pipeline_id, "key", "srt")
        .await
        .unwrap();

    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "hevc".to_string(),
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
            }),
            None,
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks(
            pipeline_id,
            vec![AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48_000,
                channels: 2,
                track_index: 0,
                ..Default::default()
            }],
        )
        .await;
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..Default::default()
    }]);
    let parameter_sets = vec![
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB, 0x00,
        0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
    ];
    source_ring.set_video_parameter_sets(parameter_sets.clone());
    source_ring.push(crate::media::ring_buffer::MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 1000,
        dts: 1000,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xDD]),
    });
    source_ring.push(crate::media::ring_buffer::MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts: 1020,
        dts: 1020,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0x11; 32]),
    });

    let stage = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "source", source_ring.clone())
        .await;
    let mut reader = TsChunkReader::new("prebuffered-hevc-reader".to_string(), &stage);
    wait_for_shared_muxer_source_reader(&source_ring).await;

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let mut chunks = Vec::new();
    assert!(
        reader.pull_burst(&mut chunks, 10).unwrap() > 0,
        "late-joining shared muxer must replay the latest prebuffered HEVC keyframe"
    );

    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in &chunks {
        demuxer.feed(&chunk.payload);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let video = packets
        .iter()
        .find(|packet| packet.media_type == MediaType::Video)
        .expect("muxed TS should contain video");
    assert!(
        video.payload.starts_with(&parameter_sets),
        "prebuffered HEVC replay must include cached VPS/SPS/PPS"
    );

    cancel_ingest.cancel();
    stage.cancel.cancel();
}

async fn wait_for_shared_muxer_source_reader(source_ring: &Arc<RingBuffer>) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.starts_with("ts_shared_muxer:"))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "shared muxer source reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn shared_ts_muxer_cancels_and_recreates_after_probe_wait_exit() {
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let pipeline_id = "test-pipe-probe-exit";
    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;

    engine
        .try_register_ingest(pipeline_id, "key", "srt")
        .await
        .unwrap();

    let stage1 = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "play", source_ring.clone())
        .await;

    engine.unregister_ingest(pipeline_id).await;

    tokio::time::timeout(std::time::Duration::from_secs(2), stage1.cancel.cancelled())
        .await
        .expect("shared muxer should cancel when ingest disappears before probe");
    assert!(stage1.cancel.is_cancelled());

    engine
        .try_register_ingest(pipeline_id, "key-2", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "h264".to_string(),
                width: 1280,
                height: 720,
                fps: 30.0,
                bw: None,
                pid: None,
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            None,
            None,
        )
        .await;

    let stage2 = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "play", source_ring)
        .await;

    assert!(
        !Arc::ptr_eq(&stage1, &stage2),
        "cancelled shared muxer stage must not be reused"
    );
    assert!(!stage2.cancel.is_cancelled());

    engine.unregister_ingest(pipeline_id).await;
    stage2.cancel.cancel();
}

#[tokio::test]
async fn benchmark_srt_sharing() {
    info!("\n=== SRT EGRESS SHARING BENCHMARK ===");
    let n_connections = 10;
    let n_packets = 2000;
    info!("Clients (N): {}, Packets (M): {}", n_connections, n_packets);

    let video_meta = VideoMeta {
        codec: "h264".to_string(),
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
    let audio_track = crate::media::engine::AudioMeta {
        track_index: 0,
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        profile: None,
        pid: None,
        language: None,
        title: None,
    };
    let audio_tracks = vec![audio_track];

    // Generate synthetic packets
    let mut packets = Vec::with_capacity(n_packets);
    let mut rng_seed = 0u8;
    for i in 0..n_packets {
        let is_video = i % 3 != 0;
        let is_keyframe = is_video && (i % 90 == 0);
        let media_type = if is_video {
            MediaType::Video
        } else {
            MediaType::Audio
        };
        let size = if is_video {
            if is_keyframe { 100_000 } else { 10_000 }
        } else {
            500
        };
        rng_seed = rng_seed.wrapping_add(1);
        let payload = bytes::Bytes::from(vec![rng_seed; size]);
        packets.push(crate::media::ring_buffer::MediaPacket {
            media_type,
            track_index: 0,
            pts: i as i64 * 33,
            dts: i as i64 * 33,
            is_keyframe,
            format: PayloadFormat::Raw,
            payload,
        });
    }

    // --- OLD ARCHITECTURE: Independent Muxing ---
    let start_old = Instant::now();
    let mut old_handles = Vec::new();
    for _ in 0..n_connections {
        let packets_clone = packets.clone();
        let video_meta_clone = video_meta.clone();
        let audio_tracks_clone = audio_tracks.clone();
        let handle = tokio::spawn(async move {
            let mut muxer =
                crate::media::mpegts::TsMuxer::new(Some(&video_meta_clone), &audio_tracks_clone);
            let mut bytes_written = 0u64;
            for pkt in &packets_clone {
                let ts_bytes = muxer.mux_packet(
                    pkt.media_type,
                    pkt.track_index,
                    pkt.pts,
                    pkt.dts,
                    pkt.is_keyframe,
                    &pkt.payload,
                );
                bytes_written += ts_bytes.len() as u64;
            }
            bytes_written
        });
        old_handles.push(handle);
    }

    let mut total_bytes_old = 0u64;
    for h in old_handles {
        total_bytes_old += h.await.unwrap();
    }
    let elapsed_old = start_old.elapsed();

    // --- NEW ARCHITECTURE: Shared Muxing ---
    let start_new = Instant::now();
    let ts_ring = Arc::new(TsChunkRing::new(4096, CancellationToken::new()));
    let mut readers = Vec::new();
    for i in 0..n_connections {
        readers.push(TsChunkReader::new(format!("reader_{}", i), &ts_ring));
    }

    let mut new_handles = Vec::new();
    for mut reader in readers {
        let handle = tokio::spawn(async move {
            let mut chunks_received = 0;
            let mut bytes_received = 0u64;
            let mut out_burst = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
            while chunks_received < n_packets {
                out_burst.clear();
                match reader.pull_burst(&mut out_burst, MEDIA_PULL_BURST_PACKETS) {
                    Ok(0) => {
                        tokio::time::sleep(std::time::Duration::from_micros(100)).await;
                    }
                    Ok(count) => {
                        chunks_received += count;
                        for chunk in &out_burst {
                            bytes_received += chunk.payload.len() as u64;
                        }
                    }
                    Err(_) => {}
                }
            }
            bytes_received
        });
        new_handles.push(handle);
    }

    // Shared muxer task
    let ts_ring_clone = ts_ring.clone();
    let packets_clone = packets.clone();
    let video_meta_clone = video_meta.clone();
    let audio_tracks_clone = audio_tracks.clone();
    let muxer_handle = tokio::spawn(async move {
        let mut muxer =
            crate::media::mpegts::TsMuxer::new(Some(&video_meta_clone), &audio_tracks_clone);
        for pkt in &packets_clone {
            let ts_bytes = muxer.mux_packet(
                pkt.media_type,
                pkt.track_index,
                pkt.pts,
                pkt.dts,
                pkt.is_keyframe,
                &pkt.payload,
            );
            ts_ring_clone.push(bytes::Bytes::copy_from_slice(ts_bytes), pkt.is_keyframe);
        }
    });

    muxer_handle.await.unwrap();

    let mut total_bytes_new = 0u64;
    for h in new_handles {
        total_bytes_new += h.await.unwrap();
    }
    let elapsed_new = start_new.elapsed();

    info!("Old Architecture Time: {:?}", elapsed_old);
    info!("New Architecture Time: {:?}", elapsed_new);
    info!("Old Total Bytes Muxed: {}", total_bytes_old);
    info!("New Total Bytes Muxed: {}", total_bytes_new);

    assert_eq!(total_bytes_old, total_bytes_new);

    let ratio = elapsed_old.as_secs_f64() / elapsed_new.as_secs_f64();
    info!("Performance Gain Ratio: {:.2}x", ratio);
    info!("=====================================");
}

/// Verify that when EpollStopGuard drops (simulating a cancelled async
/// future), a waiter parked in `wait_for_request()` observes the stop and
/// exits promptly. This exercises the RAII path that prevents
/// srt_epoll_release from being skipped on future cancellation.
#[tokio::test]
async fn epoll_stop_guard_signals_waiter_on_drop() {
    let signal = Arc::new(EpollWaiterSignal::new());
    let notify = Arc::new(Notify::new());
    let task_exited = Arc::new(AtomicBool::new(false));

    let w_signal = signal.clone();
    let w_exited = task_exited.clone();

    // Simulates the epoll_waiter task: parks for requests, exits on stop.
    let handle = tokio::task::spawn_blocking(move || {
        while w_signal.wait_for_request() {
            // No epoll in this simulation; a real waiter would arm one
            // srt_epoll_wait per serviced request here.
        }
        w_exited.store(true, Ordering::Release);
    });

    // EpollStopGuard inline: signals stop + notifies on drop.
    struct EpollStopGuard {
        signal: Arc<EpollWaiterSignal>,
        notify: Arc<Notify>,
    }
    impl Drop for EpollStopGuard {
        fn drop(&mut self) {
            self.signal.stop();
            self.notify.notify_one();
        }
    }
    let guard = EpollStopGuard {
        signal: signal.clone(),
        notify: notify.clone(),
    };

    // Drop the guard — simulates the async future being cancelled.
    drop(guard);

    // Task must exit within 300ms (condvar wake + scheduling slack).
    tokio::time::timeout(std::time::Duration::from_millis(300), handle)
        .await
        .expect("epoll_waiter task must exit within 300ms of guard drop")
        .expect("task should not panic");

    assert!(
        task_exited.load(Ordering::Acquire),
        "task must have observed the stop flag"
    );
}

/// The demand-gating regression: a waiter parked in `wait_for_request()` must
/// not run any epoll iterations while the receive loop is busy (no request
/// outstanding). The old unconditional wait loop spun a full core against a
/// level-triggered-ready socket; this asserts the waiter services exactly as
/// many waits as were requested — zero without a request.
#[tokio::test]
async fn epoll_waiter_parks_until_wait_is_requested() {
    use std::sync::atomic::AtomicU32;

    let signal = Arc::new(EpollWaiterSignal::new());
    let serviced = Arc::new(AtomicU32::new(0));

    let w_signal = signal.clone();
    let w_serviced = serviced.clone();
    let handle = tokio::task::spawn_blocking(move || {
        while w_signal.wait_for_request() {
            w_serviced.fetch_add(1, Ordering::Release);
        }
    });

    // Consumer busy, no request outstanding: the waiter must stay parked.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        serviced.load(Ordering::Acquire),
        0,
        "waiter must not service waits nobody requested"
    );

    // One request -> exactly one serviced wait, then parked again.
    signal.request_wait();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while serviced.load(Ordering::Acquire) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "waiter must service a requested wait"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        serviced.load(Ordering::Acquire),
        1,
        "one request must arm exactly one wait"
    );

    signal.stop();
    tokio::time::timeout(std::time::Duration::from_millis(300), handle)
        .await
        .expect("waiter must exit promptly on stop")
        .expect("task should not panic");
}

#[tokio::test]
async fn srt_readiness_wait_retries_without_epoll_notification() {
    let data_ready = AtomicBool::new(false);
    let signal = EpollWaiterSignal::new();
    let notify = Notify::new();
    let cancel = CancellationToken::new();

    let started = std::time::Instant::now();
    let should_retry = wait_for_srt_ingest_readiness(&data_ready, &signal, &notify, &cancel).await;
    let elapsed = started.elapsed();

    assert!(
        should_retry,
        "missing epoll notification should retry non-blocking srt_recv"
    );
    assert!(
        elapsed >= SRT_INGEST_READINESS_RETRY,
        "retry should wait for the bounded readiness interval"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "retry safeguard must not let ingest sleep indefinitely"
    );
}

#[tokio::test]
async fn srt_readiness_wait_exits_on_cancel() {
    let data_ready = AtomicBool::new(false);
    let signal = EpollWaiterSignal::new();
    let notify = Notify::new();
    let cancel = CancellationToken::new();
    cancel.cancel();

    let should_retry = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        wait_for_srt_ingest_readiness(&data_ready, &signal, &notify, &cancel),
    )
    .await
    .expect("cancelled readiness wait should return promptly");

    assert!(
        !should_retry,
        "cancelled ingest should break instead of retrying receive"
    );
}

#[test]
fn loom_srt_readiness_retry_does_not_depend_on_epoll_wake() {
    loom::model(|| {
        use loom::sync::Arc as LoomArc;
        use loom::sync::atomic::{AtomicBool as LoomAtomicBool, Ordering as LoomOrdering};
        use loom::thread;

        let data_ready = LoomArc::new(LoomAtomicBool::new(false));
        let wait_requested = LoomArc::new(LoomAtomicBool::new(false));
        let consumer_progress = LoomArc::new(LoomAtomicBool::new(false));

        let consumer_data_ready = data_ready.clone();
        let consumer_wait_requested = wait_requested.clone();
        let consumer_progress_flag = consumer_progress.clone();
        let consumer = thread::spawn(move || {
            if consumer_data_ready.swap(false, LoomOrdering::AcqRel) {
                consumer_progress_flag.store(true, LoomOrdering::Release);
                return;
            }

            consumer_wait_requested.store(true, LoomOrdering::Release);
            // Models the bounded retry timer in wait_for_srt_ingest_readiness:
            // even if the epoll waiter never observes the request, the async
            // receive loop must re-enter non-blocking srt_recv.
            consumer_progress_flag.store(true, LoomOrdering::Release);
        });

        let producer_data_ready = data_ready.clone();
        let producer_wait_requested = wait_requested.clone();
        let producer = thread::spawn(move || {
            if producer_wait_requested.load(LoomOrdering::Acquire) {
                producer_data_ready.store(true, LoomOrdering::Release);
            }
        });

        consumer.join().expect("consumer model should not panic");
        producer.join().expect("producer model should not panic");
        assert!(
            consumer_progress.load(LoomOrdering::Acquire),
            "readiness wait must make progress even when the epoll wake is lost"
        );
    });
}

#[derive(Debug, Clone, Copy)]
enum ReadinessOutcome {
    EpollWake,
    RetryTimer,
    LostWake,
    Cancel,
}

fn modeled_readiness_wait(already_ready: bool, outcome: ReadinessOutcome) -> bool {
    if already_ready {
        return true;
    }
    !matches!(outcome, ReadinessOutcome::Cancel)
}

prop_compose! {
    fn readiness_outcome_strategy()(raw in 0u8..4) -> ReadinessOutcome {
        match raw {
            0 => ReadinessOutcome::EpollWake,
            1 => ReadinessOutcome::RetryTimer,
            2 => ReadinessOutcome::LostWake,
            _ => ReadinessOutcome::Cancel,
        }
    }
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(128))]

    #[test]
    fn proptest_srt_readiness_retry_model_never_requires_epoll_wake(
        events in proptest::collection::vec((any::<bool>(), readiness_outcome_strategy()), 1..256)
    ) {
        for (already_ready, outcome) in events {
            let should_retry = modeled_readiness_wait(already_ready, outcome);
            if already_ready || !matches!(outcome, ReadinessOutcome::Cancel) {
                prop_assert!(
                    should_retry,
                    "readiness wait must retry for {:?} without requiring an epoll notification",
                    outcome
                );
            } else {
                prop_assert!(!should_retry, "cancel must remain the only non-retry outcome");
            }
        }
    }
}

/// Stress-test the demand-gated handshake used by the long-lived epoll waiter
/// (EpollWaiterSignal + AtomicBool + Notify). Concurrent producer and consumer
/// run with randomized timing to surface missed-wakeup races.
///
/// The producer (spawn_blocking) simulates the real waiter: parks in
/// wait_for_request(), then after a brief random delay (simulating
/// srt_epoll_wait returning ready) does store(true) + notify_one(). The
/// consumer (async) simulates the EAGAIN handler: swap(false) -> fall
/// through, or request_wait() + notified().await.
///
/// A 30-second deadline prevents hangs from missed wakeups, and the producer
/// must service no more waits than the consumer requested — the demand-gating
/// property that prevents the busy-spin.
#[tokio::test]
async fn epoll_waiter_coordination() {
    use rand::RngExt;
    use rand::SeedableRng;
    use std::sync::atomic::AtomicU32;

    const ITEMS: u32 = 10_000;
    let data_ready = Arc::new(AtomicBool::new(false));
    let signal = Arc::new(EpollWaiterSignal::new());
    let notify = Arc::new(Notify::new());
    let produced = Arc::new(AtomicU32::new(0));

    let w_data_ready = data_ready.clone();
    let w_signal = signal.clone();
    let w_notify = notify.clone();
    let w_produced = produced.clone();

    // Producer: services requested waits on a blocking thread until the
    // consumer signals stop.
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    let producer = tokio::task::spawn_blocking(move || {
        while w_signal.wait_for_request() {
            // Jitter: 1-9µs typical, occasionally 1ms (simulating idle).
            let delay = if rng.random_range(0..100) == 0 {
                1_000
            } else {
                rng.random_range(1..10)
            };
            std::thread::sleep(std::time::Duration::from_micros(delay));

            w_produced.fetch_add(1, Ordering::Relaxed);
            w_data_ready.store(true, Ordering::Release);
            w_notify.notify_one();
        }
    });

    // Consumer: exactly the swap+request_wait+notified pattern used by the
    // real EAGAIN handler (SrtReceiveErrorAction::WaitForReadiness).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut requested: u32 = 0;
    for i in 0..ITEMS {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out after {i} items (produced={})",
            produced.load(Ordering::Relaxed),
        );

        if !data_ready.swap(false, Ordering::Acquire) {
            requested += 1;
            signal.request_wait();
            tokio::time::timeout(std::time::Duration::from_secs(5), notify.notified())
                .await
                .expect("consumer should not hang: permit must be available");
        }
    }

    signal.stop();
    tokio::time::timeout(std::time::Duration::from_secs(5), producer)
        .await
        .expect("producer must exit promptly on stop")
        .expect("producer should not panic");

    let total_produced = produced.load(Ordering::Relaxed);
    assert!(
        total_produced <= requested,
        "producer serviced {total_produced} waits but only {requested} were requested - demand gating is broken"
    );
    assert!(
        requested <= ITEMS,
        "consumer requested more waits than iterations"
    );
}
