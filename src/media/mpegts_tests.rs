use super::mpegts_probe::*;
use super::*;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[test]
fn parse_timestamp_round_trip() {
    let ts: i64 = 132000; // 90kHz timestamp
    let mut buf = Vec::new();
    write_timestamp(&mut buf, ts, 0x02);
    let parsed = parse_timestamp(&buf);
    assert_eq!(parsed, ts);
}

fn h264_stream_info(pid: u16) -> StreamInfo {
    StreamInfo {
        pid,
        kind: StreamKind::H264,
        track_index: 0,
        language: None,
        title: None,
        continuity: CC_UNSET,
        pes: PesAccumulator::new(),
    }
}

fn aac_adts_stream_info(pid: u16, track_index: u32) -> StreamInfo {
    StreamInfo {
        pid,
        kind: StreamKind::AacAdts,
        track_index,
        language: None,
        title: None,
        continuity: CC_UNSET,
        pes: PesAccumulator::new(),
    }
}

fn ts_header_bytes(pid: u16, pusi: bool, afc: u8, cc: u8) -> [u8; 4] {
    [
        TS_SYNC_BYTE,
        (if pusi { 0x40 } else { 0x00 }) | ((pid >> 8) as u8 & 0x1F),
        (pid & 0xFF) as u8,
        (afc << 4) | (cc & 0x0F),
    ]
}

/// Builds a well-formed TS packet: an adaptation field sized exactly to
/// `af_body.len()` (if `afc` calls for one), followed by `payload` truncated
/// to whatever room remains and 0xFF-stuffed past that.
fn build_ts_packet(
    pid: u16,
    pusi: bool,
    afc: u8,
    cc: u8,
    af_body: &[u8],
    payload: &[u8],
) -> [u8; TS_PACKET_SIZE] {
    let mut pkt = [0xFFu8; TS_PACKET_SIZE];
    pkt[0..4].copy_from_slice(&ts_header_bytes(pid, pusi, afc, cc));
    let mut offset = 4;
    if afc == 0x02 || afc == 0x03 {
        pkt[offset] = af_body.len() as u8;
        offset += 1;
        pkt[offset..offset + af_body.len()].copy_from_slice(af_body);
        offset += af_body.len();
    }
    if afc == 0x01 || afc == 0x03 {
        let n = payload.len().min(TS_PACKET_SIZE - offset);
        pkt[offset..offset + n].copy_from_slice(&payload[..n]);
    }
    pkt
}

fn install_single_h264_stream(demuxer: &mut TsDemuxer, pid: u16) {
    demuxer.streams = vec![h264_stream_info(pid)];
    demuxer.pid_to_stream[pid as usize] = 0;
}

/// A single-TS-packet, PTS-only video PES with an explicit (bounded)
/// `pes_packet_len`, so `es_payload` demuxes exactly regardless of the
/// 0xFF stuffing that fills the rest of the fixed-size 184-byte TS payload
/// region. `payload_unit_start` carries a complete PES header (9-byte
/// mandatory + 5-byte PTS) plus `es_payload`.
fn valid_video_pes_packet(
    pid: u16,
    cc: u8,
    pts_90k: i64,
    es_payload: &[u8],
) -> [u8; TS_PACKET_SIZE] {
    const PES_HEADER_LEN: u8 = 5; // PTS-only optional header
    let pes_packet_len = 3 + PES_HEADER_LEN as u16 + es_payload.len() as u16;
    let mut pes = vec![0x00, 0x00, 0x01, 0xE0];
    pes.extend_from_slice(&pes_packet_len.to_be_bytes());
    pes.push(0x80);
    pes.push(0x80);
    pes.push(PES_HEADER_LEN);
    write_timestamp(&mut pes, pts_90k, 0x02);
    pes.extend_from_slice(es_payload);
    build_ts_packet(pid, true, 0x01, cc, &[], &pes)
}

/// Like [`valid_video_pes_packet`], but leaves `pes_packet_len` at 0
/// (unbounded), the standard MPEG-TS encoding for a video PES whose length
/// isn't known up front — completion then depends solely on the next
/// `payload_unit_start` packet.
fn unbounded_video_pes_start_packet(
    pid: u16,
    cc: u8,
    pts_90k: i64,
    es_payload: &[u8],
) -> [u8; TS_PACKET_SIZE] {
    let mut pes = vec![0x00, 0x00, 0x01, 0xE0, 0x00, 0x00, 0x80, 0x80, 0x05];
    write_timestamp(&mut pes, pts_90k, 0x02);
    pes.extend_from_slice(es_payload);
    build_ts_packet(pid, true, 0x01, cc, &[], &pes)
}

fn first_probe_ready_payloads() -> (Vec<u8>, Vec<u8>) {
    let fixture =
        crate::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let ts = std::fs::read(&fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let video = packets
        .iter()
        .find(|packet| {
            packet.media_type == MediaType::Video
                && video_meta_complete(
                    StreamKind::H264,
                    &probe_video(StreamKind::H264, 0x100, None, None, packet.payload.as_ref()),
                )
        })
        .map(|packet| packet.payload.to_vec())
        .expect("fixture should contain a probe-ready H.264 access unit");
    let audio = packets
        .iter()
        .find(|packet| {
            packet.media_type == MediaType::Audio
                && audio_meta_complete(
                    StreamKind::AacAdts,
                    &probe_audio(
                        StreamKind::AacAdts,
                        0,
                        0x101,
                        None,
                        None,
                        packet.payload.as_ref(),
                    ),
                )
        })
        .map(|packet| packet.payload.to_vec())
        .expect("fixture should contain a probe-ready AAC access unit");
    (video, audio)
}

#[test]
fn parse_timestamp_large_value() {
    let ts: i64 = 8_589_934_591; // max 33-bit value
    let mut buf = Vec::new();
    write_timestamp(&mut buf, ts, 0x03);
    let parsed = parse_timestamp(&buf);
    assert_eq!(parsed, ts);
}

#[test]
fn ts_ms_conversion() {
    assert_eq!(ts_to_ms(90000), 1000);
    assert_eq!(ts_to_ms(0), 0);
    assert_eq!(ms_to_ts(1000), 90000);
    assert_eq!(ms_to_ts(0), 0);
}

#[test]
fn ts_to_ms_no_float_drift() {
    // Verify no floating-point drift at 24-hour scale.
    // At 90 kHz, 24 hours = 24*3600*90000 = 7_776_000_000 ticks.
    // f64 has 53-bit mantissa; at this scale each ULP is ~1024 ticks = ~11 ms.
    // Integer division: ts / 90 must give exact ms with no drift.
    let day_90k: i64 = 24 * 3600 * 90_000;
    let day_ms: i64 = 24 * 3600 * 1000;
    assert_eq!(
        ts_to_ms(day_90k),
        day_ms,
        "ts_to_ms must be exact for 24-hour timestamps (no f64 drift)"
    );
    // Also verify round-trip for a one-hour mark
    let hour_90k: i64 = 3600 * 90_000;
    let hour_ms: i64 = 3600 * 1000;
    assert_eq!(ts_to_ms(hour_90k), hour_ms);
}

#[test]
fn crc32_known_value() {
    // PAT with known CRC
    let data = [
        0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xE1, 0x00,
    ];
    let crc = crc32_mpeg2(&data);
    assert_ne!(crc, 0); // Just verify it produces a non-trivial value
    // The expected CRC32/MPEG-2 of this PAT payload is 0xE8F95E7D
    assert_eq!(crc, 0xE8F95E7D);
}

#[test]
fn crc32_bit_at_a_time_equivalence() {
    // Local reference implementation of the bit-at-a-time algorithm
    let reference_crc = |data: &[u8]| {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= (byte as u32) << 24;
            for _ in 0..8 {
                if crc & 0x8000_0000 != 0 {
                    crc = (crc << 1) ^ 0x04C1_1DB7;
                } else {
                    crc <<= 1;
                }
            }
        }
        crc
    };

    // Test with different sizes and randomized inputs
    let mut rng = 12345u32;
    let mut next_random_byte = || {
        // simple LCG generator
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        (rng >> 24) as u8
    };

    for size in [0, 1, 4, 12, 188, 1024, 4096] {
        for _ in 0..10 {
            let data: Vec<u8> = (0..size).map(|_| next_random_byte()).collect();
            let ref_val = reference_crc(&data);
            let table_val = crc32_mpeg2(&data);
            assert_eq!(
                table_val, ref_val,
                "Failed equivalence test at size {}",
                size
            );
        }
    }
}

#[test]
fn demux_fixture_file() {
    let fixture =
        crate::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let ts_data = std::fs::read(&fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&ts_data);
    demuxer.flush();
    let packets = demuxer.drain();

    assert!(!packets.is_empty(), "should produce packets");

    let video_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Video)
        .count();
    let audio_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio)
        .count();
    assert!(video_count > 0, "should have video packets");
    assert!(audio_count > 0, "should have audio packets");

    // Check that at least one keyframe exists
    let keyframes = packets.iter().filter(|p| p.is_keyframe).count();
    assert!(keyframes > 0, "should have keyframes");

    // PTS should be monotonically non-decreasing per stream
    let mut last_video_pts = i64::MIN;
    let mut last_audio_pts = i64::MIN;
    for pkt in &packets {
        match pkt.media_type {
            MediaType::Video => {
                // DTS must be non-decreasing (PTS can jump with B-frames)
                // Just verify PTS is reasonable (positive)
                assert!(
                    pkt.pts >= 0,
                    "video PTS should be non-negative: {}",
                    pkt.pts
                );
                last_video_pts = pkt.pts;
            }
            MediaType::Audio => {
                assert!(
                    pkt.pts >= last_audio_pts,
                    "audio PTS should be non-decreasing: {} < {}",
                    pkt.pts,
                    last_audio_pts
                );
                last_audio_pts = pkt.pts;
            }
        }
    }
    let _ = last_video_pts;

    // Check probe
    let mut demuxer2 = TsDemuxer::new();
    demuxer2.feed(&ts_data);
    demuxer2.flush();
    let probe = demuxer2.take_probe();
    assert!(probe.is_some(), "should produce probe result");
    let probe = probe.unwrap();

    if let Some(ref video) = probe.video {
        assert_eq!(video.codec, "h264");
        assert_eq!(video.width, 1920);
        assert_eq!(video.height, 1080);
        assert!(
            (video.fps - 30.0).abs() < 1.0,
            "fps should be ~30: {}",
            video.fps
        );
        assert_eq!(video.profile.as_deref(), Some("High"));
    } else {
        panic!("should probe video metadata");
    }

    assert!(!probe.audio_tracks.is_empty());
    assert_eq!(probe.audio_tracks[0].codec, "aac");
    assert_eq!(probe.audio_tracks[0].sample_rate, 48000);
}

#[test]
fn drain_into_reuses_output_batches() {
    let mut demuxer = TsDemuxer::new();
    let mut output = Vec::with_capacity(4);

    assert_eq!(demuxer.drain_into(&mut output), 0);
    assert!(output.is_empty());
    assert!(output.capacity() >= 4);
}

#[test]
fn muxer_writes_service_metadata_sdt_packet() {
    let video = VideoMeta {
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
    };
    let metadata = TsServiceMetadata {
        provider_name: "Restream pipeline_id=pipe-1".to_string(),
        service_name: "pipeline=Main; source=publisher; recorded_at=2026-06-27T00:00:00Z"
            .to_string(),
    };
    let mut muxer = TsMuxer::new_with_metadata(Some(&video), &[], metadata);
    let payload = [0x00, 0x00, 0x01, 0x09, 0x10];
    let ts = muxer.mux_packet(MediaType::Video, 0, 0, 0, true, &payload);

    assert!(
        ts.chunks(TS_PACKET_SIZE).any(|pkt| {
            pkt.len() == TS_PACKET_SIZE
                && pkt[0] == TS_SYNC_BYTE
                && ((((pkt[1] & 0x1F) as u16) << 8) | pkt[2] as u16) == SDT_PID
        }),
        "first table burst should contain an SDT packet"
    );
    let text = String::from_utf8_lossy(ts);
    assert!(text.contains("Restream pipeline_id=pipe-1"));
    assert!(text.contains("pipeline=Main"));
    assert!(text.contains("source=publisher"));
    assert!(text.contains("recorded_at=2026-06-27T00:00:00Z"));
}

#[test]
fn demux_chunked_feed() {
    let fixture =
        crate::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let ts_data = std::fs::read(&fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));

    // Feed in 1316-byte chunks (SRT packet size)
    let mut demuxer = TsDemuxer::new();
    for chunk in ts_data.chunks(1316) {
        demuxer.feed(chunk);
    }
    demuxer.flush();
    let packets = demuxer.drain();

    assert!(!packets.is_empty(), "chunked feed should produce packets");

    // Feed all at once for comparison
    let mut demuxer2 = TsDemuxer::new();
    demuxer2.feed(&ts_data);
    demuxer2.flush();
    let packets2 = demuxer2.drain();

    assert_eq!(
        packets.len(),
        packets2.len(),
        "chunked and full feed should produce same packet count"
    );
}

#[test]
fn demux_h265_fixture_file() {
    let fixture =
        crate::test_fixtures::canonical_h265_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let ts_data = std::fs::read(&fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));

    let mut demuxer = TsDemuxer::new();
    for chunk in ts_data.chunks(1316) {
        demuxer.feed(chunk);
    }
    demuxer.flush();
    let packets = demuxer.drain();

    assert!(!packets.is_empty(), "should produce packets");

    let video_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Video)
        .count();
    let audio_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio)
        .count();
    assert!(video_count > 0, "should have video packets");
    assert!(audio_count > 0, "should have audio packets");

    let keyframes = packets.iter().filter(|p| p.is_keyframe).count();
    assert!(keyframes > 0, "should have keyframes");

    let mut demuxer2 = TsDemuxer::new();
    demuxer2.feed(&ts_data);
    demuxer2.flush();
    let probe = demuxer2.take_probe();
    assert!(probe.is_some(), "should produce probe result");
    let probe = probe.unwrap();

    if let Some(ref video) = probe.video {
        assert_eq!(video.codec, "hevc");
        assert_eq!(video.width, 1920);
        assert_eq!(video.height, 1080);
        assert!(
            (video.fps - 30.0).abs() < 1.0,
            "fps should be ~30: {}",
            video.fps
        );
        assert_eq!(video.profile.as_deref(), Some("Main"));
    } else {
        panic!("should probe video metadata");
    }

    assert!(!probe.audio_tracks.is_empty());
    assert_eq!(probe.audio_tracks[0].codec, "aac");
    assert_eq!(probe.audio_tracks[0].sample_rate, 48000);
}

#[test]
fn demux_corrupt_input_no_panic() {
    let mut demuxer = TsDemuxer::new();

    // Empty input
    demuxer.feed(&[]);
    assert!(demuxer.drain().is_empty());

    // Random garbage
    demuxer.feed(&[0xDE, 0xAD, 0xBE, 0xEF, 0x47, 0x00]);
    assert!(demuxer.drain().is_empty());

    // Truncated TS packet
    let short = vec![0x47u8; 100];
    demuxer.feed(&short);
    assert!(demuxer.drain().is_empty());

    // All zeros
    demuxer.feed(&[0u8; 188]);
    assert!(demuxer.drain().is_empty());
}

#[test]
fn process_ts_packet_ignores_oversized_adaptation_field_without_state_corruption() {
    let mut demuxer = TsDemuxer::new();
    install_single_h264_stream(&mut demuxer, 0x100);
    assert_eq!(demuxer.streams[0].continuity, CC_UNSET);

    // adaptation_field_control = 0x03 (AF + payload); af_len declares 255
    // bytes, far exceeding the 184 bytes actually available after the 4-byte
    // TS header, so payload_offset overruns the packet.
    let mut pkt = [0xFFu8; TS_PACKET_SIZE];
    pkt[0..4].copy_from_slice(&ts_header_bytes(0x100, false, 0x03, 7));
    pkt[4] = 255;
    demuxer.process_ts_packet(&pkt);

    assert!(
        demuxer.drain().is_empty(),
        "an oversized adaptation field must never yield a media packet"
    );
    assert_eq!(
        demuxer.streams[0].continuity, CC_UNSET,
        "a packet whose adaptation field overruns the TS packet must be ignored \
         entirely, including continuity-counter bookkeeping"
    );
    assert!(demuxer.streams[0].pes.buf.is_empty());

    // A subsequent legitimate PES on the same PID must still demux correctly.
    let good = valid_video_pes_packet(0x100, 0, 900, &[0x00, 0x00, 0x00, 0x01, 0x65]);
    demuxer.process_ts_packet(&good);
    demuxer.flush();
    let packets = demuxer.drain();
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].media_type, MediaType::Video);
    assert_eq!(packets[0].payload.as_ref(), &[0x00, 0x00, 0x00, 0x01, 0x65]);
}

#[test]
fn process_ts_packet_rejects_pes_header_len_overrunning_payload() {
    let mut demuxer = TsDemuxer::new();
    install_single_h264_stream(&mut demuxer, 0x100);

    // PES header claims has_pts+has_dts and a 255-byte optional header, but
    // the TS payload only carries the mandatory 9 bytes plus the 10-byte
    // PTS/DTS pair (19 total): no elementary data can possibly follow such a
    // declared header within this packet.
    let mut pes = vec![0x00, 0x00, 0x01, 0xE0, 0x00, 0x00, 0x80, 0xC0, 0xFF];
    write_timestamp(&mut pes, 900, 0x03);
    write_timestamp(&mut pes, 900, 0x01);
    assert_eq!(pes.len(), 19);
    let pkt = build_ts_packet(0x100, true, 0x01, 0, &[], &pes);

    demuxer.process_ts_packet(&pkt);
    assert!(
        demuxer.drain().is_empty(),
        "a PES header whose declared length overruns the packet must not emit a packet"
    );
    assert!(
        demuxer.streams[0].pes.buf.is_empty(),
        "no elementary data should have been appended past an overrunning header"
    );

    // A subsequent legitimate PES on the same PID must still demux correctly;
    // its payload_unit_start must cleanly flush (and discard, since it never
    // accumulated any bytes) the truncated one rather than emitting garbage.
    let good = valid_video_pes_packet(0x100, 1, 1800, &[0x00, 0x00, 0x00, 0x01, 0x65]);
    demuxer.process_ts_packet(&good);
    demuxer.flush();
    let packets = demuxer.drain();
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].payload.as_ref(), &[0x00, 0x00, 0x00, 0x01, 0x65]);
}

#[test]
fn process_ts_packet_caps_pes_buffer_at_max_size_under_continuation_flood() {
    let mut demuxer = TsDemuxer::new();
    install_single_h264_stream(&mut demuxer, 0x100);

    // Start an unbounded-length video PES (pes_packet_len == 0, the standard
    // encoding for video) carrying a timestamp, then flood it with far more
    // continuation packets than MAX_PES_BUFFER can hold.
    let start = unbounded_video_pes_start_packet(0x100, 0, 900, &[0x00, 0x00, 0x00, 0x01, 0x65]);
    demuxer.process_ts_packet(&start);
    assert!(demuxer.drain().is_empty());

    let filler = [0xABu8; TS_PACKET_SIZE - 4];
    // 512 KiB / 184 bytes/packet =~ 2849 packets to reach the cap; send well
    // past that to prove the cap holds under sustained pressure, not just once.
    for cc in 0..6000u32 {
        let pkt = build_ts_packet(0x100, false, 0x01, (cc & 0x0F) as u8, &[], &filler);
        demuxer.process_ts_packet(&pkt);
    }
    assert!(
        demuxer.drain().is_empty(),
        "an unbounded-length PES never auto-completes without a new payload_unit_start"
    );

    let capped_len = demuxer.streams[0].pes.buf.len();
    assert!(
        capped_len <= MAX_PES_BUFFER,
        "PES accumulator must never exceed the {MAX_PES_BUFFER}-byte cap, got {capped_len}"
    );
    assert!(
        capped_len > MAX_PES_BUFFER - TS_PACKET_SIZE,
        "cap should plateau within one packet payload of the limit, got {capped_len}"
    );

    // A subsequent legitimate PES on the same PID must still demux correctly:
    // its payload_unit_start flushes the capped accumulator (as one
    // oversized-but-bounded packet) and then buffers the new frame cleanly.
    let good = valid_video_pes_packet(0x100, 1, 1800, &[0x00, 0x00, 0x00, 0x01, 0x41]);
    demuxer.process_ts_packet(&good);
    demuxer.flush();

    let packets = demuxer.drain();
    assert_eq!(
        packets.len(),
        2,
        "capped PES flush + the new complete frame"
    );
    assert_eq!(packets[0].payload.len(), capped_len);
    assert!(packets[0].payload.len() <= MAX_PES_BUFFER);
    assert_eq!(packets[1].payload.as_ref(), &[0x00, 0x00, 0x00, 0x01, 0x41]);
}

proptest! {
    #[test]
    fn ts_demuxer_feed_never_panics_on_arbitrary_bytes(
        chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..512), 0..8),
    ) {
        let mut demuxer = TsDemuxer::new();
        for chunk in &chunks {
            demuxer.feed(chunk);
        }
        demuxer.flush();
        let _ = demuxer.drain();
    }

    #[test]
    fn ts_demuxer_feed_caps_pes_buffer_under_arbitrary_ts_packets(
        packets in prop::collection::vec(
            (any::<bool>(), 0u8..4, 0u8..16, prop::collection::vec(any::<u8>(), TS_PACKET_SIZE - 4)),
            0..64,
        ),
    ) {
        let mut demuxer = TsDemuxer::new();
        install_single_h264_stream(&mut demuxer, 0x100);

        let mut data = Vec::with_capacity(packets.len() * TS_PACKET_SIZE);
        for (pusi, afc, cc, body) in &packets {
            data.extend_from_slice(&ts_header_bytes(0x100, *pusi, *afc, *cc));
            data.extend_from_slice(body);
        }
        demuxer.feed(&data);

        prop_assert!(
            demuxer.streams[0].pes.buf.len() <= MAX_PES_BUFFER,
            "PES accumulator exceeded the {MAX_PES_BUFFER}-byte cap under arbitrary packet content"
        );

        demuxer.flush();
        for packet in demuxer.drain() {
            prop_assert!(packet.payload.len() <= MAX_PES_BUFFER);
        }
    }
}

#[test]
fn mux_round_trip() {
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 640,
        height: 360,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: Some("High".to_string()),
        level: Some("3.0".to_string()),
        pixel_format: None,
    };

    let audio = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };

    let mut muxer = TsMuxer::new(Some(&video), &[audio]);

    // Create test packets
    let video_payload = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB, 0xCC]; // IDR NAL
    let audio_payload = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0xDE, 0x02]; // ADTS

    let ts_out1 = muxer.mux_packet(MediaType::Video, 0, 0, 0, true, &video_payload);
    assert!(!ts_out1.is_empty());
    assert_eq!(ts_out1.len() % TS_PACKET_SIZE, 0);

    let ts_out2 = muxer.mux_packet(MediaType::Audio, 0, 0, 0, false, &audio_payload);
    assert!(!ts_out2.is_empty());
    assert_eq!(ts_out2.len() % TS_PACKET_SIZE, 0);
}

#[test]
fn muxer_enforces_strict_dts_when_ms_timestamps_repeat() {
    let audio = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };

    let mut muxer = TsMuxer::new(None, &[audio]);
    let audio_payload = [0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0xDE, 0x02];

    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 0, 1000, 1000, false, &audio_payload)
            .is_empty()
    );
    assert_eq!(muxer.last_dts_90k[0], 90_000);

    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 0, 1000, 1000, false, &audio_payload)
            .is_empty()
    );
    assert_eq!(
        muxer.last_dts_90k[0], 90_001,
        "equal millisecond DTS must not become equal 90 kHz DTS"
    );
}

#[test]
fn muxer_tracks_strict_dts_per_audio_track() {
    let audio0 = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: Some("eng".to_string()),
        title: None,
        profile: None,
    };
    let audio1 = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 1,
        pid: None,
        language: Some("spa".to_string()),
        title: None,
        profile: None,
    };

    let mut muxer = TsMuxer::new(None, &[audio0, audio1]);
    let audio_payload = [0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0xDE, 0x02];

    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 0, 1000, 1000, false, &audio_payload)
            .is_empty()
    );
    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 1, 1000, 1000, false, &audio_payload)
            .is_empty()
    );
    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 0, 1000, 1000, false, &audio_payload)
            .is_empty()
    );

    assert_eq!(muxer.last_dts_90k[0], 90_001);
    assert_eq!(
        muxer.last_dts_90k[1], 90_000,
        "DTS repair must be isolated per elementary stream"
    );
}

#[test]
fn muxer_reserves_internal_adts_frame_timestamps() {
    let audio = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };
    let mut muxer = TsMuxer::new(None, &[audio]);
    let mut two_frame_payload = Vec::new();
    let frame_a = crate::media::codec::build_adts_header(2, 48000, 2);
    two_frame_payload.extend_from_slice(&frame_a);
    two_frame_payload.extend_from_slice(&[0x11, 0x22]);
    let frame_b = crate::media::codec::build_adts_header(2, 48000, 2);
    two_frame_payload.extend_from_slice(&frame_b);
    two_frame_payload.extend_from_slice(&[0x33, 0x44]);

    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 0, 1000, 1000, false, &two_frame_payload)
            .is_empty()
    );
    assert_eq!(
        muxer.last_dts_90k[0], 91_920,
        "two 48 kHz AAC frames occupy the PES start plus one 1024-sample step"
    );

    let mut one_frame_payload = Vec::new();
    let frame = crate::media::codec::build_adts_header(2, 48000, 2);
    one_frame_payload.extend_from_slice(&frame);
    one_frame_payload.extend_from_slice(&[0x55, 0x66]);
    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 0, 1021, 1021, false, &one_frame_payload)
            .is_empty()
    );
    assert_eq!(
        muxer.last_dts_90k[0], 91_921,
        "next PES must start after the previous payload's final internal ADTS frame"
    );
}

#[test]
fn mux_demux_round_trip() {
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 320,
        height: 240,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };

    let audio = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 44100,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };

    let mut muxer = TsMuxer::new(Some(&video), &[audio]);
    let mut all_ts = Vec::new();

    // Mux a few packets
    let video_payload = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84];
    let audio_payload = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC];

    for i in 0..5 {
        let pts = i * 33; // ~30fps
        let ts = muxer.mux_packet(MediaType::Video, 0, pts, pts, i == 0, &video_payload);
        all_ts.extend_from_slice(ts);

        let ts = muxer.mux_packet(MediaType::Audio, 0, pts, pts, false, &audio_payload);
        all_ts.extend_from_slice(ts);
    }

    // Demux it back
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&all_ts);
    demuxer.flush();
    let packets = demuxer.drain();

    let video_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Video)
        .count();
    let audio_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio)
        .count();

    assert_eq!(video_count, 5, "round-trip should preserve video count");
    assert_eq!(audio_count, 5, "round-trip should preserve audio count");

    // First video should be keyframe
    let first_video = packets
        .iter()
        .find(|p| p.media_type == MediaType::Video)
        .unwrap();
    assert!(first_video.is_keyframe, "first video should be keyframe");
}

#[test]
fn h265_demux_requires_irap_for_keyframe_even_with_random_access_bit() {
    let video = VideoMeta {
        codec: "hevc".to_string(),
        width: 320,
        height: 240,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };

    let mut muxer = TsMuxer::new(Some(&video), &[]);
    let trail_r_payload = vec![0x00, 0x00, 0x00, 0x01, 0x02u8, 0x01, 0xDD];
    let ts = muxer.mux_packet(MediaType::Video, 0, 0, 0, true, &trail_r_payload);

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let video_packet = packets
        .iter()
        .find(|packet| packet.media_type == MediaType::Video)
        .expect("demuxed HEVC video packet");

    assert!(
        !video_packet.is_keyframe,
        "HEVC random_access alone must not mark non-IRAP payloads keyframe"
    );
}

#[test]
fn mux_demux_two_audio_tracks_round_trip() {
    // TsMuxer assigns separate PIDs to each audio track.
    // TsDemuxer must recover both with distinct track_index values
    // and correct packet counts.
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 320,
        height: 240,
        fps: 30.0,
        bw: None,
        pid: Some(0x100),
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    let audio0 = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: Some("eng".to_string()),
        title: None,
        profile: None,
    };
    let audio1 = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 44100,
        channels: 1,
        channel_layout: None,
        track_index: 1,
        pid: None,
        language: Some("spa".to_string()),
        title: None,
        profile: None,
    };

    let mut muxer = TsMuxer::new(Some(&video), &[audio0, audio1]);
    let mut all_ts = Vec::new();

    // Probe-ready H.264 access unit (contains SPS/PPS) so the demuxer's
    // metadata-completeness gate can build the probe.
    let (video_payload, _) = first_probe_ready_payloads();
    // ADTS frame for AAC-LC 48 kHz stereo (7-byte header, no CRC)
    let audio0_payload = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0x21, 0x10];
    // ADTS frame for AAC-LC 44.1 kHz mono
    let audio1_payload = vec![0xFF, 0xF1, 0x58, 0x40, 0x02, 0x1F, 0xFC, 0x21, 0x10];

    for i in 0..3u32 {
        let pts = (i as i64) * 33;
        let ts = muxer.mux_packet(MediaType::Video, 0, pts, pts, i == 0, &video_payload);
        all_ts.extend_from_slice(ts);
        let ts = muxer.mux_packet(MediaType::Audio, 0, pts, pts, false, &audio0_payload);
        all_ts.extend_from_slice(ts);
        let ts = muxer.mux_packet(MediaType::Audio, 1, pts, pts, false, &audio1_payload);
        all_ts.extend_from_slice(ts);
    }

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&all_ts);
    demuxer.flush();
    let packets = demuxer.drain();

    let video_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Video)
        .count();
    let audio0_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio && p.track_index == 0)
        .count();
    let audio1_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio && p.track_index == 1)
        .count();

    assert_eq!(video_count, 3, "should demux 3 video packets");
    assert_eq!(audio0_count, 3, "should demux 3 packets for audio track 0");
    assert_eq!(audio1_count, 3, "should demux 3 packets for audio track 1");

    // Verify both audio track indices appear in the demuxed stream
    let audio_track_indices: std::collections::HashSet<u32> = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio)
        .map(|p| p.track_index)
        .collect();
    assert!(
        audio_track_indices.contains(&0),
        "track_index 0 must be present"
    );
    assert!(
        audio_track_indices.contains(&1),
        "track_index 1 must be present"
    );

    let probe = demuxer
        .take_probe()
        .expect("round-trip should produce a probe");
    assert_eq!(probe.video.as_ref().and_then(|v| v.pid), Some(0x100));
    assert_eq!(probe.audio_tracks.len(), 2);
    assert_eq!(probe.audio_tracks[0].pid, Some(0x101));
    assert_eq!(probe.audio_tracks[0].language.as_deref(), Some("eng"));
    assert_eq!(probe.audio_tracks[1].pid, Some(0x102));
    assert_eq!(probe.audio_tracks[1].language.as_deref(), Some("spa"));
}

#[test]
fn mux_demux_32_audio_tracks_spans_pmt_packets() {
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 640,
        height: 360,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    let languages = [
        "eng", "spa", "fra", "deu", "ita", "por", "nld", "swe", "nor", "dan", "fin", "pol", "ces",
        "slk", "hun", "ron", "bul", "ell", "tur", "rus", "ukr", "ara", "heb", "hin", "tam", "tel",
        "jpn", "kor", "zho", "vie", "tha", "ind",
    ];
    let audio_tracks = languages
        .iter()
        .enumerate()
        .map(|(index, language)| AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 1,
            channel_layout: None,
            track_index: index as u32,
            pid: None,
            language: Some((*language).to_string()),
            title: None,
            profile: None,
        })
        .collect::<Vec<_>>();

    let mut muxer = TsMuxer::new(Some(&video), &audio_tracks);
    let mut all_ts = Vec::new();
    // Probe-ready H.264 access unit (contains SPS/PPS) so the demuxer's
    // metadata-completeness gate can build the probe.
    let (video_payload, _) = first_probe_ready_payloads();
    let audio_payload = vec![0xFF, 0xF1, 0x4C, 0x40, 0x02, 0x1F, 0xFC, 0x21, 0x10];

    all_ts.extend_from_slice(muxer.mux_packet(MediaType::Video, 0, 0, 0, true, &video_payload));
    for index in 0..audio_tracks.len() {
        all_ts.extend_from_slice(muxer.mux_packet(
            MediaType::Audio,
            index as u32,
            0,
            0,
            false,
            &audio_payload,
        ));
    }

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&all_ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let probe = demuxer
        .take_probe()
        .expect("32-track round-trip should produce a probe");

    assert_eq!(probe.video.as_ref().and_then(|v| v.pid), Some(0x100));
    assert_eq!(probe.audio_tracks.len(), 32);
    assert_eq!(probe.audio_tracks[0].pid, Some(0x101));
    assert_eq!(probe.audio_tracks[0].language.as_deref(), Some("eng"));
    assert_eq!(probe.audio_tracks[31].pid, Some(0x120));
    assert_eq!(probe.audio_tracks[31].language.as_deref(), Some("ind"));
    assert_eq!(
        packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Audio)
            .map(|packet| packet.track_index)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        32
    );
}

#[test]
fn marker_fixture_probe_recovers_two_audio_tracks() {
    let fixture = crate::test_fixtures::av_marker_transport_fixture("h264", true)
        .unwrap_or_else(|e| panic!("{e}"));
    let ts = std::fs::read(&fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let probe = demuxer
        .take_probe()
        .expect("fixture probe should discover stream metadata");

    assert!(
        probe.video.is_some(),
        "fixture should contain a video stream"
    );
    assert_eq!(probe.video_track_count, 1);
    assert_eq!(
        probe.audio_tracks.len(),
        2,
        "marker fixture should expose two audio tracks"
    );
    assert_eq!(
        probe
            .audio_tracks
            .iter()
            .map(|track| track.track_index)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Audio)
            .map(|packet| packet.track_index)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        2,
        "fixture packets should cover both logical audio tracks"
    );
}

#[test]
fn try_build_probe_waits_for_complete_h264_and_aac_metadata() {
    let (complete_video, complete_audio) = first_probe_ready_payloads();
    let mut demuxer = TsDemuxer::new();
    demuxer.streams = vec![h264_stream_info(0x100), aac_adts_stream_info(0x101, 0)];

    demuxer.try_build_probe(0, &[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80]);
    demuxer.try_build_probe(1, &complete_audio);
    assert!(
        demuxer.take_probe().is_none(),
        "probe must wait for complete video dimensions instead of locking in 0x0 metadata"
    );

    demuxer.try_build_probe(0, &complete_video);
    let probe = demuxer
        .take_probe()
        .expect("probe should finalize once both tracks have complete metadata");
    let video = probe.video.expect("probe should include video metadata");
    assert!(video.width > 0);
    assert!(video.height > 0);
    assert_eq!(probe.audio_tracks.len(), 1);
    assert!(probe.audio_tracks[0].sample_rate > 0);
    assert!(probe.audio_tracks[0].channels > 0);
}

#[test]
fn try_build_probe_keeps_complete_payload_when_later_frames_lack_sps() {
    let (complete_video, complete_audio) = first_probe_ready_payloads();
    let mut demuxer = TsDemuxer::new();
    demuxer.streams = vec![h264_stream_info(0x100), aac_adts_stream_info(0x101, 0)];

    demuxer.try_build_probe(0, &complete_video);
    demuxer.try_build_probe(0, &[0x00, 0x00, 0x00, 0x01, 0x41, 0x9A, 0x00]);
    assert!(demuxer.take_probe().is_none());

    demuxer.try_build_probe(1, &complete_audio);
    let probe = demuxer
        .take_probe()
        .expect("probe must survive non-SPS frames after complete video metadata");
    let video = probe.video.expect("probe should include video metadata");
    assert!(video.width > 0);
    assert!(video.height > 0);
}

#[test]
fn try_build_probe_caches_h264_sequence_header() {
    let (complete_video, complete_audio) = first_probe_ready_payloads();
    let mut demuxer = TsDemuxer::new();
    demuxer.streams = vec![h264_stream_info(0x100), aac_adts_stream_info(0x101, 0)];

    demuxer.try_build_probe(0, &complete_video);
    demuxer.try_build_probe(1, &complete_audio);

    let probe = demuxer
        .take_probe()
        .expect("probe should finalize once both tracks are complete");
    let sequence_header = probe
        .video_sequence_header
        .expect("H.264 probe should synthesize an RTMP startup header");
    assert_eq!(sequence_header[0], 0x17);
    assert_eq!(sequence_header[1], 0x00);
}

#[test]
fn remux_segment_view_splits_video_and_selected_audio() {
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 640,
        height: 360,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    let audio_tracks = (0..16)
        .map(|index| AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: index,
            pid: None,
            language: None,
            title: None,
            profile: None,
        })
        .collect::<Vec<_>>();

    let mut muxer = TsMuxer::new(Some(&video), &audio_tracks);
    let mut source = Vec::new();
    let video_payload = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x88];
    let audio_payload = vec![0xFF, 0xF1, 0x4C, 0x80, 0x02, 0x1F, 0xFC, 0x21, 0x10];

    for frame in 0..3 {
        let pts = frame * 33;
        source.extend_from_slice(muxer.mux_packet(
            MediaType::Video,
            0,
            pts,
            pts,
            frame == 0,
            &video_payload,
        ));
        for track_index in 0..audio_tracks.len() {
            source.extend_from_slice(muxer.mux_packet(
                MediaType::Audio,
                track_index as u32,
                pts,
                pts,
                false,
                &audio_payload,
            ));
        }
    }

    let video_only = remux_segment_view(&source, Some(&video), &audio_tracks, TsSegmentView::Video)
        .expect("video rendition should contain media");
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&video_only);
    demuxer.flush();
    let packets = demuxer.drain();
    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
    );
    assert!(
        packets
            .iter()
            .all(|packet| packet.media_type == MediaType::Video)
    );

    let audio_only = remux_segment_view(
        &source,
        Some(&video),
        &audio_tracks,
        TsSegmentView::Audio(15),
    )
    .expect("audio rendition should contain media");
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&audio_only);
    demuxer.flush();
    let packets = demuxer.drain();
    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Audio)
    );
    assert!(
        packets
            .iter()
            .all(|packet| packet.media_type == MediaType::Audio)
    );
    assert_eq!(
        packets
            .iter()
            .map(|packet| packet.track_index)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        1,
        "audio rendition should expose exactly one logical track"
    );
}

#[derive(Clone, Copy, Debug)]
enum GeneratedVideoCodec {
    H264,
    H265,
}

impl GeneratedVideoCodec {
    fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "hevc",
        }
    }

    fn payload(self, is_keyframe: bool, payload_len: usize, seed: u8) -> Vec<u8> {
        let mut payload = match (self, is_keyframe) {
            (Self::H264, true) => vec![0x00, 0x00, 0x00, 0x01, 0x65],
            (Self::H264, false) => vec![0x00, 0x00, 0x00, 0x01, 0x41],
            (Self::H265, true) => vec![0x00, 0x00, 0x00, 0x01, 0x26, 0x01],
            (Self::H265, false) => vec![0x00, 0x00, 0x00, 0x01, 0x02, 0x01],
        };
        payload.extend((0..payload_len).map(|offset| seed.wrapping_add(offset as u8)));
        payload
    }
}

#[derive(Clone, Debug)]
struct GeneratedMuxPacket {
    media_type: MediaType,
    track_index: u32,
    pts_ms: i64,
    dts_ms: i64,
    is_keyframe: bool,
    payload: Vec<u8>,
}

fn generated_audio_payload(track_index: u32, payload_len: usize, seed: u8) -> Vec<u8> {
    let raw_len = payload_len.max(1);
    let mut payload = Vec::from(crate::media::codec::build_adts_header(raw_len, 48_000, 2));
    payload.extend((0..raw_len).map(|offset| {
        seed.wrapping_add(track_index as u8)
            .wrapping_add(offset as u8)
    }));
    payload
}

fn generated_mux_sequence(
    codec: GeneratedVideoCodec,
    include_video: bool,
    audio_track_count: usize,
    events: Vec<(usize, u8, u8, bool, u8)>,
) -> Vec<GeneratedMuxPacket> {
    let stream_count = usize::from(include_video) + audio_track_count;
    let mut next_dts_by_stream = vec![0_i64; stream_count];
    let mut packets: Vec<GeneratedMuxPacket> = Vec::with_capacity(events.len());

    for (selector, delta_ms, payload_len, keyframe_hint, pts_offset_units) in events {
        let stream_idx = selector % stream_count;
        let dts = next_dts_by_stream[stream_idx] + i64::from(delta_ms % 40);
        next_dts_by_stream[stream_idx] = dts + 1;
        let payload_len = usize::from(payload_len % 97) + 1;
        let pts_offset = i64::from(pts_offset_units % 4) * 8;

        if include_video && stream_idx == 0 {
            let is_keyframe =
                keyframe_hint || packets.iter().all(|p| p.media_type != MediaType::Video);
            packets.push(GeneratedMuxPacket {
                media_type: MediaType::Video,
                track_index: 0,
                pts_ms: dts + pts_offset,
                dts_ms: dts,
                is_keyframe,
                payload: codec.payload(is_keyframe, payload_len, payload_len as u8),
            });
        } else {
            let track_index = if include_video {
                stream_idx - 1
            } else {
                stream_idx
            } as u32;
            packets.push(GeneratedMuxPacket {
                media_type: MediaType::Audio,
                track_index,
                pts_ms: dts,
                dts_ms: dts,
                is_keyframe: false,
                payload: generated_audio_payload(track_index, payload_len, payload_len as u8),
            });
        }
    }

    packets
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn proptest_ts_muxer_demuxer_preserves_stream_invariants(
        codec_choice in any::<bool>(),
        include_video in any::<bool>(),
        audio_track_count in 0usize..=30,
        events in proptest::collection::vec((0usize..64, 0u8..80, 1u8..160, any::<bool>(), 0u8..8), 1..96),
    ) {
        prop_assume!(include_video || audio_track_count > 0);

        let codec = if codec_choice {
            GeneratedVideoCodec::H265
        } else {
            GeneratedVideoCodec::H264
        };
        let video = include_video.then(|| VideoMeta {
            codec: codec.name().to_string(),
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
        });
        let audio_tracks = (0..audio_track_count)
            .map(|track_index| AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48_000,
                channels: 2,
                channel_layout: None,
                track_index: track_index as u32,
                pid: None,
                language: None,
                title: None,
                profile: None,
            })
            .collect::<Vec<_>>();
        let generated = generated_mux_sequence(codec, include_video, audio_track_count, events);

        let mut muxer = TsMuxer::new(video.as_ref(), &audio_tracks);
        let mut ts = Vec::new();
        let mut expected_streams = BTreeSet::new();
        for packet in &generated {
            expected_streams.insert((packet.media_type as u8, packet.track_index));
            ts.extend_from_slice(muxer.mux_packet(
                packet.media_type,
                packet.track_index,
                packet.pts_ms,
                packet.dts_ms,
                packet.is_keyframe,
                &packet.payload,
            ));
        }

        prop_assert!(!ts.is_empty());
        prop_assert_eq!(ts.len() % TS_PACKET_SIZE, 0);
        prop_assert!(ts.chunks_exact(TS_PACKET_SIZE).all(|chunk| chunk[0] == TS_SYNC_BYTE));

        let mut demuxer = TsDemuxer::new();
        demuxer.feed(&ts);
        demuxer.flush();
        let packets = demuxer.drain();

        prop_assert_eq!(packets.len(), generated.len());
        type StreamKey = (u8, u32);
        type ExpectedPacketsByStream = BTreeMap<StreamKey, VecDeque<(Vec<u8>, bool)>>;

        let mut last_dts_by_stream: BTreeMap<StreamKey, i64> = BTreeMap::new();
        let mut seen_streams = BTreeSet::new();
        let mut expected_by_stream: ExpectedPacketsByStream = BTreeMap::new();
        for expected in &generated {
            expected_by_stream
                .entry((expected.media_type as u8, expected.track_index))
                .or_default()
                .push_back((expected.payload.clone(), expected.is_keyframe));
        }

        for actual in &packets {
            let stream_key = (actual.media_type as u8, actual.track_index);
            let Some(expected_queue) = expected_by_stream.get_mut(&stream_key) else {
                prop_assert!(false, "unexpected stream in demux output: {:?}", stream_key);
                unreachable!();
            };
            let Some((expected_payload, expected_keyframe)) = expected_queue.pop_front() else {
                prop_assert!(false, "too many packets for stream {:?}", stream_key);
                unreachable!();
            };

            prop_assert_eq!(actual.payload.as_ref(), expected_payload.as_slice());
            prop_assert_eq!(actual.is_keyframe, expected_keyframe);
            prop_assert!(actual.pts >= actual.dts);

            if let Some(previous_dts) = last_dts_by_stream.insert(stream_key, actual.dts) {
                prop_assert!(
                    actual.dts >= previous_dts,
                    "DTS regressed for {:?}: {} -> {}",
                    stream_key,
                    previous_dts,
                    actual.dts
                );
            }
            seen_streams.insert(stream_key);
        }

        for (stream_key, expected_queue) in expected_by_stream {
            prop_assert!(
                expected_queue.is_empty(),
                "missing demux output packets for stream {:?}",
                stream_key
            );
        }
        prop_assert_eq!(seen_streams, expected_streams);
    }
}

#[test]
fn nal_scanner_h264_idr() {
    // Start code + IDR NAL
    let data = [0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB];
    assert!(h264_is_keyframe(&data));

    // Start code + non-IDR slice
    let data2 = [0x00, 0x00, 0x00, 0x01, 0x41, 0xAA, 0xBB];
    assert!(!h264_is_keyframe(&data2));
}

#[test]
fn h265_irap_detection() {
    // H.265 NAL header: byte0 = forbidden(1b) | nal_unit_type(6b) >> ... encoded as (type << 1)
    // IDR_W_RADL = type 19 → byte0 = (19 << 1) = 0x26, byte1 = 0x01 (layer=0, tid=1)
    // for_each_nal_h265 extracts: (byte0 >> 1) & 0x3F = (0x26 >> 1) & 0x3F = 19 ✓
    let idr_nal = vec![0x00, 0x00, 0x00, 0x01, 0x26u8, 0x01, 0xAA, 0xBB];
    assert!(
        h265_is_keyframe(&idr_nal),
        "IDR_W_RADL (type 19) should be a keyframe"
    );

    // IDR_N_LP = type 20 → byte0 = (20 << 1) = 0x28
    let idr_nlp = vec![0x00, 0x00, 0x00, 0x01, 0x28u8, 0x01, 0xCC];
    assert!(
        h265_is_keyframe(&idr_nlp),
        "IDR_N_LP (type 20) should be a keyframe"
    );

    // Non-IRAP: TRAIL_R = type 1 → byte0 = (1 << 1) = 0x02
    let trail_r = vec![0x00, 0x00, 0x00, 0x01, 0x02u8, 0x01, 0xDD];
    assert!(
        !h265_is_keyframe(&trail_r),
        "TRAIL_R (type 1) should not be a keyframe"
    );

    // CRA_NUT = type 21 → byte0 = (21 << 1) = 0x2A
    // CRA is commonly produced by software encoders (ffmpeg, x265) and hardware
    // encoders. Must be treated as a keyframe for ring-buffer overflow recovery.
    let cra = vec![0x00, 0x00, 0x00, 0x01, 0x2Au8, 0x01, 0xEE];
    assert!(
        h265_is_keyframe(&cra),
        "CRA_NUT (type 21) should be a keyframe"
    );

    // BLA_W_LP = type 16 → byte0 = (16 << 1) = 0x20 (low boundary of IRAP range)
    let bla = vec![0x00, 0x00, 0x00, 0x01, 0x20u8, 0x01, 0xFF];
    assert!(
        h265_is_keyframe(&bla),
        "BLA_W_LP (type 16) should be a keyframe"
    );

    // Type 15 (non-IRAP, just below boundary) → byte0 = (15 << 1) = 0x1E
    let non_irap_below = vec![0x00, 0x00, 0x00, 0x01, 0x1Eu8, 0x01, 0x00];
    assert!(
        !h265_is_keyframe(&non_irap_below),
        "Type 15 is non-IRAP, should not be a keyframe"
    );

    // Type 24 (just above IRAP range) → byte0 = (24 << 1) = 0x30
    let non_irap_above = vec![0x00, 0x00, 0x00, 0x01, 0x30u8, 0x01, 0x00];
    assert!(
        !h265_is_keyframe(&non_irap_above),
        "Type 24 is non-IRAP, should not be a keyframe"
    );
}

// --- NAL scanner edge cases ---

#[test]
fn h264_is_keyframe_empty_payload_returns_false() {
    assert!(!h264_is_keyframe(&[]));
}

#[test]
fn h264_is_keyframe_no_start_codes_returns_false() {
    // Non-Annex B data, no 0x000001 or 0x00000001 start code
    assert!(!h264_is_keyframe(&[0x00, 0x01, 0x65, 0x88]));
}

#[test]
fn h265_is_keyframe_empty_payload_returns_false() {
    assert!(!h265_is_keyframe(&[]));
}

#[test]
fn h265_is_keyframe_no_start_codes_returns_false() {
    assert!(!h265_is_keyframe(&[0x00, 0x01, 0x26, 0x01]));
}

#[test]
fn find_h264_sps_no_sps_nal_returns_none() {
    // IDR slice (nal_type=5), no SPS (nal_type=7) present
    let data = [0x00, 0x00, 0x00, 0x01, 0x65u8, 0xAA, 0xBB];
    assert!(find_h264_sps(&data).is_none());
}

#[test]
fn find_h264_sps_empty_returns_none() {
    assert!(find_h264_sps(&[]).is_none());
}

#[test]
fn find_h264_sps_extracts_sps_nal() {
    // SPS NAL: nal_type=7 (byte & 0x1F == 7)
    // find_h264_sps returns NAL data after the first byte (the header byte)
    let data = [0x00, 0x00, 0x00, 0x01, 0x67u8, 0x64, 0x00, 0x1F];
    let sps = find_h264_sps(&data);
    assert!(sps.is_some(), "SPS NAL type 7 must be found");
    // Returns data after the NAL header byte (0x67)
    assert_eq!(sps.unwrap(), vec![0x64, 0x00, 0x1F]);
}

#[test]
fn find_h265_sps_no_sps_returns_none() {
    // H.265 IDR (nal_type 19, byte0=(19<<1)=0x26), not SPS (nal_type 33)
    let data = [0x00, 0x00, 0x00, 0x01, 0x26u8, 0x01, 0xAA];
    assert!(find_h265_sps(&data).is_none());
}

#[test]
fn find_h265_sps_empty_returns_none() {
    assert!(find_h265_sps(&[]).is_none());
}

#[test]
fn find_h265_sps_extracts_sps_payload() {
    // H.265 SPS: nal_unit_type=33 → byte0=(33<<1)=0x42, byte1=nuh_layer/temporal
    // find_h265_sps returns sps[2..] (skips the 2-byte NAL header)
    let data = [0x00, 0x00, 0x00, 0x01, 0x42u8, 0x01, 0xAA, 0xBB, 0xCC];
    let sps = find_h265_sps(&data);
    assert!(sps.is_some(), "H.265 SPS (type 33) must be found");
    assert_eq!(sps.unwrap(), vec![0xAA, 0xBB, 0xCC]);
}

/// Appends `width` bits of `value` (MSB-first) to a bitstream under
/// construction. Shared by SPS-bitstream builders below.
fn push_bits(bits: &mut Vec<bool>, value: u64, width: u32) {
    for shift in (0..width).rev() {
        bits.push((value >> shift) & 1 == 1);
    }
}

/// Appends an Exp-Golomb `ue(v)` encoding of `value`.
fn push_ue(bits: &mut Vec<bool>, value: u32) {
    let code_num = value as u64 + 1;
    let width = u64::BITS - code_num.leading_zeros();
    bits.extend(std::iter::repeat_n(false, (width - 1) as usize));
    push_bits(bits, code_num, width);
}

/// Packs a bitstream into bytes, zero-padding the final byte.
fn pack_bits(bits: &[bool]) -> Vec<u8> {
    bits.chunks(8)
        .map(|chunk| {
            chunk.iter().enumerate().fold(0u8, |byte, (index, bit)| {
                byte | (u8::from(*bit) << (7 - index))
            })
        })
        .collect()
}

/// Inverse of `remove_emulation_prevention`: inserts a 0x03 byte after any
/// `00 00` run followed by a byte <= 3, so a hand-built RBSP round-trips
/// through the parser's emulation-prevention removal unchanged. Needed
/// because a randomly chosen Exp-Golomb field can incidentally contain a
/// `00 00 0x` sequence, which would otherwise either get silently eaten by
/// `remove_emulation_prevention` (for `00 00 03`) or, worse, be mistaken by
/// the Annex-B start-code scanner for a `00 00 01` NAL boundary and truncate
/// the payload before it ever reaches the SPS parser.
fn insert_emulation_prevention(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zero_run = 0u32;
    for &byte in data {
        if zero_run >= 2 && byte <= 3 {
            out.push(3);
            zero_run = 0;
        }
        out.push(byte);
        zero_run = if byte == 0 { zero_run + 1 } else { 0 };
    }
    out
}

/// Appends a well-formed H.265 `profile_tier_level` + SPS id + chroma format
/// prefix (single sub-layer, Main profile, level 3.0, 4:2:0 chroma).
fn push_h265_profile_prefix(bits: &mut Vec<bool>) {
    push_bits(bits, 0, 4); // sps_video_parameter_set_id
    push_bits(bits, 0, 3); // sps_max_sub_layers_minus1
    push_bits(bits, 1, 1); // sps_temporal_id_nesting_flag
    push_bits(bits, 0, 2); // general_profile_space
    push_bits(bits, 0, 1); // general_tier_flag
    push_bits(bits, 1, 5); // general_profile_idc
    push_bits(bits, 0, 32); // compatibility flags
    push_bits(bits, 0, 48); // constraint flags
    push_bits(bits, 90, 8); // general_level_idc
    push_ue(bits, 0); // sps_seq_parameter_set_id
    push_ue(bits, 1); // chroma_format_idc
}

#[test]
fn malformed_sps_bitstreams_fail_closed_without_partial_metadata() {
    let h264_exp_golomb_shift_overflow = [
        0, 0, 0, 1, 0x67, // Annex B start code and H.264 SPS NAL header
        100, 0, 31, // High profile, compatibility, level
        0, 0, 0, 0, 0x80, // 32 zero prefix bits followed by the stop bit
        0, 0, 0, 0, // 32 suffix bits
    ];

    let mut h265_bits = Vec::new();
    push_h265_profile_prefix(&mut h265_bits);
    push_ue(&mut h265_bits, 0); // pic_width_in_luma_samples
    push_ue(&mut h265_bits, 0); // pic_height_in_luma_samples
    push_bits(&mut h265_bits, 1, 1); // conformance_window_flag
    push_ue(&mut h265_bits, 1); // conf_win_left_offset: larger than width
    push_ue(&mut h265_bits, 0);
    push_ue(&mut h265_bits, 0);
    push_ue(&mut h265_bits, 0);
    let mut h265_crop_underflow = vec![
        0, 0, 0, 1, 0x42, 0x01, // Annex B start code and H.265 SPS NAL header
    ];
    h265_crop_underflow.extend(pack_bits(&h265_bits));

    let mut h265_count_bits = Vec::new();
    push_h265_profile_prefix(&mut h265_count_bits);
    push_ue(&mut h265_count_bits, 1_920);
    push_ue(&mut h265_count_bits, 1_080);
    push_bits(&mut h265_count_bits, 0, 1); // conformance_window_flag
    push_ue(&mut h265_count_bits, 0); // bit_depth_luma_minus8
    push_ue(&mut h265_count_bits, 0); // bit_depth_chroma_minus8
    push_ue(&mut h265_count_bits, 0); // log2_max_pic_order_cnt_lsb_minus4
    push_bits(&mut h265_count_bits, 0, 1); // sub_layer_ordering_info_present
    for _ in 0..9 {
        push_ue(&mut h265_count_bits, 0);
    }
    push_bits(&mut h265_count_bits, 0, 4); // scaling, AMP, SAO, and PCM flags
    push_ue(&mut h265_count_bits, 65); // num_short_term_ref_pic_sets
    let mut h265_unbounded_count = vec![0, 0, 0, 1, 0x42, 0x01];
    h265_unbounded_count.extend(pack_bits(&h265_count_bits));

    let outcomes = [
        (
            "h264-exp-golomb-overflow",
            StreamKind::H264,
            h264_exp_golomb_shift_overflow.as_slice(),
        ),
        (
            "h265-crop-underflow",
            StreamKind::H265,
            h265_crop_underflow.as_slice(),
        ),
        (
            "h265-unbounded-short-term-rps-count",
            StreamKind::H265,
            h265_unbounded_count.as_slice(),
        ),
    ]
    .into_iter()
    .map(|(case, kind, bytes)| {
        (
            case,
            std::panic::catch_unwind(|| probe_video(kind, 0x100, None, None, bytes)),
        )
    })
    .collect::<Vec<_>>();

    for (case, result) in outcomes {
        assert!(result.is_ok(), "{case} panicked");
        let meta = result.expect("probe result");
        assert_eq!(meta.width, 0, "{case} published an invalid width");
        assert_eq!(meta.height, 0, "{case} published an invalid height");
        assert!(meta.profile.is_none(), "{case} published partial metadata");
        assert!(meta.level.is_none(), "{case} published partial metadata");
    }
}

#[test]
fn h264_scaling_matrix_uses_4x4_list_length_before_dimensions() {
    let payload = [
        0, 0, 0, 1, 0x67, // Annex B start code and SPS NAL header
        100, 0, 31, // High profile, compatibility, level
        0xAD, 0xFF, 0xFF, 0x80, 0xF0, 0x50, 0x7E, 0x00,
    ];

    let meta = probe_video(StreamKind::H264, 0x100, None, None, &payload);

    assert_eq!(meta.profile.as_deref(), Some("High"));
    assert_eq!((meta.width, meta.height), (320, 240));
}

proptest! {
    #[test]
    fn probe_video_never_panics(
        h265 in any::<bool>(),
        pes_payload in prop::collection::vec(any::<u8>(), 0..512),
    ) {
        let kind = if h265 { StreamKind::H265 } else { StreamKind::H264 };
        let _ = probe_video(kind, 0x100, None, None, &pes_payload);
    }

    #[test]
    fn probe_video_h264_truncation_never_yields_partial_metadata(
        profile_idc in prop::sample::select(vec![66u8, 77, 88, 100, 110, 122, 244]),
        level_idc in 0u8..255,
        width_mbs_minus1 in 0u32..500,
        height_map_units_minus1 in 0u32..500,
    ) {
        let mut bits = Vec::new();
        push_ue(&mut bits, 0); // seq_parameter_set_id
        if matches!(profile_idc, 100 | 110 | 122 | 244) {
            push_ue(&mut bits, 1); // chroma_format_idc (4:2:0)
            push_ue(&mut bits, 0); // bit_depth_luma_minus8
            push_ue(&mut bits, 0); // bit_depth_chroma_minus8
            push_bits(&mut bits, 0, 1); // qpprime_y_zero_transform_bypass_flag
            push_bits(&mut bits, 0, 1); // seq_scaling_matrix_present_flag
        }
        push_ue(&mut bits, 0); // log2_max_frame_num_minus4
        push_ue(&mut bits, 0); // pic_order_cnt_type
        push_ue(&mut bits, 0); // log2_max_pic_order_cnt_lsb_minus4
        push_ue(&mut bits, 0); // max_num_ref_frames
        push_bits(&mut bits, 0, 1); // gaps_in_frame_num_allowed_flag
        push_ue(&mut bits, width_mbs_minus1);
        push_ue(&mut bits, height_map_units_minus1);
        push_bits(&mut bits, 1, 1); // frame_mbs_only_flag
        push_bits(&mut bits, 0, 1); // direct_8x8_inference_flag
        push_bits(&mut bits, 0, 1); // frame_cropping_flag
        push_bits(&mut bits, 0, 1); // vui_parameters_present_flag

        let mut raw_sps = vec![profile_idc, 0, level_idc];
        raw_sps.extend(pack_bits(&bits));

        let mut payload = vec![0, 0, 0, 1, 0x67];
        payload.extend(insert_emulation_prevention(&raw_sps));

        let expected_width = (width_mbs_minus1 + 1) * 16;
        let expected_height = (height_map_units_minus1 + 1) * 16;

        let meta = probe_video(StreamKind::H264, 0x100, None, None, &payload);
        prop_assert_eq!(meta.width, expected_width);
        prop_assert_eq!(meta.height, expected_height);
        prop_assert!(meta.profile.is_some());
        prop_assert!(meta.level.is_some());

        for cut in 0..payload.len() {
            let partial = probe_video(StreamKind::H264, 0x100, None, None, &payload[..cut]);
            let fully_default = partial.width == 0
                && partial.height == 0
                && partial.profile.is_none()
                && partial.level.is_none();
            prop_assert!(
                fully_default,
                "truncating at byte {cut} of {} must fail closed, got {partial:?}",
                payload.len()
            );
        }
    }

    #[test]
    fn probe_video_h265_truncation_never_yields_partial_metadata(
        width in 16u32..4096,
        height in 16u32..2160,
    ) {
        let mut bits = Vec::new();
        push_h265_profile_prefix(&mut bits);
        push_ue(&mut bits, width); // pic_width_in_luma_samples
        push_ue(&mut bits, height); // pic_height_in_luma_samples
        push_bits(&mut bits, 0, 1); // conformance_window_flag
        push_ue(&mut bits, 0); // bit_depth_luma_minus8
        push_ue(&mut bits, 0); // bit_depth_chroma_minus8
        push_ue(&mut bits, 0); // log2_max_pic_order_cnt_lsb_minus4
        push_bits(&mut bits, 1, 1); // sps_sub_layer_ordering_info_present_flag
        push_ue(&mut bits, 0); // sps_max_dec_pic_buffering_minus1[0]
        push_ue(&mut bits, 0); // sps_max_num_reorder_pics[0]
        push_ue(&mut bits, 0); // sps_max_latency_increase_plus1[0]
        push_ue(&mut bits, 0); // log2_min_luma_coding_block_size_minus3
        push_ue(&mut bits, 0); // log2_diff_max_min_luma_coding_block_size
        push_ue(&mut bits, 0); // log2_min_luma_transform_block_size_minus2
        push_ue(&mut bits, 0); // log2_diff_max_min_luma_transform_block_size
        push_ue(&mut bits, 0); // max_transform_hierarchy_depth_inter
        push_ue(&mut bits, 0); // max_transform_hierarchy_depth_intra
        push_bits(&mut bits, 0, 1); // scaling_list_enabled_flag
        push_bits(&mut bits, 0, 1); // amp_enabled_flag
        push_bits(&mut bits, 0, 1); // sample_adaptive_offset_enabled_flag
        push_bits(&mut bits, 0, 1); // pcm_enabled_flag
        push_ue(&mut bits, 0); // num_short_term_ref_pic_sets
        push_bits(&mut bits, 0, 1); // long_term_ref_pics_present_flag
        push_bits(&mut bits, 0, 1); // sps_temporal_mvp_enabled_flag
        push_bits(&mut bits, 0, 1); // strong_intra_smoothing_enabled_flag
        push_bits(&mut bits, 0, 1); // vui_parameters_present_flag

        let mut raw_sps = vec![0x42u8, 0x01];
        raw_sps.extend(pack_bits(&bits));

        let mut payload = vec![0, 0, 0, 1];
        payload.extend(insert_emulation_prevention(&raw_sps));

        let meta = probe_video(StreamKind::H265, 0x100, None, None, &payload);
        prop_assert_eq!(meta.width, width);
        prop_assert_eq!(meta.height, height);
        prop_assert!(meta.profile.is_some());

        for cut in 0..payload.len() {
            let partial = probe_video(StreamKind::H265, 0x100, None, None, &payload[..cut]);
            let fully_default = partial.width == 0
                && partial.height == 0
                && partial.profile.is_none()
                && partial.level.is_none();
            prop_assert!(
                fully_default,
                "truncating at byte {cut} of {} must fail closed, got {partial:?}",
                payload.len()
            );
        }
    }
}

#[test]
fn mux_audio_only_does_not_panic() {
    // TsMuxer with no video track, one audio track.
    // This path is used when a pipeline receives audio-only ingest.
    let audio = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };
    let mut muxer = TsMuxer::new(None, &[audio]);
    let adts = [0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0x21, 0x10];
    let ts = muxer.mux_packet(MediaType::Audio, 0, 1000, 1000, false, &adts);
    assert!(!ts.is_empty(), "audio-only muxer must produce TS packets");
    assert_eq!(
        ts.len() % TS_PACKET_SIZE,
        0,
        "output must be aligned to TS packet size"
    );

    // Demux round-trip: audio packets must survive
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let audio_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio)
        .count();
    assert!(
        audio_count > 0,
        "audio-only round-trip must produce audio packets"
    );
    let video_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Video)
        .count();
    assert_eq!(
        video_count, 0,
        "audio-only stream must contain no video packets"
    );
}

#[test]
fn pat_pmt_parsing() {
    // Build a minimal PAT + PMT
    let mut ts_data = Vec::new();

    // PAT packet
    let mut pat_pkt = [0xFFu8; 188];
    pat_pkt[0] = 0x47;
    pat_pkt[1] = 0x40; // PUSI, PID=0
    pat_pkt[2] = 0x00;
    pat_pkt[3] = 0x10; // payload only, CC=0
    pat_pkt[4] = 0x00; // pointer
    pat_pkt[5] = 0x00; // table_id = PAT
    pat_pkt[6] = 0xB0;
    pat_pkt[7] = 13; // section_length
    pat_pkt[8] = 0x00;
    pat_pkt[9] = 0x01; // TSID
    pat_pkt[10] = 0xC1; // version
    pat_pkt[11] = 0x00;
    pat_pkt[12] = 0x00;
    // Program 1 → PMT PID 0x1000
    pat_pkt[13] = 0x00;
    pat_pkt[14] = 0x01;
    pat_pkt[15] = 0xF0;
    pat_pkt[16] = 0x00;
    let crc = crc32_mpeg2(&pat_pkt[5..17]);
    pat_pkt[17] = (crc >> 24) as u8;
    pat_pkt[18] = (crc >> 16) as u8;
    pat_pkt[19] = (crc >> 8) as u8;
    pat_pkt[20] = crc as u8;
    ts_data.extend_from_slice(&pat_pkt);

    // PMT packet (1 video + 1 audio)
    let mut pmt_pkt = [0xFFu8; 188];
    pmt_pkt[0] = 0x47;
    pmt_pkt[1] = 0x50; // PUSI, PID=0x1000
    pmt_pkt[2] = 0x00;
    pmt_pkt[3] = 0x10;
    pmt_pkt[4] = 0x00;
    pmt_pkt[5] = 0x02; // table_id = PMT
    let section_len = 9 + 10 + 4; // 9 fixed + 2 streams — 5 + CRC
    pmt_pkt[6] = 0xB0;
    pmt_pkt[7] = section_len as u8;
    pmt_pkt[8] = 0x00;
    pmt_pkt[9] = 0x01;
    pmt_pkt[10] = 0xC1;
    pmt_pkt[11] = 0x00;
    pmt_pkt[12] = 0x00;
    pmt_pkt[13] = 0xE1;
    pmt_pkt[14] = 0x00; // PCR PID = 0x100
    pmt_pkt[15] = 0xF0;
    pmt_pkt[16] = 0x00; // program_info_length = 0
    // Video: H.264, PID=0x100
    pmt_pkt[17] = 0x1B;
    pmt_pkt[18] = 0xE1;
    pmt_pkt[19] = 0x00;
    pmt_pkt[20] = 0xF0;
    pmt_pkt[21] = 0x00;
    // Audio: AAC, PID=0x101
    pmt_pkt[22] = 0x0F;
    pmt_pkt[23] = 0xE1;
    pmt_pkt[24] = 0x01;
    pmt_pkt[25] = 0xF0;
    pmt_pkt[26] = 0x00;
    let crc2 = crc32_mpeg2(&pmt_pkt[5..27]);
    pmt_pkt[27] = (crc2 >> 24) as u8;
    pmt_pkt[28] = (crc2 >> 16) as u8;
    pmt_pkt[29] = (crc2 >> 8) as u8;
    pmt_pkt[30] = crc2 as u8;
    ts_data.extend_from_slice(&pmt_pkt);

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&ts_data);

    assert!(demuxer.has_streams());
    assert_eq!(demuxer.streams.len(), 2);
    assert_eq!(demuxer.streams[0].kind, StreamKind::H264);
    assert_eq!(demuxer.streams[0].pid, 0x100);
    assert_eq!(demuxer.streams[1].kind, StreamKind::AacAdts);
    assert_eq!(demuxer.streams[1].pid, 0x101);
}

#[test]
fn adts_probe() {
    // Valid ADTS header: 48kHz, mono
    let adts = [0xFF, 0xF1, 0x4C, 0x40, 0x02, 0x1F, 0xFC];
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &adts);
    assert_eq!(meta.sample_rate, 48000);
    assert_eq!(meta.channels, 1);
}

#[test]
fn adts_probe_boundary_and_malformed_inputs() {
    // Empty payload must not panic and must leave metadata at its unparsed default.
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &[]);
    assert_eq!(meta.sample_rate, 0);
    assert_eq!(meta.channels, 0);
    assert!(!audio_meta_complete(StreamKind::AacAdts, &meta));

    // One byte short of the 7-byte ADTS fixed header: the length guard must
    // reject it even though the sync word and rate/channel bits look valid.
    let short = [0xFF, 0xF1, 0x4C, 0x40, 0x02, 0x1F];
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &short);
    assert_eq!(meta.sample_rate, 0);
    assert_eq!(meta.channels, 0);
    assert!(!audio_meta_complete(StreamKind::AacAdts, &meta));

    // Sync word mismatch (second byte's top nibble isn't 0xF): must not be
    // parsed as ADTS even with an otherwise 7+ byte payload.
    let bad_sync = [0xFF, 0x00, 0x4C, 0x40, 0x02, 0x1F, 0xFC];
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &bad_sync);
    assert_eq!(meta.sample_rate, 0);
    assert_eq!(meta.channels, 0);
    assert_eq!(meta.profile, None);

    // sample_rate_idx = 13 is reserved (only 0..=12 are defined rates): must
    // leave sample_rate at 0 (incomplete), not panic or index out of bounds.
    let reserved_rate = [0xFF, 0xF1, 0x34, 0x00, 0x02, 0x1F, 0xFC];
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &reserved_rate);
    assert_eq!(
        meta.sample_rate, 0,
        "reserved sample rate index must not map to a rate"
    );
    assert_eq!(meta.profile, Some("Main".to_string()));
    assert!(!audio_meta_complete(StreamKind::AacAdts, &meta));

    // channel_config == 7 is the "8 channels" special case per the ADTS spec.
    let eight_channel = [0xFF, 0xF1, 0x4D, 0xC0, 0x02, 0x1F, 0xFC];
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &eight_channel);
    assert_eq!(meta.channels, 8, "channel_config 7 must map to 8 channels");
    assert!(audio_meta_complete(StreamKind::AacAdts, &meta));
}

// --- Helpers shared by PMT version tests ---

/// Build a 188-byte TS PAT packet pointing to PMT PID 0x1000.
#[path = "mpegts_tests/tables_and_sync.rs"]
mod tables_and_sync;
