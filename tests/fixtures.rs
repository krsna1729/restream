//! Contract tests for checked-in media fixtures.
//! This file owns the canonical fixture lookup guarantees that higher-level
//! correctness and integration tests depend on.

#[test]
fn checked_in_fixture_contract_is_satisfied() {
    for relative_path in restream::test_fixtures::REQUIRED_CHECKED_IN_FIXTURES {
        let path = restream::test_fixtures::checked_in_fixture(relative_path)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            path.is_file(),
            "fixture contract path must exist: {}",
            path.display()
        );
    }
}

#[test]
fn canonical_transport_fixtures_resolve() {
    let h264 =
        restream::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let h265 =
        restream::test_fixtures::canonical_h265_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let sparse =
        restream::test_fixtures::sparse_gop_mp4_fixture().unwrap_or_else(|e| panic!("{e}"));

    assert!(h264.ends_with("test/fixtures/correctness-h264.ts"));
    assert!(h265.ends_with("test/fixtures/correctness-h265.ts"));
    assert!(sparse.ends_with("test/fixtures/sparse-gop-5s.mp4"));
}

#[test]
fn bf0_marker_transport_fixtures_demux_into_packets() {
    use restream::media::mpegts::TsDemuxer;
    use restream::test_fixtures::{AvMarkerBframeMode, av_marker_transport_fixture_for_bframes};

    for (codec, multi_audio) in [
        ("h264", false),
        ("h264", true),
        ("h265", false),
        ("h265", true),
    ] {
        let path =
            av_marker_transport_fixture_for_bframes(codec, multi_audio, AvMarkerBframeMode::Bf0)
                .unwrap_or_else(|e| panic!("{e}"));
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let mut demuxer = TsDemuxer::new();
        let mut packets = Vec::new();

        for chunk in bytes.chunks(1316) {
            demuxer.feed(chunk);
            demuxer.drain_into(&mut packets);
        }
        demuxer.flush();
        demuxer.drain_into(&mut packets);

        assert!(
            !packets.is_empty(),
            "{} should demux into media packets",
            path.display()
        );
    }
}

#[test]
fn h264_bf0_marker_probe_emits_startup_sequence_header() {
    use restream::media::mpegts::TsDemuxer;
    use restream::test_fixtures::{AvMarkerBframeMode, av_marker_transport_fixture_for_bframes};

    let path = av_marker_transport_fixture_for_bframes("h264", false, AvMarkerBframeMode::Bf0)
        .unwrap_or_else(|e| panic!("{e}"));
    let bytes =
        std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let mut demuxer = TsDemuxer::new();
    let mut probe = None;

    for chunk in bytes.chunks(1316) {
        demuxer.feed(chunk);
        probe = demuxer.take_probe();
        if probe.is_some() {
            break;
        }
    }

    let probe = probe.unwrap_or_else(|| panic!("{} should yield a demux probe", path.display()));
    assert!(
        probe.video.is_some(),
        "{} should yield video metadata in the demux probe",
        path.display()
    );
    assert!(
        probe.video_sequence_header.is_some(),
        "{} should yield an H.264 startup sequence header in the demux probe",
        path.display()
    );
}
