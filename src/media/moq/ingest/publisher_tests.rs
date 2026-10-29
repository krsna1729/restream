//! The probe hold and publication into the ring.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwapOption;
use bytes::Bytes;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::media::input_gate::InputPacketGate;
use restream_core::{MediaType, PayloadFormat};

fn counters() -> PublisherCounters {
    PublisherCounters {
        bytes_received: Arc::new(AtomicU64::new(0)),
        first_media_ms: Arc::new(AtomicU64::new(0)),
        ingest_metrics: Arc::new(StageMetrics::new()),
        last_progress_ms: Arc::new(AtomicU64::new(0)),
        keyframe_times: Arc::new(Mutex::new(Vec::new())),
    }
}

fn publisher(ring: Arc<RingBuffer>, limits: ProbeHoldLimits) -> PublisherMedia {
    let registration = IngestRegistration {
        cancel_token: CancellationToken::new(),
        attempt_id: 1,
        input_id: "input-moq".to_string(),
        gate: Arc::new(InputPacketGate::active()),
        last_forwarded_dts: Arc::new(AtomicI64::new(i64::MIN)),
        preview_ring: Arc::new(ArcSwapOption::empty()),
    };
    PublisherMedia::new(registration, ring, counters(), limits)
}

fn audio(dts: i64, len: usize) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Audio,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: dts,
        dts,
        payload: Bytes::from(vec![0u8; len]),
    }
}

fn ring_dts(ring: &RingBuffer) -> Vec<i64> {
    (0..ring.get_write_idx())
        .map(|index| ring.read_at(index).expect("published packet").dts)
        .collect()
}

#[test]
fn packets_wait_for_the_probe_answer_and_follow_the_replacement_ring() {
    let old = Arc::new(RingBuffer::new(16));
    let mut media = publisher(old.clone(), ProbeHoldLimits::default());
    media.publish(audio(10, 4));
    assert_eq!(ring_dts(&old), vec![10], "before the probe, packets flow");

    assert!(media.begin_probe());
    assert!(!media.begin_probe(), "one probe per session");
    media.publish(audio(20, 4));
    media.publish(audio(30, 4));
    assert_eq!(
        ring_dts(&old),
        vec![10],
        "held while Tokio applies the probe"
    );

    let new = Arc::new(RingBuffer::new(32));
    media.probe_applied(Some(new.clone()));
    assert_eq!(
        ring_dts(&new),
        vec![20, 30],
        "held packets, in order, on the new ring"
    );
    media.publish(audio(40, 4));
    assert_eq!(ring_dts(&new), vec![20, 30, 40]);
    assert_eq!(ring_dts(&old), vec![10]);
    assert_eq!(media.probe_hold_overflows, 0);
}

#[test]
fn a_hold_past_its_byte_bound_is_released_to_the_current_ring() {
    let ring = Arc::new(RingBuffer::new(16));
    let limits = ProbeHoldLimits {
        bytes: 10,
        timeout: std::time::Duration::from_secs(60),
    };
    let mut media = publisher(ring.clone(), limits);
    media.begin_probe();
    media.publish(audio(1, 6));
    assert!(ring_dts(&ring).is_empty());
    media.publish(audio(2, 6));
    assert_eq!(ring_dts(&ring), vec![1, 2]);
    assert_eq!(media.probe_hold_overflows, 1);
    // A late answer is harmless: nothing is held, packets keep flowing.
    media.probe_applied(None);
    media.publish(audio(3, 6));
    assert_eq!(ring_dts(&ring), vec![1, 2, 3]);
}

#[test]
fn a_hold_past_its_time_bound_is_released_and_flush_empties_any_hold() {
    let ring = Arc::new(RingBuffer::new(16));
    let limits = ProbeHoldLimits {
        bytes: 1 << 20,
        timeout: std::time::Duration::ZERO,
    };
    let mut media = publisher(ring.clone(), limits);
    media.begin_probe();
    media.publish(audio(1, 4));
    assert_eq!(ring_dts(&ring), vec![1]);
    assert_eq!(media.probe_hold_overflows, 1);

    let ring = Arc::new(RingBuffer::new(16));
    let mut media = publisher(ring.clone(), ProbeHoldLimits::default());
    media.begin_probe();
    media.publish(audio(5, 4));
    media.flush();
    assert_eq!(ring_dts(&ring), vec![5]);
}

#[test]
fn received_bytes_reach_the_session_counters() {
    let media = publisher(Arc::new(RingBuffer::new(16)), ProbeHoldLimits::default());
    media.note_received(1_000);
    media.note_received(500);
    assert_eq!(media.counters.bytes_received.load(Ordering::Relaxed), 1_500);
    assert!(media.counters.first_media_ms.load(Ordering::Relaxed) > 0);
    assert!(media.counters.last_progress_ms.load(Ordering::Relaxed) > 0);
}
