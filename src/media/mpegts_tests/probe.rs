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
