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

