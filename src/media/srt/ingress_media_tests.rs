//! Owner-side SRT ingest media: held-before-admission media, probe holding
//! with ring replacement, detach flush, and the unattached bound. Driven with
//! the checked-in H.264 TS fixture; no sockets or threads.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwapOption;
use tokio_util::sync::CancellationToken;

use super::super::ingress_test_support::ts_chunks;
use super::*;
use crate::media::input_gate::InputPacketGate;
use crate::media::snapshots::SrtIngressOwnerStats;
use crate::media::stage_metrics::StageMetrics;

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
        ingest_metrics: Arc::new(StageMetrics::new()),
        last_progress_ms: Arc::new(AtomicU64::new(0)),
        probe_sent: false,
    })
}

/// Feed chunks to peer 1 and return the first stream probe, if any.
fn feed(
    media: &mut IngressMedia<u64>,
    chunks: &[Bytes],
    stats: &SrtIngressOwnerStats,
) -> Option<DemuxProbe> {
    let mut probe = None;
    for chunk in chunks {
        if let Some(found) = media.on_payload(1, chunk.clone(), stats) {
            probe = Some(found);
        }
    }
    probe
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
    media.probe_applied(1, None);
    let _ = feed(&mut media, &chunks[300..], &stats);
    assert!(
        ring.get_write_idx() > 0,
        "replayed and live media reached the ring"
    );
    assert_eq!(stats.media_unattached_dropped.load(Ordering::Relaxed), 0);
    assert_eq!(stats.media_payloads.load(Ordering::Relaxed), 600);
}

#[test]
fn packets_wait_for_the_probe_answer_and_follow_a_replaced_ring() {
    let stats = SrtIngressOwnerStats::default();
    let old_ring = Arc::new(RingBuffer::new(4096));
    let new_ring = Arc::new(RingBuffer::new(8192));
    let mut media = IngressMedia::<u64>::default();
    assert!(
        media
            .attach_publisher(1, publisher(old_ring.clone()), &stats)
            .is_none()
    );

    let chunks = ts_chunks(900);
    let mut probed_at = None;
    for (index, chunk) in chunks.iter().enumerate() {
        if media.on_payload(1, chunk.clone(), &stats).is_some() {
            probed_at = Some(index);
            break;
        }
    }
    let probed_at = probed_at.expect("the fixture yields a stream probe");
    let before_probe = old_ring.get_write_idx();

    // Tokio has not answered: later packets are held, not published.
    let _ = feed(&mut media, &chunks[probed_at + 1..], &stats);
    assert_eq!(
        old_ring.get_write_idx(),
        before_probe,
        "nothing published while the probe is outstanding"
    );
    assert_eq!(new_ring.get_write_idx(), 0);

    // The answer replaces the ring; held packets go to the new ring in order.
    media.probe_applied(1, Some(new_ring.clone()));
    assert_eq!(old_ring.get_write_idx(), before_probe);
    let released = new_ring.get_write_idx();
    assert!(released > 0, "held packets reached the replacement ring");
    let dts: Vec<i64> = (0..released)
        .filter_map(|index| new_ring.read_at(index))
        .filter(|packet| packet.media_type == crate::media::packet::MediaType::Video)
        .map(|packet| packet.dts)
        .collect();
    assert!(
        dts.windows(2).all(|pair| pair[0] <= pair[1]),
        "held video left in DTS order"
    );
}

#[test]
fn detach_flushes_the_demuxer_and_forgets_the_peer() {
    let stats = SrtIngressOwnerStats::default();
    let ring = Arc::new(RingBuffer::new(4096));
    let mut media = IngressMedia::<u64>::default();
    let _ = media.attach_publisher(1, publisher(ring.clone()), &stats);
    if feed(&mut media, &ts_chunks(400), &stats).is_some() {
        media.probe_applied(1, None);
    }
    let before = ring.get_write_idx();
    media.detach(1);
    assert!(
        ring.get_write_idx() >= before,
        "the final flush never loses what was published"
    );
    assert!(media.publisher_peers().is_empty());
    // Media for a detached peer is only held (bounded), never published.
    let after_detach = ring.get_write_idx();
    let _ = feed(&mut media, &ts_chunks(10), &stats);
    assert_eq!(ring.get_write_idx(), after_detach);
}

#[test]
fn unattached_media_is_bounded_per_peer() {
    let stats = SrtIngressOwnerStats::default();
    let mut media = IngressMedia::<u64>::default();
    let chunk = ts_chunks(1).remove(0);
    for _ in 0..(UNATTACHED_PER_PEER + 10) {
        let _ = media.on_payload(7, chunk.clone(), &stats);
    }
    assert_eq!(
        stats.media_unattached_dropped.load(Ordering::Relaxed),
        10,
        "the oldest payloads beyond the per-peer bound are dropped and counted"
    );
    // A refused peer's held media is released on detach.
    media.detach(7);
    assert_eq!(media.unattached_total, 0);
}
