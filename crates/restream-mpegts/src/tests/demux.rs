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


/// A TS audio PES that carries three ADTS frames becomes three packets, one
/// per frame, with the timestamps stepped by 1024 samples. Consumers that take
/// one access unit per unit — RTMP's audio messages, the fMP4 packager's
/// samples — depend on that: otherwise one unit holds N frames, with N−1 ADTS
/// headers inside it.
#[test]
fn a_multi_frame_audio_pes_becomes_one_packet_per_adts_frame() {
    let pid = 0x0101;
    let mut demuxer = TsDemuxer::new();
    demuxer.streams = vec![aac_adts_stream_info(pid, 0)];
    demuxer.pid_to_stream[pid as usize] = 0;

    let frame = |fill: u8| {
        let mut frame = restream_core::build_adts_header(40, 48_000, 2)
            .expect("a 40-byte payload fits one ADTS frame")
            .to_vec();
        frame.extend(std::iter::repeat_n(fill, 40));
        frame
    };
    let mut payload = Vec::new();
    for fill in [0x11u8, 0x22, 0x33] {
        payload.extend_from_slice(&frame(fill));
    }
    let packet = valid_audio_pes_packet(pid, 0, 90_000, &payload);
    demuxer.feed(&packet);
    demuxer.flush();
    let packets = demuxer.drain();

    assert_eq!(packets.len(), 3, "one packet per ADTS frame");
    assert_eq!(
        packets[0].dts, 1000,
        "the first frame keeps the PES timestamp"
    );
    for (index, packet) in packets.iter().enumerate() {
        assert_eq!(packet.media_type, MediaType::Audio);
        assert_eq!(packet.payload.len(), 47, "one frame: header plus 40 bytes");
        assert_eq!(packet.payload[0], 0xFF, "each unit starts at a frame header");
        assert_eq!(packet.pts, packet.dts);
        if index > 0 {
            // 1024 samples at 48 kHz is 21.33 ms, so 21 or 22 after rounding.
            let delta = packet.dts - packets[index - 1].dts;
            assert!(
                (21..=22).contains(&delta),
                "frame {index} steps {delta} ms, not 1024 samples"
            );
        }
    }
}

/// The split's one rule: a PES that is exactly one frame, or names no known
/// sample rate, is forwarded as it is (a rate-less split would give every
/// frame one timestamp); any other ADTS PES becomes its complete frames, and a
/// partial frame at its end waits for the next PES, so one the stream never
/// completes is dropped, whether one or several frames precede it.
#[test]
fn an_audio_pes_is_split_by_one_rule() {
    let pid = 0x0101;
    let frame = |fill: u8| {
        let mut frame = restream_core::build_adts_header(40, 48_000, 2)
            .expect("a 40-byte payload fits one ADTS frame")
            .to_vec();
        frame.extend(std::iter::repeat_n(fill, 40));
        frame
    };
    let demux = |payload: &[u8]| {
        let mut demuxer = TsDemuxer::new();
        demuxer.streams = vec![aac_adts_stream_info(pid, 0)];
        demuxer.pid_to_stream[pid as usize] = 0;
        demuxer.feed(&valid_audio_pes_packet(pid, 0, 90_000, payload));
        demuxer.flush();
        demuxer
            .drain()
            .into_iter()
            .map(|packet| packet.payload.len())
            .collect::<Vec<_>>()
    };

    assert_eq!(demux(&frame(0x11)), vec![47], "one exact frame passes as it is");

    let mut one_and_a_tail = frame(0x11);
    one_and_a_tail.extend_from_slice(&frame(0x22)[..20]);
    assert_eq!(
        demux(&one_and_a_tail),
        vec![47],
        "a partial frame after one complete frame is dropped"
    );

    let mut two_and_a_tail = frame(0x11);
    two_and_a_tail.extend_from_slice(&frame(0x22));
    two_and_a_tail.extend_from_slice(&frame(0x33)[..20]);
    assert_eq!(
        demux(&two_and_a_tail),
        vec![47, 47],
        "and after several, by the same rule"
    );

    // Sampling-frequency index 13 is reserved: the rate is unknown.
    let mut no_rate = frame(0x11);
    no_rate.extend_from_slice(&frame(0x22));
    for header in [0, 47] {
        no_rate[header + 2] = (no_rate[header + 2] & !0x3C) | (13 << 2);
    }
    assert_eq!(
        demux(&no_rate),
        vec![94],
        "a PES with no known rate is forwarded whole, not split onto one timestamp"
    );
}

/// One AAC frame: an ADTS header and `len` bytes of `fill`.
fn adts_frame(len: usize, fill: u8) -> Vec<u8> {
    let mut frame = restream_core::build_adts_header(len, 48_000, 2)
        .expect("the payload fits one ADTS frame")
        .to_vec();
    frame.extend(std::iter::repeat_n(fill, len));
    frame
}

/// An audio PES in one TS packet, with a PTS or (when no frame starts in it,
/// or its muxer left it out) without.
fn audio_pes(pid: u16, cc: u8, pts_90k: Option<i64>, es_payload: &[u8]) -> [u8; TS_PACKET_SIZE] {
    let header_len: u8 = if pts_90k.is_some() { 5 } else { 0 };
    let pes_packet_len = 3 + u16::from(header_len) + es_payload.len() as u16;
    let mut pes = vec![0x00, 0x00, 0x01, 0xC0];
    pes.extend_from_slice(&pes_packet_len.to_be_bytes());
    pes.push(0x80);
    pes.push(if pts_90k.is_some() { 0x80 } else { 0x00 });
    pes.push(header_len);
    if let Some(pts) = pts_90k {
        write_timestamp(&mut pes, pts, 0x02);
    }
    pes.extend_from_slice(es_payload);
    build_ts_packet(pid, true, 0x01, cc, &[], &pes)
}

/// Demux `pieces` (each a PES payload with its PTS, if any) as one AAC stream.
fn demux_audio_pes(pieces: &[(Option<i64>, Vec<u8>)]) -> Vec<restream_core::MediaPacket> {
    let pid = 0x0101;
    let mut demuxer = TsDemuxer::new();
    demuxer.streams = vec![aac_adts_stream_info(pid, 0)];
    demuxer.pid_to_stream[pid as usize] = 0;
    for (index, (pts, payload)) in pieces.iter().enumerate() {
        demuxer.feed(&audio_pes(pid, (index % 16) as u8, *pts, payload));
    }
    demuxer.flush();
    demuxer.drain()
}

/// 1024 samples at 48 kHz, in 90 kHz ticks.
const STEP_90K: i64 = 1024 * 90_000 / 48_000;

/// A muxer that does not align audio PES to frames (`data_alignment_indicator`
/// 0) cuts a frame across two PES: the next PES completes it, so every frame
/// arrives once, whole. The carried frame keeps the time it started with, and
/// the PES timestamp belongs to the first frame that starts in the PES (here
/// 10 ms late, as after a gap in the source).
#[test]
fn a_frame_cut_across_two_pes_is_completed_by_the_next() {
    let frames = [adts_frame(40, 0x11), adts_frame(40, 0x22), adts_frame(40, 0x33)];
    let mut first = frames[0].clone();
    first.extend_from_slice(&frames[1][..20]);
    let mut second = frames[1][20..].to_vec();
    second.extend_from_slice(&frames[2]);

    let late = 90_000 + 2 * STEP_90K + 900;
    let packets = demux_audio_pes(&[(Some(90_000), first), (Some(late), second)]);

    let payloads: Vec<&[u8]> = packets.iter().map(|packet| &packet.payload[..]).collect();
    assert_eq!(payloads, frames.iter().map(Vec::as_slice).collect::<Vec<_>>());
    let dts: Vec<i64> = packets.iter().map(|packet| packet.dts).collect();
    let expected = vec![ts_to_ms(90_000), ts_to_ms(90_000 + STEP_90K), ts_to_ms(late)];
    assert_eq!(dts, expected, "each frame keeps its own time");
}

/// A frame whose rest was lost (dropped TS packets) never swallows what comes
/// next: a PES that starts with a frame of its own is framed on its own, and
/// one that starts mid-frame resyncs on its next frame. Only the cut frame
/// (and what was lost with it) is gone.
#[test]
fn a_cut_frame_whose_rest_was_lost_does_not_swallow_the_next_pes() {
    let frames = [
        adts_frame(40, 0x11),
        adts_frame(40, 0x22),
        adts_frame(3, 0x33),
        adts_frame(40, 0x44),
        adts_frame(40, 0x55),
    ];
    let payloads = |pieces: &[(Option<i64>, Vec<u8>)]| -> Vec<Vec<u8>> {
        demux_audio_pes(pieces)
            .iter()
            .map(|packet| packet.payload.to_vec())
            .collect()
    };
    let mut cut = frames[0].clone();
    cut.extend_from_slice(&frames[1][..12]);

    // The next PES is aligned, and its frame is shorter than the cut one.
    assert_eq!(
        payloads(&[
            (Some(90_000), cut.clone()),
            (Some(90_000 + 2 * STEP_90K), frames[2].clone()),
        ]),
        vec![frames[0].clone(), frames[2].clone()]
    );

    // The next PES starts inside a later frame: joined with the cut frame it
    // would be long enough, but no frame starts where that one would end.
    let mut after_loss = frames[3][20..].to_vec();
    after_loss.extend_from_slice(&frames[4]);
    assert_eq!(
        payloads(&[
            (Some(90_000), cut),
            (Some(90_000 + 4 * STEP_90K), after_loss),
        ]),
        vec![frames[0].clone(), frames[4].clone()]
    );
}

/// A dropped carried frame takes its time with it: when the next PES has no
/// PTS, its frames wait for the next PES that has one rather than taking the
/// lost frame's slot (an unknown number of frames went with the loss).
#[test]
fn a_dropped_carry_leaves_no_time_for_a_pes_without_a_pts() {
    let frames: Vec<Vec<u8>> = (0..6).map(|index| adts_frame(40, 0x11 * (index + 1))).collect();
    let mut cut = frames[0].clone();
    cut.extend_from_slice(&frames[1][..12]);
    let mut after_loss = frames[3][20..].to_vec();
    after_loss.extend_from_slice(&frames[4]);
    let resumed = 90_000 + 5 * STEP_90K;

    let packets = demux_audio_pes(&[
        (Some(90_000), cut),
        (None, after_loss),
        (Some(resumed), frames[5].clone()),
    ]);

    let payloads: Vec<&[u8]> = packets.iter().map(|packet| &packet.payload[..]).collect();
    assert_eq!(payloads, vec![&frames[0][..], &frames[5][..]]);
    assert_eq!(packets[1].dts, ts_to_ms(resumed));
}

/// A stream joined mid-frame resyncs on a header whose frame ends where the
/// next one starts: AAC payload bytes that happen to read as a header (a
/// stray 0xFFF sync word) are not taken for a frame.
#[test]
fn a_stray_sync_word_inside_a_frame_is_not_taken_for_one() {
    // A header naming a 9-byte frame, inside the first frame's payload.
    let fake = [0xFF, 0xF1, 0x50, 0x80, 0x01, 0x3F, 0xFC];
    let mut body = vec![0x11; 10];
    body.extend_from_slice(&fake);
    body.extend_from_slice(&[0x11; 10]);
    let mut first = restream_core::build_adts_header(body.len(), 48_000, 2)
        .expect("the payload fits one ADTS frame")
        .to_vec();
    first.extend_from_slice(&body);
    let next = adts_frame(40, 0x22);

    let mut joined = first[12..].to_vec();
    joined.extend_from_slice(&next);
    let packets = demux_audio_pes(&[(Some(90_000), joined)]);

    let payloads: Vec<&[u8]> = packets.iter().map(|packet| &packet.payload[..]).collect();
    assert_eq!(payloads, vec![&next[..]], "only the real frame comes out");
}

proptest! {
    /// Whatever the PES cuts, every frame comes out once, whole, in order,
    /// with its own time; a stream joined mid-frame starts at the next frame.
    #[test]
    fn random_pes_cuts_yield_every_frame_once(
        lens in proptest::collection::vec(1usize..60, 1..24),
        cuts in proptest::collection::vec(1usize..150, 1..64),
        joined_mid_frame in any::<bool>(),
    ) {
        // Payload bytes never 0xFF, so only real headers carry a sync word.
        let frames: Vec<Vec<u8>> = lens
            .iter()
            .enumerate()
            .map(|(index, len)| adts_frame(*len, (index % 200) as u8))
            .collect();
        let starts: Vec<usize> = frames
            .iter()
            .scan(0, |at, frame| {
                let start = *at;
                *at += frame.len();
                Some(start)
            })
            .collect();
        let stream: Vec<u8> = frames.concat();
        let skip = if joined_mid_frame { frames[0].len() / 2 } else { 0 };

        let mut pieces = Vec::new();
        let mut at = skip;
        let mut cut = cuts.iter().cycle();
        while at < stream.len() {
            let end = (at + cut.next().copied().unwrap_or(1)).min(stream.len());
            // The PTS of the first frame that starts in the piece, if one does.
            let pts = starts
                .iter()
                .position(|&start| start >= at && start < end)
                .map(|index| index as i64 * STEP_90K);
            pieces.push((pts, stream[at..end].to_vec()));
            at = end;
        }

        let packets = demux_audio_pes(&pieces);
        let expected = if joined_mid_frame { &frames[1..] } else { &frames[..] };
        prop_assert_eq!(packets.len(), expected.len());
        let first = frames.len() - expected.len();
        for (offset, (packet, frame)) in packets.iter().zip(expected).enumerate() {
            let index = (first + offset) as i64;
            prop_assert_eq!(packet.payload[0], 0xFF, "every unit starts at a sync word");
            prop_assert_eq!(&packet.payload[..], &frame[..]);
            prop_assert_eq!(packet.dts, ts_to_ms(index * STEP_90K));
        }
    }
}

/// A feed that ends mid-packet keeps exactly that tail, and the next feed
/// joins and consumes it. The root allocation guard
/// (`restream_engine::tests::mpegts_allocation`) measures this same path; these state
/// transitions prove each measured iteration really enters and clears it.
#[test]
fn continuing_a_partial_packet_stages_then_clears_the_tail() {
    let partial = [TS_SYNC_BYTE; 100];
    let body = vec![0xFFu8; 64 * 1024];
    let mut demuxer = TsDemuxer::new();
    for _ in 0..4 {
        demuxer.feed(&partial);
        assert_eq!(demuxer.remainder.len(), partial.len());
        demuxer.feed(&body);
        assert!(demuxer.remainder.is_empty());
    }
}

