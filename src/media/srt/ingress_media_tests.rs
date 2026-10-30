//! Owner-side SRT ingest media: held-before-admission media and its fairness,
//! the first-probe boundary and ring migration, probe holding and its byte and
//! time budgets (one and many stalled probes), detach flush. Driven with the
//! checked-in H.264 TS fixture; no sockets or threads.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwapOption;
use tokio_util::sync::CancellationToken;

use super::super::ingress_test_support::{CHUNK, ts_chunks};
use super::*;
use crate::media::input_gate::InputPacketGate;
use crate::media::snapshots::SrtIngressOwnerStats;
use crate::media::stage_metrics::StageMetrics;
use restream_core::MediaType;

fn publisher(ring: Arc<RingBuffer>) -> Box<SrtPublisherMedia> {
    Box::new(SrtPublisherMedia {
        registration: IngestRegistration {
            cancel_token: CancellationToken::new(),
            attempt_id: 1,
            input_id: "input".to_string(),
            gate: Arc::new(InputPacketGate::active()),
            last_forwarded_dts: Arc::new(AtomicI64::new(i64::MIN)),
            preview_ring: Arc::new(ArcSwapOption::empty()),
        },
        ring_buffer: ring,
        demuxer: TsDemuxer::new(),
        timestamp_mapper: InputTimestampMapper::default(),
        standby_gop: StandbyGopCache::default(),
        packets: Vec::new(),
        keyframe_times: Arc::new(Mutex::new(Vec::new())),
        bytes_received: Arc::new(AtomicU64::new(0)),
        first_media_ms: Arc::new(AtomicU64::new(0)),
        ingest_metrics: Arc::new(StageMetrics::new()),
        last_progress_ms: Arc::new(AtomicU64::new(0)),
    })
}

/// Feed chunks to `peer` and return the first stream probe, if any.
fn feed_peer(
    media: &mut IngressMedia<u64>,
    peer: u64,
    chunks: &[Bytes],
    stats: &SrtIngressOwnerStats,
) -> Option<DemuxProbe> {
    let mut probe = None;
    for chunk in chunks {
        if let Some(found) = media.on_payload(peer, chunk.clone(), stats) {
            probe = Some(found);
        }
    }
    probe
}

fn feed(
    media: &mut IngressMedia<u64>,
    chunks: &[Bytes],
    stats: &SrtIngressOwnerStats,
) -> Option<DemuxProbe> {
    feed_peer(media, 1, chunks, stats)
}

/// Feed until the payload that yields the first probe; returns its index.
fn feed_until_probe(
    media: &mut IngressMedia<u64>,
    peer: u64,
    chunks: &[Bytes],
    stats: &SrtIngressOwnerStats,
) -> usize {
    chunks
        .iter()
        .position(|chunk| media.on_payload(peer, chunk.clone(), stats).is_some())
        .expect("the fixture yields a stream probe")
}

fn video_dts(ring: &RingBuffer, from: usize, to: usize) -> Vec<i64> {
    (from..to)
        .filter_map(|index| ring.read_at(index))
        .filter(|packet| packet.media_type == MediaType::Video)
        .map(|packet| packet.dts)
        .collect()
}

#[test]
fn media_received_before_admission_is_replayed_on_attach() {
    let stats = SrtIngressOwnerStats::default();
    let ring = Arc::new(RingBuffer::new(4096));
    let mut media = IngressMedia::<u64>::default();
    let chunks = ts_chunks(600);

    // Admission is still running on Tokio: nothing reaches the ring.
    assert!(feed(&mut media, &chunks[..300], &stats).is_none());
    assert_eq!(ring.get_write_idx(), 0);

    // Attach replays the held payloads; the probe they complete is returned.
    let probe = media.attach_publisher(1, publisher(ring.clone()), &stats);
    assert!(
        probe.is_some(),
        "the replayed media completed a stream probe"
    );
    media.probe_applied(1, media.probe_identity(1).unwrap(), None, &stats);
    let _ = feed(&mut media, &chunks[300..], &stats);
    assert!(
        ring.get_write_idx() > 0,
        "replayed and live media reached the ring"
    );
    assert_eq!(stats.media_unattached_dropped.load(Ordering::Relaxed), 0);
    assert_eq!(stats.media_payloads.load(Ordering::Relaxed), 600);
}

/// When earlier peers fill the Owner-wide admission cap, a
/// newly arriving peer still keeps its first payloads (its PAT/PMT and codec
/// data); the largest queue gives up room instead.
#[test]
fn a_new_peer_keeps_its_first_payloads_when_the_admission_cap_is_full() {
    let stats = SrtIngressOwnerStats::default();
    let limits = MediaLimits {
        unattached_peer_bytes: 200 * CHUNK,
        unattached_total_bytes: 400 * CHUNK,
        ..MediaLimits::default()
    };
    let mut media = IngressMedia::<u64>::with_limits(limits);
    let chunks = ts_chunks(600);
    // Two earlier peers fill the Owner-wide cap exactly.
    let _ = feed_peer(&mut media, 1, &chunks[..200], &stats);
    let _ = feed_peer(&mut media, 2, &chunks[..200], &stats);
    assert_eq!(media.unattached_bytes, 400 * CHUNK);
    assert_eq!(stats.media_unattached_dropped.load(Ordering::Relaxed), 0);

    // A new peer arrives: every one of its payloads is kept.
    let _ = feed_peer(&mut media, 3, &chunks[..50], &stats);
    assert_eq!(media.unattached[&3].payloads.len(), 50);
    assert_eq!(
        media.unattached[&3].payloads[0], chunks[0],
        "its first payload is kept"
    );
    assert!(media.unattached_bytes <= 400 * CHUNK);
    assert_eq!(stats.media_unattached_dropped.load(Ordering::Relaxed), 50);
    // The room came from the largest queues, oldest first.
    let kept_1 = media.unattached[&1].payloads.len();
    let kept_2 = media.unattached[&2].payloads.len();
    assert_eq!(kept_1 + kept_2, 350);
    assert!(
        kept_1.abs_diff(kept_2) <= 1,
        "evictions spread over the largest queues"
    );

    // Admission of the new peer replays its stream from the start.
    let ring = Arc::new(RingBuffer::new(4096));
    let _ = media.attach_publisher(3, publisher(ring.clone()), &stats);
    assert!(!media.unattached.contains_key(&3));
}

#[test]
fn a_peer_over_its_own_quota_drops_its_own_oldest_payloads() {
    let stats = SrtIngressOwnerStats::default();
    let limits = MediaLimits {
        unattached_peer_bytes: 100 * CHUNK,
        ..MediaLimits::default()
    };
    let mut media = IngressMedia::<u64>::with_limits(limits);
    let chunks = ts_chunks(110);
    let _ = feed_peer(&mut media, 7, &chunks, &stats);
    assert_eq!(stats.media_unattached_dropped.load(Ordering::Relaxed), 10);
    assert_eq!(media.unattached[&7].payloads[0], chunks[10]);
    // A refused peer's held media is released on detach.
    media.detach(7);
    assert_eq!(media.unattached_bytes, 0);
}

/// The first-probe boundary as a contract. Packets completed
/// by the payload that yields the probe are published to the current ring;
/// later packets are held. When Tokio adapts the ring the way
/// `adapt_pipeline_ring` does (a continuing ring seeded with the old ring's
/// readable tail), the held packets follow that tail in the new ring, so the
/// replacement carries everything published so far, in order.
#[test]
fn the_first_probe_boundary_and_ring_migration_are_ordered() {
    let stats = SrtIngressOwnerStats::default();
    let old_ring = Arc::new(RingBuffer::new(4096));
    let mut media = IngressMedia::<u64>::default();
    assert!(
        media
            .attach_publisher(1, publisher(old_ring.clone()), &stats)
            .is_none()
    );
    let chunks = ts_chunks(900);
    let probed_at = feed_until_probe(&mut media, 1, &chunks, &stats);
    let at_probe = old_ring.get_write_idx();

    // Later payloads are held, not published.
    let _ = feed(&mut media, &chunks[probed_at + 1..], &stats);
    assert_eq!(old_ring.get_write_idx(), at_probe);
    let held_bytes = media.held_bytes;
    assert!(held_bytes > 0);

    // Tokio adapts the ring exactly as `adapt_pipeline_ring` does.
    let new_ring = Arc::new(RingBuffer::new_continuing(8192, at_probe));
    let seeded = new_ring.seed_readable_tail_from(&old_ring);
    assert_eq!(seeded, at_probe, "the whole published tail migrates");
    media.probe_applied(
        1,
        media.probe_identity(1).unwrap(),
        Some(new_ring.clone()),
        &stats,
    );

    assert_eq!(
        old_ring.get_write_idx(),
        at_probe,
        "nothing late on the old ring"
    );
    assert!(
        new_ring.get_write_idx() > at_probe,
        "held packets follow the tail"
    );
    let dts = video_dts(&new_ring, 0, new_ring.get_write_idx());
    assert!(
        dts.windows(2).all(|pair| pair[0] <= pair[1]),
        "migrated tail then held packets, in DTS order"
    );
    assert_eq!(media.held_bytes, 0);
    assert_eq!(stats.media_probe_hold_overflows.load(Ordering::Relaxed), 0);
}

/// Many publishers waiting on Tokio at once stay inside the
/// per-publisher and Owner-wide byte budgets; the overflow outcome publishes
/// held media to the current ring, counted, instead of growing.
#[test]
fn concurrent_stalled_probes_stay_inside_their_byte_budgets() {
    let stats = SrtIngressOwnerStats::default();
    let limits = MediaLimits {
        probe_hold_peer_bytes: 256 << 10,
        probe_hold_total_bytes: 600 << 10,
        probe_hold_timeout: Duration::from_secs(3600),
        ..MediaLimits::default()
    };
    let mut media = IngressMedia::<u64>::with_limits(limits);
    let chunks = ts_chunks(900);
    let rings: Vec<_> = (0..4)
        .map(|peer| {
            let ring = Arc::new(RingBuffer::new(8192));
            let _ = media.attach_publisher(peer, publisher(ring.clone()), &stats);
            ring
        })
        .collect();
    // Every publisher reaches its probe; none is answered.
    let probed: Vec<usize> = (0..4)
        .map(|peer| feed_until_probe(&mut media, peer, &chunks, &stats))
        .collect();
    let mut peak = 0;
    for step in 0..900 {
        for peer in 0..4u64 {
            let index = probed[peer as usize] + 1 + step;
            if let Some(chunk) = chunks.get(index) {
                let _ = media.on_payload(peer, chunk.clone(), &stats);
            }
        }
        peak = peak.max(media.held_bytes);
        assert!(media.held_bytes <= limits.probe_hold_total_bytes);
        for slot in media.publishers.values() {
            assert!(slot.held_bytes <= limits.probe_hold_peer_bytes);
        }
    }
    assert!(peak > 0);
    assert!(
        stats.media_probe_hold_overflows.load(Ordering::Relaxed) > 0,
        "budgets were reached and the overflow outcome ran"
    );
    // After the overflow outcome, media is published to the current rings.
    for ring in &rings {
        assert!(ring.get_write_idx() > 0);
    }
    // A late answer still lands cleanly.
    for peer in 0..4 {
        media.probe_applied(peer, media.probe_identity(peer).unwrap(), None, &stats);
    }
    assert_eq!(media.held_bytes, 0);
}

#[test]
fn a_probe_unanswered_past_the_timeout_releases_its_hold() {
    let stats = SrtIngressOwnerStats::default();
    let limits = MediaLimits {
        probe_hold_timeout: Duration::ZERO,
        ..MediaLimits::default()
    };
    let mut media = IngressMedia::<u64>::with_limits(limits);
    let ring = Arc::new(RingBuffer::new(4096));
    let _ = media.attach_publisher(1, publisher(ring.clone()), &stats);
    let chunks = ts_chunks(900);
    let probed_at = feed_until_probe(&mut media, 1, &chunks, &stats);
    let at_probe = ring.get_write_idx();
    let _ = feed(&mut media, &chunks[probed_at + 1..], &stats);
    assert_eq!(stats.media_probe_hold_overflows.load(Ordering::Relaxed), 1);
    assert!(
        ring.get_write_idx() > at_probe,
        "publication resumed on the current ring"
    );
    assert_eq!(media.held_bytes, 0);
}

#[test]
fn detach_flushes_the_demuxer_and_forgets_the_peer() {
    let stats = SrtIngressOwnerStats::default();
    let ring = Arc::new(RingBuffer::new(4096));
    let mut media = IngressMedia::<u64>::default();
    let _ = media.attach_publisher(1, publisher(ring.clone()), &stats);
    let chunks = ts_chunks(400);
    let probed_at = feed_until_probe(&mut media, 1, &chunks, &stats);
    let _ = feed(&mut media, &chunks[probed_at + 1..], &stats);
    // Detach while the probe is outstanding: held media is published.
    let before = ring.get_write_idx();
    media.detach(1);
    assert!(
        ring.get_write_idx() > before,
        "held and flushed media was published"
    );
    assert!(media.publisher_peers().is_empty());
    assert_eq!(media.held_bytes, 0);
    // Media for a detached peer is only held (bounded), never published.
    let after_detach = ring.get_write_idx();
    let _ = feed(&mut media, &ts_chunks(10), &stats);
    assert_eq!(ring.get_write_idx(), after_detach);
}

fn poison_payload() -> Bytes {
    let mut payload = vec![0u8; CHUNK];
    payload[..INJECTED_PANIC_PAYLOAD.len()].copy_from_slice(INJECTED_PANIC_PAYLOAD);
    Bytes::from(payload)
}

/// A peer whose media work unwinds is discarded and gives back exactly its
/// share of the Owner-wide held-bytes budget; another publisher's held media
/// and its release are unaffected.
#[test]
fn discarding_a_panicked_peer_returns_only_its_held_bytes() {
    let stats = SrtIngressOwnerStats::default();
    let mut media = IngressMedia::<u64>::default();
    let chunks = ts_chunks(900);
    let ring_one = Arc::new(RingBuffer::new(4096));
    let ring_two = Arc::new(RingBuffer::new(4096));
    let _ = media.attach_publisher(1, publisher(ring_one), &stats);
    let _ = media.attach_publisher(2, publisher(ring_two.clone()), &stats);
    let probed = feed_until_probe(&mut media, 1, &chunks, &stats);
    let _ = feed_peer(&mut media, 1, &chunks[probed + 1..], &stats);
    let one_held = media.held_bytes;
    let probed = feed_until_probe(&mut media, 2, &chunks, &stats);
    let _ = feed_peer(&mut media, 2, &chunks[probed + 1..], &stats);
    let two_held = media.held_bytes - one_held;
    assert!(one_held > 0 && two_held > 0);

    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        media.on_payload(1, poison_payload(), &stats)
    }));
    assert!(unwound.is_err());
    media.discard(1);

    assert_eq!(media.held_bytes, two_held);
    let before = ring_two.get_write_idx();
    media.probe_applied(2, media.probe_identity(2).unwrap(), None, &stats);
    assert!(
        ring_two.get_write_idx() > before,
        "peer 2's held media flows"
    );
    assert_eq!(media.held_bytes, 0);
}

/// The admission replay can unwind too: the slot is in the map before the
/// replay, so discarding the peer returns the bytes the replay held. (With
/// the slot inserted after the replay, an unwind dropped it and its held
/// bytes stayed charged to the Owner-wide budget forever.)
#[test]
fn a_replay_that_unwinds_leaves_no_bytes_charged() {
    let stats = SrtIngressOwnerStats::default();
    let mut media = IngressMedia::<u64>::default();
    let mut early = ts_chunks(900);
    early.push(poison_payload());
    let _ = feed(&mut media, &early, &stats);
    assert!(media.unattached_bytes > 0);

    let ring = Arc::new(RingBuffer::new(4096));
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        media.attach_publisher(1, publisher(ring), &stats)
    }));
    assert!(unwound.is_err());
    media.discard(1);

    assert_eq!(media.held_bytes, 0);
    assert_eq!(media.unattached_bytes, 0);
    assert!(media.publisher_peers().is_empty());
}

#[test]
fn stale_probe_ack_cannot_release_or_redirect_a_newer_topology_or_session() {
    use super::super::ingress_test_support::topology_chunks;

    let stats = SrtIngressOwnerStats::default();
    let ring = Arc::new(RingBuffer::new(4096));
    let wrong_ring = Arc::new(RingBuffer::new(4096));
    let mut media = IngressMedia::<u64>::default();
    media.attach_publisher(1, publisher(ring.clone()), &stats);
    assert!(feed(&mut media, &topology_chunks(true, true, 0), &stats).is_some());
    let first = media.probe_identity(1).unwrap();
    let removed = feed(&mut media, &topology_chunks(false, true, 1), &stats).unwrap();
    assert!(removed.video.is_none());
    let current = media.probe_identity(1).unwrap();
    assert_ne!(current, first);
    assert!(media.held_bytes > 0);

    media.probe_applied(1, first, Some(wrong_ring.clone()), &stats);
    assert!(Arc::ptr_eq(&media.publishers[&1].media.ring_buffer, &ring));
    assert!(media.publishers[&1].probe_pending);
    assert!(media.held_bytes > 0);
    media.probe_applied(1, current, None, &stats);
    assert_eq!(media.held_bytes, 0);

    // Reuse the addressable peer slot, but never its ingest attempt identity.
    let mut replacement = publisher(ring.clone());
    replacement.registration.attempt_id = 2;
    media.attach_publisher(1, replacement, &stats);
    assert!(feed(&mut media, &topology_chunks(true, true, 0), &stats).is_some());
    let replacement = media.probe_identity(1).unwrap();
    assert_eq!(replacement.revision, first.revision);
    media.probe_applied(1, first, Some(wrong_ring), &stats);
    assert!(Arc::ptr_eq(&media.publishers[&1].media.ring_buffer, &ring));
    assert!(media.publishers[&1].probe_pending);
    media.probe_applied(1, replacement, None, &stats);
    assert_eq!(media.held_bytes, 0);
}

#[test]
fn reindexed_audio_does_not_replay_a_removed_pid_from_the_probe_hold() {
    use super::super::ingress_test_support::topology_pmt;

    let fixture = crate::media::test_fixtures::av_marker_transport_fixture("h264", true).unwrap();
    let fixture = std::fs::read(fixture).unwrap();
    let chunks: Vec<_> = fixture
        .chunks(CHUNK)
        .take(400)
        .map(Bytes::copy_from_slice)
        .collect();
    let stats = SrtIngressOwnerStats::default();
    let ring = Arc::new(RingBuffer::new(4096));
    let mut media = IngressMedia::<u64>::default();
    media.attach_publisher(1, publisher(ring), &stats);
    let first = feed(&mut media, &chunks, &stats).unwrap();
    assert_eq!(first.audio_tracks.len(), 2);
    assert!(
        media.publishers[&1]
            .held
            .iter()
            .flatten()
            .any(|packet| { packet.media_type == MediaType::Audio && packet.track_index == 0 })
    );
    assert!(
        media.publishers[&1]
            .held
            .iter()
            .flatten()
            .any(|packet| { packet.media_type == MediaType::Video })
    );

    let pmt = topology_pmt(
        first.video.unwrap().pid,
        &[first.audio_tracks[1].pid.unwrap()],
        1,
    );
    let update = media.on_payload(1, pmt, &stats).unwrap();
    assert_eq!(update.audio_tracks.len(), 1);
    assert_eq!(update.audio_tracks[0].track_index, 0);
    assert_eq!(update.audio_tracks[0].pid, first.audio_tracks[1].pid);
    assert!(
        media.publishers[&1]
            .held
            .iter()
            .flatten()
            .all(|packet| { packet.media_type != MediaType::Audio }),
        "old index 0 packets must not masquerade as the retained PID"
    );
    assert!(
        media.publishers[&1]
            .held
            .iter()
            .flatten()
            .any(|packet| { packet.media_type == MediaType::Video }),
        "still-live video survives the topology update"
    );
}

#[test]
fn hold_empty_topology_releases_batch_allocations_with_their_credits() {
    use super::super::ingress_test_support::{topology_chunks, topology_pmt};

    let stats = SrtIngressOwnerStats::default();
    let mut media = IngressMedia::<u64>::default();
    media.attach_publisher(1, publisher(Arc::new(RingBuffer::new(4096))), &stats);
    assert!(feed(&mut media, &topology_chunks(true, true, 0), &stats).is_some());
    assert!(!media.publishers[&1].held.is_empty());
    assert!(media.held_bytes > 0);
    let update = media
        .on_payload(1, topology_pmt(None, &[], 1), &stats)
        .unwrap();
    assert!(update.video.is_none() && update.audio_tracks.is_empty());
    assert_eq!(media.held_bytes, 0);
    assert_eq!(media.publishers[&1].held_bytes, 0);
    assert!(
        media.publishers[&1].held.is_empty(),
        "credited empty batches must not retain packet Vec allocations"
    );
}

#[test]
fn hold_topology_updates_keep_the_outstanding_absolute_deadline() {
    use super::super::ingress_test_support::{topology_chunks, topology_pmt};

    let stats = SrtIngressOwnerStats::default();
    let limits = MediaLimits {
        probe_hold_timeout: Duration::from_secs(30),
        ..MediaLimits::default()
    };
    let mut media = IngressMedia::<u64>::with_limits(limits);
    let ring = Arc::new(RingBuffer::new(4096));
    media.attach_publisher(1, publisher(ring.clone()), &stats);
    let first = feed(&mut media, &topology_chunks(true, true, 0), &stats).unwrap();
    assert!(media.held_bytes > 0);
    // Deterministically model an already-expired control response, without
    // sleeping or adding a second clock/deadline implementation.
    media.publishers.get_mut(&1).unwrap().probe_sent_at =
        Some(Instant::now() - Duration::from_secs(60));
    let update = media
        .on_payload(
            1,
            topology_pmt(None, &[first.audio_tracks[0].pid.unwrap()], 1),
            &stats,
        )
        .unwrap();
    assert!(update.video.is_none());
    assert!(
        media.held_bytes > 0,
        "still-live audio is held until the next drain"
    );
    let before = ring.get_write_idx();
    assert!(feed(&mut media, &topology_chunks(false, true, 1), &stats).is_none());
    assert_eq!(
        media.held_bytes, 0,
        "the original absolute deadline must release media"
    );
    assert!(!media.publishers[&1].probe_pending);
    assert!(
        ring.get_write_idx() > before,
        "expired held audio was published"
    );
    assert!(stats.media_probe_hold_overflows.load(Ordering::Relaxed) > 0);
}

#[test]
fn coalesced_video_replacement_invalidates_held_media_and_standby_gop() {
    use super::super::ingress_test_support::topology_pmt_typed;
    use restream_mpegts::TsMuxer;

    for (codec, new_pid) in [("h264", 0x120), ("hevc", 0x100), ("hevc", 0x120)] {
        let stats = SrtIngressOwnerStats::default();
        let mut media = IngressMedia::<u64>::default();
        let mut source = publisher(Arc::new(RingBuffer::new(4096)));
        source.registration.gate = Arc::new(InputPacketGate::standby());
        media.attach_publisher(1, source, &stats);
        let first = feed(&mut media, &ts_chunks(600), &stats).unwrap();
        assert!(media.publishers[&1].media.standby_gop.is_replay_ready());
        assert!(
            media.publishers[&1]
                .held
                .iter()
                .flatten()
                .any(|p| p.media_type == MediaType::Video)
        );
        let fixture = if codec == "h264" {
            crate::media::test_fixtures::canonical_h264_ts_fixture().unwrap()
        } else {
            crate::media::test_fixtures::canonical_h265_ts_fixture().unwrap()
        };
        let mut demux = TsDemuxer::new();
        demux.feed(&std::fs::read(fixture).unwrap());
        demux.flush();
        let video = demux.take_probe().unwrap().video.unwrap();
        let mut packets = Vec::new();
        demux.drain_into(&mut packets);
        let sets = packets
            .iter()
            .filter(|p| p.media_type == MediaType::Video)
            .find_map(|p| restream_core::codec::annexb_parameter_sets(&p.payload))
            .unwrap();
        let kind = if codec == "hevc" { 0x24 } else { 0x1B };
        let mut batch = topology_pmt_typed(
            Some((kind, new_pid)),
            &[first.audio_tracks[0].pid.unwrap()],
            1,
        )
        .to_vec();
        let mut mux = TsMuxer::new(Some(&video), &[]);
        // Two short, real configuration PESs: the second start completes the
        // first in the same receive that changes the PMT, without an IDR.
        for dts in [1000, 1040] {
            for packet in mux
                .mux_packet(MediaType::Video, 0, dts, dts, false, &sets)
                .chunks_exact(188)
            {
                let pid = (u16::from(packet[1] & 31) << 8) | u16::from(packet[2]);
                if pid == 0x100 {
                    let mut packet = packet.to_vec();
                    packet[1] = (packet[1] & 0xE0) | ((new_pid >> 8) as u8 & 31);
                    packet[2] = new_pid as u8;
                    batch.extend_from_slice(&packet);
                }
            }
        }
        assert!(batch.len() <= CHUNK);
        let latest = media.on_payload(1, Bytes::from(batch), &stats).unwrap();
        assert_eq!(latest.video.unwrap().pid, Some(new_pid));
        let slot = &media.publishers[&1];
        assert!(
            !slot.media.standby_gop.is_replay_ready(),
            "{codec}/{new_pid}"
        );
        let held_video: Vec<_> = slot
            .held
            .iter()
            .flatten()
            .filter(|p| p.media_type == MediaType::Video)
            .collect();
        assert!(!held_video.is_empty(), "fresh replacement packets survive");
        assert!(
            held_video
                .iter()
                .all(|p| p.payload.as_ref() == sets.as_slice()),
            "removed video identity must never be replayed as its replacement"
        );
    }
}
