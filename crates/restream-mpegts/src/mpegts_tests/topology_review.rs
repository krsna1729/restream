use super::tables_and_sync::{
    fixture_pid_packets_as, fixture_probe_demux, make_pat_ts_pkt, make_pmt_ts_pkt,
    make_pmt_ts_pkt_with_descriptors, next_pmt_version,
};
use super::*;

#[test]
fn q098_review_coalesced_audio_index_churn_discards_old_identity() {
    let (a_pid, b_pid) = (0x101, 0x102);
    let a = adts_frame(40, 0xAA);
    let b = adts_frame(40, 0xBB);
    let streams = [(0x0F, a_pid), (0x0F, b_pid)];
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&make_pat_ts_pkt());
    demuxer.feed(&make_pmt_ts_pkt(0, &streams));
    demuxer.feed(&valid_audio_pes_packet(a_pid, 0, 0, &a));
    demuxer.feed(&valid_audio_pes_packet(b_pid, 0, 0, &b));
    let initial = demuxer.take_probe().expect("both audio configs completed");
    demuxer.drain();
    demuxer.take_topology_changes();

    // This entire A0/B1 -> B0 -> A0/B1 transition arrives in ONE receive.
    let mut coalesced = make_pmt_ts_pkt(1, &[(0x0F, b_pid)]);
    coalesced.extend_from_slice(&valid_audio_pes_packet(b_pid, 1, 90_000, &b));
    coalesced.extend_from_slice(&make_pmt_ts_pkt(2, &streams));
    coalesced.extend_from_slice(&valid_audio_pes_packet(a_pid, 1, 180_000, &a));
    demuxer.feed(&coalesced);

    let changes = demuxer.take_topology_changes();
    assert!(
        changes.audio,
        "intermediate reindexing must survive coalescing"
    );
    assert!(!changes.video);
    let restored = demuxer
        .take_probe()
        .expect("A config restores the full snapshot");
    assert_eq!(
        restored
            .audio_tracks
            .iter()
            .map(|a| (a.pid, a.track_index))
            .collect::<Vec<_>>(),
        initial
            .audio_tracks
            .iter()
            .map(|a| (a.pid, a.track_index))
            .collect::<Vec<_>>()
    );
    let packets = demuxer.drain();
    assert_eq!(
        packets.len(),
        1,
        "B/index-0 must be discarded at the second PMT"
    );
    assert_eq!(packets[0].media_type, MediaType::Audio);
    assert_eq!(packets[0].track_index, 0);
    assert_eq!(packets[0].payload.as_ref(), a);
    assert_eq!(packets[0].pts, 2000);
    let cleared = demuxer.take_topology_changes();
    assert!(!cleared.audio && !cleared.video, "taking flags clears them");
}

#[test]
fn q098_review_coalesced_video_pid_and_codec_replacement_invalidates() {
    let hevc_fixture =
        crate::test_fixtures::canonical_h265_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let hevc_ts = std::fs::read(&hevc_fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", hevc_fixture.display()));
    let mut hevc_demux = TsDemuxer::new();
    hevc_demux.feed(&hevc_ts);
    hevc_demux.flush();
    let hevc_probe = hevc_demux
        .take_probe()
        .expect("HEVC fixture completes its probe");
    let hevc_pid = hevc_probe
        .video
        .as_ref()
        .and_then(|v| v.pid)
        .expect("HEVC PID");

    // Exercise PID-only, kind-only, and combined identity replacement.
    for (replace_pid, replace_kind) in [(true, false), (false, true), (true, true)] {
        let (mut demuxer, initial, h264_ts) = fixture_probe_demux(false);
        let video_pid = initial
            .video
            .as_ref()
            .and_then(|v| v.pid)
            .expect("video PID");
        let audio_pid = initial.audio_tracks[0].pid.expect("audio PID");
        let original = [(0x1B, video_pid), (0x0F, audio_pid)];
        let version = next_pmt_version(&demuxer);
        demuxer.feed(&make_pmt_ts_pkt(version, &original));
        let initial = demuxer.take_probe().unwrap_or(initial);
        demuxer.take_topology_changes();
        let replacement_pid = if replace_pid { 0x120 } else { video_pid };
        let replacement_kind = if replace_kind { 0x24 } else { 0x1B };
        let (replacement_ts, source_pid) = if replace_kind {
            (&hevc_ts, hevc_pid)
        } else {
            (&h264_ts, video_pid)
        };
        let retained_audio = adts_frame(40, 0xA1);
        demuxer.feed(&valid_audio_pes_packet(
            audio_pid,
            1,
            900_000,
            &retained_audio,
        ));
        demuxer.feed(&valid_video_pes_packet(
            video_pid,
            1,
            900_000,
            &[0, 0, 1, 0x65, 0xDD],
        ));

        let mut coalesced = make_pmt_ts_pkt(
            version.wrapping_add(1) & 0x1F,
            &[(replacement_kind, replacement_pid), (0x0F, audio_pid)],
        );
        coalesced.extend_from_slice(&fixture_pid_packets_as(
            replacement_ts,
            source_pid,
            replacement_pid,
        ));
        coalesced.extend_from_slice(&make_pmt_ts_pkt(version.wrapping_add(2) & 0x1F, &original));
        coalesced.extend_from_slice(&fixture_pid_packets_as(&h264_ts, video_pid, video_pid));
        demuxer.feed(&coalesced);
        let changes = demuxer.take_topology_changes();
        assert!(
            changes.video,
            "every selected video identity boundary invalidates held GOPs"
        );
        assert!(!changes.audio, "unchanged audio identity remains live");
        demuxer.flush();
        let restored = demuxer
            .take_probe()
            .expect("restored fixture config completes snapshot");
        assert_eq!(
            serde_json::to_value(&restored.video).unwrap(),
            serde_json::to_value(&initial.video).unwrap()
        );
        assert_eq!(
            restored.video_sequence_header,
            initial.video_sequence_header
        );
        let packets = demuxer.drain();
        assert!(packets.iter().any(|p| p.media_type == MediaType::Video));
        assert!(packets.iter().any(|p| p.payload.as_ref() == retained_audio));
        // Compare every surviving video unit with an independent demux of the
        // original fixture: neither the queued marker nor replacement HEVC
        // units can be replayed under the restored H.264 metadata.
        let mut expected = TsDemuxer::new();
        expected.feed(&h264_ts);
        expected.flush();
        let expected = expected.drain();
        assert_eq!(
            packets
                .iter()
                .filter(|p| p.media_type == MediaType::Video)
                .count(),
            expected
                .iter()
                .filter(|p| p.media_type == MediaType::Video)
                .count(),
            "intermediate replacement packets must not survive the restoring PMT"
        );
        for packet in packets.iter().filter(|p| p.media_type == MediaType::Video) {
            assert!(
                expected
                    .iter()
                    .any(|p| { p.media_type == MediaType::Video && p.payload == packet.payload })
            );
        }
    }
}

#[test]
fn q098_review_topology_flags_accumulate_without_a_complete_probe() {
    let original = [(0x1B, 0x100), (0x0F, 0x101)];
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&make_pat_ts_pkt());
    demuxer.feed(&make_pmt_ts_pkt(0, &original));
    assert!(
        demuxer.take_probe().is_none(),
        "nonempty initial topology needs configs"
    );
    let initial = demuxer.take_topology_changes();
    assert!(initial.audio && initial.video);

    let mut coalesced = make_pmt_ts_pkt(1, &[(0x24, 0x120), (0x0F, 0x101)]);
    coalesced.extend_from_slice(&make_pmt_ts_pkt(2, &[(0x1B, 0x100)]));
    coalesced.extend_from_slice(&make_pmt_ts_pkt(3, &original));
    demuxer.feed(&coalesced);
    assert!(
        demuxer.take_probe().is_none(),
        "no config or publishable final probe yet"
    );
    let changes = demuxer.take_topology_changes();
    assert!(
        changes.audio && changes.video,
        "OR all boundaries, not just the final layout"
    );
    let cleared = demuxer.take_topology_changes();
    assert!(!cleared.audio && !cleared.video);
}

#[test]
fn q098_review_noop_and_descriptor_pmts_preserve_packets_and_flags() {
    let pid = 0x101;
    let audio = adts_frame(40, 0xAA);
    let pmt = make_pmt_ts_pkt(0, &[(0x0F, pid)]);
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&make_pat_ts_pkt());
    demuxer.feed(&pmt);
    demuxer.feed(&valid_audio_pes_packet(pid, 0, 0, &audio));
    demuxer.take_probe().expect("initial audio config");
    demuxer.take_topology_changes();
    demuxer.drain();
    demuxer.feed(&valid_audio_pes_packet(pid, 1, 90_000, &audio));

    demuxer.feed(&pmt);
    demuxer.feed(&make_pmt_ts_pkt(1, &[(0x0F, pid)]));
    assert!(
        demuxer.take_probe().is_none(),
        "version-only changes are no-ops"
    );
    demuxer.feed(&make_pmt_ts_pkt_with_descriptors(
        2,
        &[(0x0F, pid, b"\x0A\x04eng\0")],
    ));
    let probe = demuxer
        .take_probe()
        .expect("descriptor update publishes metadata");
    assert_eq!(probe.audio_tracks[0].language.as_deref(), Some("eng"));
    let changes = demuxer.take_topology_changes();
    assert!(!changes.audio && !changes.video);
    let packets = demuxer.drain();
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].payload.as_ref(), audio);
    assert_eq!(packets[0].pts, 1000);
}

#[test]
fn q098_review_unselected_video_changes_do_not_invalidate_packets() {
    let (mut demuxer, initial, _) = fixture_probe_demux(false);
    let video_pid = initial
        .video
        .as_ref()
        .and_then(|v| v.pid)
        .expect("video PID");
    let audio_pid = initial.audio_tracks[0].pid.expect("audio PID");
    let version = next_pmt_version(&demuxer);
    demuxer.feed(&make_pmt_ts_pkt(
        version,
        &[(0x1B, video_pid), (0x0F, audio_pid)],
    ));
    demuxer.take_probe();
    demuxer.take_topology_changes();
    let video = [0, 0, 1, 0x65, 0xAA];
    demuxer.feed(&valid_video_pes_packet(video_pid, 0, 90_000, &video));
    for (offset, extra) in [
        (1, Some((0x24, 0x120))),
        (2, Some((0x1B, 0x121))),
        (3, None),
    ] {
        let mut streams = vec![(0x1B, video_pid), (0x0F, audio_pid)];
        if let Some(extra) = extra {
            streams.push(extra);
        }
        demuxer.feed(&make_pmt_ts_pkt(
            version.wrapping_add(offset) & 0x1F,
            &streams,
        ));
        let changes = demuxer.take_topology_changes();
        assert!(
            !changes.audio && !changes.video,
            "only the selected video identity matters"
        );
        assert_eq!(demuxer.streams.len(), 2, "extra video stays unselected");
    }
    let packets = demuxer.drain();
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].payload.as_ref(), video);
}

#[test]
fn q098_review_same_version_bad_crc_video_type_is_ignored() {
    let (mut demuxer, initial, _) = fixture_probe_demux(false);
    let video_pid = initial
        .video
        .as_ref()
        .and_then(|v| v.pid)
        .expect("video PID");
    let audio_pid = initial.audio_tracks[0].pid.expect("audio PID");
    let streams = [(0x1B, video_pid), (0x0F, audio_pid)];
    let version = next_pmt_version(&demuxer);
    let valid = make_pmt_ts_pkt(version, &streams);
    demuxer.feed(&valid);
    let active = demuxer.take_probe().unwrap_or(initial);
    demuxer.take_topology_changes();
    let queued = [0, 0, 1, 0x65, 0xCC];
    demuxer.feed(&valid_video_pes_packet(video_pid, 0, 90_000, &queued));
    demuxer.feed(&unbounded_video_pes_start_packet(
        video_pid,
        1,
        180_000,
        &[0, 0, 1, 0x41],
    ));
    let partial = demuxer.streams[0].pes.buf.clone();
    let allocation = demuxer.streams[0].pes.buf.as_ptr();

    for corrupt_version in [version, version.wrapping_add(1) & 0x1F] {
        let mut corrupt = make_pmt_ts_pkt(corrupt_version, &streams);
        corrupt[17] = 0x1A; // Structurally valid, unsupported video type; retain old CRC.
        let section_len = (usize::from(corrupt[6] & 0x0F) << 8) | usize::from(corrupt[7]);
        assert_ne!(crc32_mpeg2(&corrupt[5..5 + 3 + section_len]), 0);
        demuxer.feed(&corrupt);
        assert_eq!(
            demuxer.pmt_version, version,
            "bad CRC must not consume any version"
        );
        assert_eq!(demuxer.streams.len(), 2);
        assert_eq!(demuxer.streams[0].pid, video_pid);
        assert_eq!(demuxer.streams[0].kind, StreamKind::H264);
        assert_eq!(demuxer.streams[1].pid, audio_pid);
        assert_eq!(demuxer.streams[1].kind, StreamKind::AacAdts);
        assert_eq!(demuxer.streams[0].pes.buf, partial);
        assert_eq!(demuxer.streams[0].pes.buf.as_ptr(), allocation);
        assert!(demuxer.take_probe().is_none());
        let changes = demuxer.take_topology_changes();
        assert!(!changes.audio && !changes.video);
    }
    let packets = demuxer.drain();
    assert_eq!(
        packets.len(),
        1,
        "corruption must not invalidate completed video"
    );
    assert_eq!(packets[0].payload.as_ref(), queued);

    // A valid descriptor-only rebuild exposes the still-cached configuration
    // without another video PES, proving corruption did not erase metadata.
    demuxer.feed(&make_pmt_ts_pkt_with_descriptors(
        version.wrapping_add(1) & 0x1F,
        &[(0x1B, video_pid, &[]), (0x0F, audio_pid, b"\x0A\x04eng\0")],
    ));
    let preserved = demuxer
        .take_probe()
        .expect("cached configs survived the corrupt PMT");
    assert_eq!(
        serde_json::to_value(&preserved.video).unwrap(),
        serde_json::to_value(&active.video).unwrap()
    );
    assert_eq!(
        preserved.video_sequence_header,
        active.video_sequence_header
    );
    assert_eq!(
        preserved.audio_tracks[0].sample_rate,
        active.audio_tracks[0].sample_rate
    );
    assert_eq!(
        preserved.audio_tracks[0].channels,
        active.audio_tracks[0].channels
    );
}

#[test]
fn q098_review_first_empty_pmt_publishes_snapshot() {
    let mut initial = make_pat_ts_pkt();
    let pmt = make_pmt_ts_pkt(0, &[]);
    initial.extend_from_slice(&pmt);
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&initial);
    let empty = demuxer
        .take_probe()
        .expect("first validated empty PMT is authoritative");
    assert!(empty.video.is_none());
    assert!(empty.video_sequence_header.is_none());
    assert_eq!(empty.video_track_count, 0);
    assert!(empty.audio_tracks.is_empty());
    assert!(!demuxer.has_streams());
    assert_eq!(demuxer.pmt_version, 0);
    let changes = demuxer.take_topology_changes();
    assert!(
        !changes.audio && !changes.video,
        "there were no prior media identities"
    );
    demuxer.feed(&pmt);
    assert!(
        demuxer.take_probe().is_none(),
        "identical empty retransmit is a no-op"
    );
}

#[test]
fn q098_review_incomplete_initial_topology_to_empty_publishes_snapshot() {
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&make_pat_ts_pkt());
    demuxer.feed(&make_pmt_ts_pkt(0, &[(0x1B, 0x100), (0x0F, 0x101)]));
    demuxer.feed(&valid_audio_pes_packet(
        0x101,
        0,
        90_000,
        &adts_frame(40, 0xAA),
    ));
    assert!(
        demuxer.take_probe().is_none(),
        "initial video config is still missing"
    );
    demuxer.take_topology_changes();
    demuxer.feed(&make_pmt_ts_pkt(1, &[]));
    let empty = demuxer
        .take_probe()
        .expect("known-empty topology ends initial incompleteness");
    assert!(empty.video.is_none());
    assert!(empty.video_sequence_header.is_none());
    assert_eq!(empty.video_track_count, 0);
    assert!(empty.audio_tracks.is_empty());
    let changes = demuxer.take_topology_changes();
    assert!(changes.audio && changes.video);
    assert!(
        demuxer.drain().is_empty(),
        "completed old audio is invalidated"
    );
}
