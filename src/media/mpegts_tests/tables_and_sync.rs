use super::*;

pub(super) fn make_pat_ts_pkt() -> Vec<u8> {
    let mut pkt = vec![0xFFu8; 188];
    pkt[0] = 0x47;
    pkt[1] = 0x40; // PUSI, PID=0x0000
    pkt[2] = 0x00;
    pkt[3] = 0x10; // payload-only, CC=0
    pkt[4] = 0x00; // pointer_field
    // PAT section
    pkt[5] = 0x00; // table_id = PAT
    pkt[6] = 0xB0;
    pkt[7] = 13; // section_length
    pkt[8] = 0x00;
    pkt[9] = 0x01; // TSID = 1
    pkt[10] = 0xC1; // version=0, current_next=1
    pkt[11] = 0x00; // section_number
    pkt[12] = 0x00; // last_section_number
    // Program 1 -> PMT PID 0x1000
    pkt[13] = 0x00;
    pkt[14] = 0x01;
    pkt[15] = 0xF0; // 0xE0 | (0x1000 >> 8) = 0xF0
    pkt[16] = 0x00; // 0x1000 & 0xFF
    let crc = crc32_mpeg2(&pkt[5..17]);
    pkt[17] = (crc >> 24) as u8;
    pkt[18] = (crc >> 16) as u8;
    pkt[19] = (crc >> 8) as u8;
    pkt[20] = crc as u8;
    pkt
}

/// Build a 188-byte TS PMT packet at PID 0x1000 with the given version and
/// stream list. Each stream is `(stream_type, elementary_pid)`.
pub(super) fn make_pmt_ts_pkt(version: u8, streams: &[(u8, u16)]) -> Vec<u8> {
    let streams = streams
        .iter()
        .map(|&(kind, pid)| (kind, pid, &[][..]))
        .collect::<Vec<_>>();
    make_pmt_ts_pkt_with_descriptors(version, &streams)
}

pub(super) fn make_pmt_ts_pkt_with_descriptors(
    version: u8,
    streams: &[(u8, u16, &[u8])],
) -> Vec<u8> {
    let mut pkt = vec![0xFFu8; 188];
    pkt[0] = 0x47;
    pkt[1] = 0x50; // PUSI, PID high (0x1000)
    pkt[2] = 0x00;
    pkt[3] = 0x10;
    pkt[4] = 0x00; // pointer_field
    pkt[5] = 0x02; // table_id = PMT
    let section_len = 13 + streams.iter().map(|(_, _, d)| 5 + d.len()).sum::<usize>();
    assert!(section_len <= 180, "test PMT must fit a single TS packet");
    pkt[6] = 0xB0 | ((section_len >> 8) as u8);
    pkt[7] = section_len as u8;
    pkt[8] = 0x00;
    pkt[9] = 0x01; // program_number
    pkt[10] = 0xC0 | ((version & 0x1F) << 1) | 0x01;
    pkt[11] = 0x00; // section_number
    pkt[12] = 0x00; // last_section_number
    pkt[13] = 0xE1; // PCR_PID = 0x100
    pkt[14] = 0x00;
    pkt[15] = 0xF0;
    pkt[16] = 0x00; // program_info_length
    let mut pos = 17;
    for &(stream_type, pid, descriptors) in streams {
        pkt[pos] = stream_type;
        pkt[pos + 1] = 0xE0 | ((pid >> 8) as u8 & 0x1F);
        pkt[pos + 2] = pid as u8;
        pkt[pos + 3] = 0xF0 | ((descriptors.len() >> 8) as u8);
        pkt[pos + 4] = descriptors.len() as u8;
        pos += 5;
        pkt[pos..pos + descriptors.len()].copy_from_slice(descriptors);
        pos += descriptors.len();
    }
    rewrite_pmt_crc(&mut pkt);
    pkt
}

fn rewrite_pmt_crc(pkt: &mut [u8]) {
    let section_len = (usize::from(pkt[6] & 0x0F) << 8) | usize::from(pkt[7]);
    let crc_start = 5 + 3 + section_len - 4;
    let crc = crc32_mpeg2(&pkt[5..crc_start]);
    pkt[crc_start..crc_start + 4].copy_from_slice(&crc.to_be_bytes());
}

pub(super) fn fixture_probe_demux(two_audio: bool) -> (TsDemuxer, DemuxProbe, Vec<u8>) {
    let fixture = if two_audio {
        crate::test_fixtures::av_marker_transport_fixture("h264", true)
    } else {
        crate::test_fixtures::canonical_h264_ts_fixture()
    }
    .unwrap_or_else(|e| panic!("{e}"));
    let ts = std::fs::read(&fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&ts);
    demuxer.flush();
    demuxer.drain();
    let probe = demuxer
        .take_probe()
        .expect("fixture should complete its initial probe");
    // Use the shared builder's PMT PID without changing fixture elementary PIDs.
    demuxer.feed(&make_pat_ts_pkt());
    (demuxer, probe, ts)
}

fn feed_fixture_pid(demuxer: &mut TsDemuxer, ts: &[u8], pid: u16) {
    feed_fixture_pid_as(demuxer, ts, pid, pid);
}

fn feed_fixture_pid_as(demuxer: &mut TsDemuxer, ts: &[u8], pid: u16, target_pid: u16) {
    demuxer.feed(&fixture_pid_packets_as(ts, pid, target_pid));
    demuxer.flush();
}

pub(super) fn fixture_pid_packets_as(ts: &[u8], pid: u16, target_pid: u16) -> Vec<u8> {
    let mut packets = Vec::new();
    for packet in ts.chunks_exact(TS_PACKET_SIZE) {
        let packet_pid = (u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2]);
        if packet_pid == pid {
            let mut packet: [u8; TS_PACKET_SIZE] = packet.try_into().expect("whole TS packet");
            packet[1] = (packet[1] & 0xE0) | ((target_pid >> 8) as u8 & 0x1F);
            packet[2] = target_pid as u8;
            packets.extend_from_slice(&packet);
        }
    }
    packets
}

pub(super) fn next_pmt_version(demuxer: &TsDemuxer) -> u8 {
    demuxer.pmt_version.wrapping_add(1) & 0x1F
}

#[test]
fn q098_pmt_video_removal_and_reappearance_preserves_audio() {
    let (mut demuxer, initial, ts) = fixture_probe_demux(false);
    let video_pid = initial
        .video
        .as_ref()
        .and_then(|v| v.pid)
        .expect("video PID");
    assert_eq!(initial.audio_tracks.len(), 1);
    let audio = &initial.audio_tracks[0];
    let audio_pid = audio.pid.expect("audio PID");
    let version = next_pmt_version(&demuxer);
    demuxer.feed(&make_pmt_ts_pkt(version, &[(0x0F, audio_pid)]));
    let removed = demuxer
        .take_probe()
        .expect("PMT removal must not wait for PES");
    assert!(removed.video.is_none());
    assert!(removed.video_sequence_header.is_none());
    assert_eq!(removed.video_track_count, 0);
    assert_eq!(removed.audio_tracks.len(), 1);
    assert_eq!(removed.audio_tracks[0].pid, audio.pid);
    assert_eq!(removed.audio_tracks[0].sample_rate, audio.sample_rate);
    assert_eq!(removed.audio_tracks[0].channels, audio.channels);
    assert_eq!(removed.audio_tracks[0].profile, audio.profile);
    feed_fixture_pid(&mut demuxer, &ts, video_pid);
    assert!(
        demuxer.drain().is_empty(),
        "removed PID must not dispatch media"
    );
    assert!(demuxer.take_probe().is_none());

    let version = next_pmt_version(&demuxer);
    demuxer.feed(&make_pmt_ts_pkt(
        version,
        &[(0x1B, video_pid), (0x0F, audio_pid)],
    ));
    assert!(
        demuxer.take_probe().is_none(),
        "reappearing video needs fresh config"
    );
    demuxer.feed(&valid_video_pes_packet(
        video_pid,
        0,
        900,
        &[0, 0, 1, 0x41, 0x9A],
    ));
    assert!(
        demuxer.take_probe().is_none(),
        "a non-SPS frame cannot restore video metadata"
    );
    demuxer.drain();
    feed_fixture_pid(&mut demuxer, &ts, video_pid);
    let restored = demuxer
        .take_probe()
        .expect("video PES should restore the complete snapshot");
    let video = restored.video.expect("restored video");
    let original_video = initial.video.expect("original video");
    assert_eq!(
        (video.width, video.height),
        (original_video.width, original_video.height)
    );
    assert_eq!(
        restored.video_sequence_header,
        initial.video_sequence_header
    );
    assert_eq!(restored.video_track_count, 1);
    assert_eq!(restored.audio_tracks.len(), 1);
    assert_eq!(restored.audio_tracks[0].sample_rate, audio.sample_rate);
    assert_eq!(restored.audio_tracks[0].channels, audio.channels);
    assert_eq!(restored.audio_tracks[0].profile, audio.profile);
    assert!(demuxer.take_probe().is_none());
}

#[test]
fn q098_pmt_audio_removal_reindexes_retained_config() {
    let (mut demuxer, initial, ts) = fixture_probe_demux(true);
    assert_eq!(initial.audio_tracks.len(), 2);
    let video_pid = initial
        .video
        .as_ref()
        .and_then(|v| v.pid)
        .expect("video PID");
    let retained = &initial.audio_tracks[1];
    let retained_pid = retained.pid.expect("retained audio PID");
    let removed_pid = initial.audio_tracks[0].pid.expect("removed audio PID");
    let version = next_pmt_version(&demuxer);
    demuxer.feed(&make_pmt_ts_pkt(
        version,
        &[(0x1B, video_pid), (0x0F, retained_pid)],
    ));
    let update = demuxer
        .take_probe()
        .expect("audio removal must publish without more PES");
    assert_eq!(update.audio_tracks.len(), 1);
    assert_eq!(update.audio_tracks[0].track_index, 0);
    assert_eq!(update.audio_tracks[0].pid, retained.pid);
    assert_eq!(update.audio_tracks[0].sample_rate, retained.sample_rate);
    assert_eq!(update.audio_tracks[0].channels, retained.channels);
    assert_eq!(update.audio_tracks[0].profile, retained.profile);
    assert_eq!(update.video_sequence_header, initial.video_sequence_header);
    assert_eq!(
        update.video.as_ref().map(|v| (v.width, v.height)),
        initial.video.as_ref().map(|v| (v.width, v.height))
    );
    feed_fixture_pid(&mut demuxer, &ts, removed_pid);
    assert!(demuxer.drain().is_empty());
    feed_fixture_pid(&mut demuxer, &ts, retained_pid);
    let packets = demuxer.drain();
    assert!(!packets.is_empty());
    assert!(
        packets
            .iter()
            .all(|p| p.media_type == MediaType::Audio && p.track_index == 0)
    );
    assert!(demuxer.take_probe().is_none());
}

#[test]
fn q098_pmt_retransmission_and_version_only_change_are_noops() {
    let (mut demuxer, initial, ts) = fixture_probe_demux(false);
    let video_pid = initial
        .video
        .as_ref()
        .and_then(|v| v.pid)
        .expect("video PID");
    let audio_pid = initial.audio_tracks[0].pid.expect("audio PID");
    let streams = [(0x1B, video_pid), (0x0F, audio_pid)];
    let version = next_pmt_version(&demuxer);
    let pmt = make_pmt_ts_pkt(version, &streams);
    demuxer.feed(&pmt);
    demuxer.take_probe(); // The fixture may have different descriptors.
    demuxer.feed(&unbounded_video_pes_start_packet(
        video_pid,
        3,
        90_000,
        &[0, 0, 1, 0x65],
    ));
    let before = demuxer.streams[0].pes.buf.clone();
    let allocation = demuxer.streams[0].pes.buf.as_ptr();
    demuxer.feed(&pmt);
    assert!(demuxer.take_probe().is_none());
    let version = next_pmt_version(&demuxer);
    demuxer.feed(&make_pmt_ts_pkt(version, &streams));
    assert_eq!(demuxer.pmt_version, version);
    assert!(demuxer.take_probe().is_none());
    assert_eq!(demuxer.streams[0].pes.buf, before);
    assert_eq!(demuxer.streams[0].pes.buf.as_ptr(), allocation);
    assert_eq!(demuxer.streams[0].continuity, 3);
    feed_fixture_pid(&mut demuxer, &ts, video_pid);
    assert!(
        demuxer.take_probe().is_none(),
        "no-op PMT must not re-arm probing"
    );
}

#[test]
fn q098_pmt_descriptor_change_updates_cached_metadata_without_pes() {
    let (mut demuxer, initial, _) = fixture_probe_demux(false);
    let video_pid = initial
        .video
        .as_ref()
        .and_then(|v| v.pid)
        .expect("video PID");
    let audio_pid = initial.audio_tracks[0].pid.expect("audio PID");
    let version = next_pmt_version(&demuxer);
    demuxer.feed(&make_pmt_ts_pkt_with_descriptors(
        version,
        &[(0x1B, video_pid, &[]), (0x0F, audio_pid, b"\x0A\x04eng\0")],
    ));
    demuxer.take_probe();
    let version = next_pmt_version(&demuxer);
    let changed = make_pmt_ts_pkt_with_descriptors(
        version,
        &[(0x1B, video_pid, &[]), (0x0F, audio_pid, b"\x0A\x04spa\0")],
    );
    demuxer.feed(&changed);
    let update = demuxer
        .take_probe()
        .expect("descriptor change is immediate");
    assert_eq!(update.audio_tracks[0].language.as_deref(), Some("spa"));
    assert_eq!(
        update.audio_tracks[0].sample_rate,
        initial.audio_tracks[0].sample_rate
    );
    assert_eq!(
        update.audio_tracks[0].channels,
        initial.audio_tracks[0].channels
    );
    assert_eq!(update.video_sequence_header, initial.video_sequence_header);
    demuxer.feed(&changed);
    assert!(demuxer.take_probe().is_none());
}

#[test]
fn q098_pmt_codec_kind_change_drops_pes_and_cached_video_config() {
    let (mut demuxer, initial, _) = fixture_probe_demux(false);
    let video_pid = initial
        .video
        .as_ref()
        .and_then(|v| v.pid)
        .expect("video PID");
    let audio_pid = initial.audio_tracks[0].pid.expect("audio PID");
    demuxer.feed(&unbounded_video_pes_start_packet(
        video_pid,
        0,
        90_000,
        &[0, 0, 1, 0x65],
    ));
    assert!(!demuxer.streams[0].pes.buf.is_empty());
    let version = next_pmt_version(&demuxer);
    demuxer.feed(&make_pmt_ts_pkt(
        version,
        &[(0x24, video_pid), (0x0F, audio_pid)],
    ));
    assert_eq!(demuxer.streams[0].kind, StreamKind::H265);
    assert!(demuxer.streams[0].pes.buf.is_empty());
    assert!(!demuxer.streams[0].pes.has_timestamp);
    let update = demuxer
        .take_probe()
        .expect("old codec metadata must be removed immediately");
    assert!(update.video.is_none());
    assert!(update.video_sequence_header.is_none());
    assert_eq!(update.video_track_count, 0);
    assert_eq!(update.audio_tracks.len(), 1);
    assert_eq!(
        update.audio_tracks[0].sample_rate,
        initial.audio_tracks[0].sample_rate
    );
}

#[test]
fn q098_pmt_removal_propagates_while_added_track_waits_for_config() {
    let (mut demuxer, initial, ts) = fixture_probe_demux(false);
    let audio_pid = initial.audio_tracks[0].pid.expect("audio PID");
    let new_pid = 0x120;
    let version = next_pmt_version(&demuxer);
    demuxer.feed(&make_pmt_ts_pkt(
        version,
        &[(0x0F, audio_pid), (0x0F, new_pid)],
    ));
    let removed = demuxer
        .take_probe()
        .expect("removed video cannot wait for a new audio PID");
    assert!(removed.video.is_none());
    assert!(removed.video_sequence_header.is_none());
    assert_eq!(removed.audio_tracks.len(), 1);
    assert_eq!(removed.audio_tracks[0].pid, Some(audio_pid));
    feed_fixture_pid_as(&mut demuxer, &ts, audio_pid, new_pid);
    let added = demuxer
        .take_probe()
        .expect("new PID must decode its own config");
    assert!(added.video.is_none());
    assert_eq!(added.audio_tracks.len(), 2);
    assert_eq!(added.audio_tracks[0].pid, Some(audio_pid));
    assert_eq!(added.audio_tracks[1].pid, Some(new_pid));
    assert_eq!(added.audio_tracks[1].track_index, 1);
    assert_eq!(
        added.audio_tracks[1].sample_rate,
        initial.audio_tracks[0].sample_rate
    );
}

#[test]
fn q098_pmt_removing_all_tracks_publishes_empty_snapshot() {
    let (mut demuxer, _, _) = fixture_probe_demux(false);
    let version = next_pmt_version(&demuxer);
    demuxer.feed(&make_pmt_ts_pkt(version, &[]));
    let empty = demuxer
        .take_probe()
        .expect("empty topology is authoritative");
    assert!(empty.video.is_none());
    assert!(empty.video_sequence_header.is_none());
    assert_eq!(empty.video_track_count, 0);
    assert!(empty.audio_tracks.is_empty());
    assert!(!demuxer.has_streams());
}

#[test]
fn q098_pmt_initial_probe_stays_complete_across_topology_change() {
    let (video, audio) = first_probe_ready_payloads();
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&make_pat_ts_pkt());
    demuxer.feed(&make_pmt_ts_pkt(0, &[(0x1B, 0x100), (0x0F, 0x101)]));
    demuxer.try_build_probe(1, &audio);
    assert!(demuxer.take_probe().is_none());
    demuxer.feed(&make_pmt_ts_pkt(
        1,
        &[(0x1B, 0x100), (0x0F, 0x101), (0x0F, 0x102)],
    ));
    demuxer.try_build_probe(0, &video);
    assert!(
        demuxer.take_probe().is_none(),
        "new audio is still unprobed"
    );
    demuxer.feed(&make_pmt_ts_pkt(2, &[(0x1B, 0x100), (0x0F, 0x101)]));
    let initial = demuxer
        .take_probe()
        .expect("all surviving configs are complete");
    assert!(initial.video.is_some());
    assert_eq!(initial.audio_tracks.len(), 1);
}

#[test]
fn q098_pmt_current_next_false_does_not_apply_or_consume_version() {
    let (mut demuxer, initial, _) = fixture_probe_demux(false);
    let video_pid = initial
        .video
        .as_ref()
        .and_then(|v| v.pid)
        .expect("video PID");
    let audio_pid = initial.audio_tracks[0].pid.expect("audio PID");
    let version = next_pmt_version(&demuxer);
    let mut future = make_pmt_ts_pkt(version, &[(0x0F, audio_pid)]);
    future[10] &= !1;
    rewrite_pmt_crc(&mut future);
    let previous_version = demuxer.pmt_version;
    demuxer.feed(&future);
    assert_eq!(demuxer.pmt_version, previous_version);
    assert_eq!(demuxer.streams[0].pid, video_pid);
    assert!(demuxer.take_probe().is_none());
    demuxer.feed(&make_pmt_ts_pkt(version, &[(0x0F, audio_pid)]));
    assert_eq!(demuxer.pmt_version, version);
    let current = demuxer
        .take_probe()
        .expect("current table applies the same version");
    assert!(current.video.is_none());
    assert_eq!(current.audio_tracks.len(), 1);
}

#[test]
fn q098_pmt_malformed_sections_leave_active_probe_and_version_unchanged() {
    let (mut demuxer, initial, _) = fixture_probe_demux(false);
    let audio_pid = initial.audio_tracks[0].pid.expect("audio PID");
    let previous_version = demuxer.pmt_version;
    let version = next_pmt_version(&demuxer);
    let valid = make_pmt_ts_pkt(version, &[(0x0F, audio_pid)]);
    let mut oversized = valid.clone();
    oversized[6] = 0xBF;
    oversized[7] = 0xFF;
    let mut short = valid.clone();
    short[7] = 0;
    let bad_descriptor =
        make_pmt_ts_pkt_with_descriptors(version, &[(0x0F, audio_pid, b"\x0A\xFFeng\0")]);
    let mut multi_section = valid.clone();
    multi_section[12] = 1;
    rewrite_pmt_crc(&mut multi_section);
    for malformed in [oversized, short, bad_descriptor, multi_section] {
        demuxer.feed(&malformed);
        let continuation = build_ts_packet(0x1000, false, 1, 0, &[], &[0; 184]);
        for _ in 0..256 {
            demuxer.feed(&continuation);
        }
        assert_eq!(demuxer.pmt_version, previous_version);
        assert_eq!(demuxer.streams.len(), 2);
        assert!(demuxer.take_probe().is_none());
    }
    demuxer.feed(&valid);
    assert_eq!(demuxer.pmt_version, version);
    let update = demuxer
        .take_probe()
        .expect("valid table must recover after malformed sections");
    assert!(update.video.is_none());
    assert_eq!(update.audio_tracks.len(), 1);
}

// --- Regression: issue #3 — PMT version tracking ---

#[test]
fn pmt_retransmission_same_version_is_idempotent() {
    // Regression: the old guard `if !self.streams.is_empty() && pmt_expected == 0 { return }`
    // was replaced by explicit version tracking. A retransmission of the same
    // PMT version must NOT rebuild the stream map (no phantom duplicates).
    let mut data = Vec::new();
    data.extend_from_slice(&make_pat_ts_pkt());
    let streams = [(0x1B, 0x100u16), (0x0F, 0x101u16)]; // H.264 + AAC
    data.extend_from_slice(&make_pmt_ts_pkt(0, &streams));

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&data);

    assert_eq!(
        demuxer.streams.len(),
        2,
        "initial PMT must produce 2 streams"
    );
    assert_eq!(demuxer.pmt_version, 0);

    // Feed the same PMT version again (broadcaster retransmits every ~100ms)
    let retransmit = make_pmt_ts_pkt(0, &streams);
    demuxer.feed(&retransmit);

    assert_eq!(
        demuxer.streams.len(),
        2,
        "retransmitting the same PMT version must not rebuild the stream map"
    );
    assert_eq!(demuxer.pmt_version, 0);
}

#[test]
fn pmt_version_change_rebuilds_stream_map() {
    // Regression: the old code returned early on non-empty streams, silently
    // dropping genuine PMT version changes (e.g., broadcaster adds audio mid-stream).
    let mut data = Vec::new();
    data.extend_from_slice(&make_pat_ts_pkt());
    // Version 0: video only
    data.extend_from_slice(&make_pmt_ts_pkt(0, &[(0x1B, 0x100u16)]));

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&data);

    assert_eq!(demuxer.streams.len(), 1, "PMT v0 must have 1 stream");
    assert_eq!(demuxer.pmt_version, 0);

    // Version 1: broadcaster added an audio track
    let v1_pkt = make_pmt_ts_pkt(1, &[(0x1B, 0x100u16), (0x0F, 0x101u16)]);
    demuxer.feed(&v1_pkt);

    assert_eq!(
        demuxer.streams.len(),
        2,
        "PMT version change must rebuild stream map so new audio PID is parsed"
    );
    assert_eq!(demuxer.pmt_version, 1);
    assert_eq!(demuxer.streams[1].kind, StreamKind::AacAdts);
}

#[test]
fn malformed_pmt_does_not_consume_version_or_replace_stream_map() {
    let valid_v1 = make_pmt_ts_pkt(1, &[(0x1B, 0x100u16), (0x0F, 0x101u16)]);
    let mut oversized_program_info = valid_v1.clone();
    oversized_program_info[15] = 0xFF;
    oversized_program_info[16] = 0xFF;
    rewrite_pmt_crc(&mut oversized_program_info);
    let mut oversized_es_info = valid_v1.clone();
    oversized_es_info[20] = 0xFF;
    oversized_es_info[21] = 0xFF;
    rewrite_pmt_crc(&mut oversized_es_info);

    for (case, malformed) in [
        ("program_info_length", oversized_program_info),
        ("ES_info_length", oversized_es_info),
    ] {
        let mut initial = Vec::new();
        initial.extend_from_slice(&make_pat_ts_pkt());
        initial.extend_from_slice(&make_pmt_ts_pkt(0, &[(0x1B, 0x100u16)]));

        let mut demuxer = TsDemuxer::new();
        demuxer.feed(&initial);
        assert_eq!(demuxer.pmt_version, 0);
        assert_eq!(demuxer.streams.len(), 1);
        assert_eq!(demuxer.streams[0].pid, 0x100);

        demuxer.feed(&malformed);

        assert_eq!(
            demuxer.pmt_version, 0,
            "{case} overflow must not consume the malformed PMT version"
        );
        assert_eq!(
            demuxer.streams.len(),
            1,
            "{case} overflow must not replace the working stream map"
        );
        assert_eq!(demuxer.streams[0].pid, 0x100);

        demuxer.feed(&valid_v1);

        assert_eq!(demuxer.pmt_version, 1);
        assert_eq!(
            demuxer.streams.len(),
            2,
            "a valid retransmission after {case} overflow must still be accepted"
        );
        assert_eq!(demuxer.streams[1].pid, 0x101);
    }
}

// --- Regression: issue #12 — PCR negative guard ---

#[test]
fn write_pcr_clamps_negative_ts_to_zero() {
    // Regression: before the .max(0) fix, a negative DTS reaching write_pcr
    // would silently cast to a large u64, producing a nonsensical PCR value
    // that makes decoders stall or seek unexpectedly.
    let mut buf_neg = [0u8; 6];
    let mut buf_zero = [0u8; 6];
    write_pcr(&mut buf_neg, -1_000_000);
    write_pcr(&mut buf_zero, 0);
    assert_eq!(
        buf_neg, buf_zero,
        "negative ts_90k must clamp to 0, not wrap to a huge u64 PCR"
    );

    // Also verify an extreme negative value does not panic.
    let mut buf_min = [0u8; 6];
    write_pcr(&mut buf_min, i64::MIN);
    assert_eq!(buf_min, buf_zero);
}

// --- Regression: issue #6 (Round 3) — TsDemuxer remainder length cap ---
// Before the MAX_REMAINDER guard, feeding a stream of single-byte 0x47
// chunks would cause remainder to grow by 1 byte on every call — O(n)
// memory growth per byte of input, i.e. O(n²) overall processing cost
// for a corrupt / adversarial stream.  After the fix the remainder must
// never exceed TS_PACKET_SIZE - 1 = 187 bytes.
#[test]
fn feed_remainder_capped_on_corrupt_stream() {
    let mut dem = TsDemuxer::new();
    // Feed 500 isolated 0x47 bytes (each looks like a TS sync byte but is
    // never followed by 187 more bytes, so no packet can complete).
    for _ in 0..500 {
        dem.feed(&[0x47]);
    }
    assert!(
        dem.remainder.len() < TS_PACKET_SIZE,
        "remainder must be capped at TS_PACKET_SIZE-1 ({}) but was {}",
        TS_PACKET_SIZE - 1,
        dem.remainder.len()
    );
}

// --- Regression: issue #5 (Round 4) — PMT version rebuild preserves in-flight PES ---
// Before the fix, a PMT version change discarded ALL StreamInfo (including
// PesAccumulator buffers).  A partially-assembled video frame would be lost,
// producing a glitch until the next IDR.  After the fix, PES buffers for PIDs
// that survive into the new PMT are carried over.
#[test]
fn q098_review_pmt_version_change_preserves_pes_for_unchanged_pid() {
    let pid = 0x100;
    let payload = [0, 0, 1, 0x65, 0xDE, 0xAD, 0xBE, 0xEF];
    let complete = valid_video_pes_packet(pid, 3, 90_000, &payload);
    // Exactly half of this bounded PES's payload, with no stuffing in the PES.
    let prefix = &complete[4..4 + 14 + 4];
    let start = build_ts_packet(pid, true, 3, 3, &vec![0; 183 - prefix.len()], prefix);
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&make_pat_ts_pkt());
    demuxer.feed(&make_pmt_ts_pkt(0, &[(0x1B, pid)]));
    demuxer.feed(&start);
    assert_eq!(demuxer.streams[0].pes.buf, payload[..4]);
    let allocation = demuxer.streams[0].pes.buf.as_ptr();
    demuxer.take_topology_changes();

    // Adding audio changes the layout and really rebuilds the stream map.
    demuxer.feed(&make_pmt_ts_pkt(1, &[(0x1B, pid), (0x0F, 0x101)]));
    assert_eq!(demuxer.streams.len(), 2);
    assert_eq!(demuxer.pmt_version, 1);
    assert_eq!(demuxer.streams[0].pes.buf, payload[..4]);
    assert_eq!(demuxer.streams[0].pes.buf.as_ptr(), allocation);
    assert_eq!(demuxer.streams[0].continuity, 3);
    let changes = demuxer.take_topology_changes();
    assert!(changes.audio);
    assert!(!changes.video);
    assert!(demuxer.take_probe().is_none(), "initial audio is unprobed");

    let suffix = &payload[4..];
    demuxer.feed(&build_ts_packet(
        pid,
        false,
        3,
        4,
        &vec![0; 183 - suffix.len()],
        suffix,
    ));
    let packets = demuxer.drain();
    assert_eq!(packets.len(), 1, "retained PES completes after the rebuild");
    assert_eq!(packets[0].payload.as_ref(), payload);
    assert_eq!(packets[0].pts, 1000);
    assert_eq!(packets[0].dts, 1000);
}

#[test]
fn crc32_empty_data() {
    assert_eq!(crc32_mpeg2(&[]), 0xFFFF_FFFF);
}

#[test]
fn crc32_known_vector() {
    // CRC-32/MPEG-2 of "123456789" (classic check value)
    let data = b"123456789";
    assert_eq!(crc32_mpeg2(data), 0x0376_E6E7);
}

#[test]
fn crc32_idempotent_across_calls() {
    let data = b"hello world";
    let a = crc32_mpeg2(data);
    let b = crc32_mpeg2(data);
    assert_eq!(a, b);
}

#[test]
fn write_pcr_zero_ts() {
    let mut buf = [0xFFu8; 6];
    write_pcr(&mut buf, 0);
    // PCR at zero: base=0, extension=0, with the PCR marker bits set
    assert_eq!(buf[0], 0x00);
    assert_eq!(buf[1], 0x00);
    assert_eq!(buf[2], 0x00);
    assert_eq!(buf[3], 0x00);
}

// --- const CRC32_TABLE correctness ---

#[test]
fn crc32_table_is_compile_time_const() {
    // The const table and a freshly-computed runtime table must agree for all 256 entries.
    let runtime_table: [u32; 256] = {
        let mut t = [0u32; 256];
        for (i, entry) in t.iter_mut().enumerate() {
            let mut crc = (i as u32) << 24;
            for _ in 0..8 {
                if crc & 0x8000_0000 != 0 {
                    crc = (crc << 1) ^ 0x04C1_1DB7;
                } else {
                    crc <<= 1;
                }
            }
            *entry = crc;
        }
        t
    };
    assert_eq!(
        CRC32_TABLE, runtime_table,
        "compile-time CRC32_TABLE must match a freshly-computed runtime table"
    );
}

// --- Sentinel u8 for continuity counter and PMT version ---

#[test]
#[allow(clippy::assertions_on_constants)]
fn continuity_sentinel_is_not_a_valid_cc_value() {
    // Valid continuity counter values are 0–15 (4-bit field in TS header).
    assert_eq!(CC_UNSET, u8::MAX, "CC_UNSET must be u8::MAX");
    assert!(
        CC_UNSET > 15,
        "CC_UNSET must be out of the valid 0–15 range"
    );
}

#[test]
#[allow(clippy::assertions_on_constants)]
fn pmt_version_sentinel_is_not_a_valid_version() {
    // Valid PMT version_number values are 0–31 (5-bit field in PMT header).
    assert_eq!(PMT_VER_UNSET, u8::MAX, "PMT_VER_UNSET must be u8::MAX");
    assert!(
        PMT_VER_UNSET > 31,
        "PMT_VER_UNSET must be out of the valid 0–31 range"
    );
}

#[test]
fn new_demuxer_pmt_version_is_unset() {
    let dem = TsDemuxer::new();
    assert_eq!(
        dem.pmt_version, PMT_VER_UNSET,
        "freshly constructed TsDemuxer must report PMT_VER_UNSET before any PMT is seen"
    );
}

#[test]
fn new_stream_continuity_is_unset() {
    let mut data = Vec::new();
    data.extend_from_slice(&make_pat_ts_pkt());
    data.extend_from_slice(&make_pmt_ts_pkt(0, &[(0x1B, 0x100u16)]));

    let mut dem = TsDemuxer::new();
    dem.feed(&data);

    assert_eq!(dem.streams.len(), 1, "PMT must add one video stream");
    assert_eq!(
        dem.streams[0].continuity, CC_UNSET,
        "new stream continuity must be CC_UNSET before first TS packet arrives"
    );
}

// --- find_ts_sync / ts_sync_candidate_is_valid direct tests ---

#[test]
fn find_ts_sync_empty_returns_zero_length() {
    assert_eq!(find_ts_sync(&[]), 0);
}

#[test]
fn find_ts_sync_no_sync_byte_returns_len() {
    let data = vec![0x00u8; 200];
    assert_eq!(find_ts_sync(&data), data.len());
}

#[test]
fn find_ts_sync_sync_at_offset_zero_accepted() {
    // One packet followed by non-0x47 — optimistic accept at offset 0
    let mut data = vec![0x00u8; TS_PACKET_SIZE * 2];
    data[0] = TS_SYNC_BYTE;
    data[TS_PACKET_SIZE] = TS_SYNC_BYTE; // confirm at +188
    assert_eq!(find_ts_sync(&data), 0);
}

#[test]
fn find_ts_sync_sync_at_offset_zero_short_buffer_optimistic() {
    // Buffer has only one packet (≤ TS_PACKET_SIZE bytes after candidate) → optimistic accept
    let mut data = vec![0x00u8; TS_PACKET_SIZE];
    data[0] = TS_SYNC_BYTE;
    assert_eq!(
        find_ts_sync(&data),
        0,
        "single-packet buffer must accept offset 0"
    );
}

#[test]
fn find_ts_sync_false_sync_skipped_real_sync_found() {
    // byte[5] = 0x47 but byte[5+188] ≠ 0x47 → rejected.
    // byte[10] = 0x47, +188 and +376 both confirmed → accepted.
    // Buffer is 4 packets so the triple-confirmation path is exercised.
    let mut data = vec![0x00u8; TS_PACKET_SIZE * 4];
    data[5] = TS_SYNC_BYTE; // false candidate — no +188 confirm
    data[10] = TS_SYNC_BYTE;
    data[10 + TS_PACKET_SIZE] = TS_SYNC_BYTE;
    data[10 + TS_PACKET_SIZE * 2] = TS_SYNC_BYTE;
    assert_eq!(find_ts_sync(&data), 10);
}

#[test]
fn find_ts_sync_double_confirmed_sync() {
    // Two consecutive confirming sync bytes (triple-confirmation path)
    let mut data = vec![0x00u8; TS_PACKET_SIZE * 4];
    data[3] = TS_SYNC_BYTE;
    data[3 + TS_PACKET_SIZE] = TS_SYNC_BYTE;
    data[3 + TS_PACKET_SIZE * 2] = TS_SYNC_BYTE;
    assert_eq!(find_ts_sync(&data), 3);
}

#[test]
fn ts_sync_candidate_not_sync_byte_returns_false() {
    let data = [0x00u8; 188];
    assert!(!ts_sync_candidate_is_valid(&data, 0));
}

#[test]
fn ts_sync_candidate_out_of_bounds_returns_false() {
    let data = [0x47u8; 10];
    assert!(!ts_sync_candidate_is_valid(&data, 20)); // candidate > data.len()
}
