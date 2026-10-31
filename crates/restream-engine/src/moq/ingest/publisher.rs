//! One MoQ publisher's ring-side state on the ingest Owner thread: the
//! session counters, the probe hold, and publication through the SRT ingest
//! path ([`crate::srt::forward_ingest_packets`]): preview, input gate,
//! standby GOP cache, timestamp mapping and the configuration-recording push.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::engine::{IngestRegistration, MediaEngine};
use crate::input_gate::InputTimestampMapper;
use crate::stage_metrics::StageMetrics;
use restream_core::MediaPacket;
use restream_sync::RingBuffer;
use restream_sync::standby_gop::StandbyGopCache;

/// Bounds on packets held while Tokio applies the probe (the ring may be
/// replaced): the same budget SRT ingest holds per publisher.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProbeHoldLimits {
    pub(crate) bytes: usize,
    pub(crate) timeout: Duration,
}

impl Default for ProbeHoldLimits {
    fn default() -> Self {
        Self {
            bytes: 4 << 20,
            timeout: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, Default)]
enum ProbeHold {
    #[default]
    NotSent,
    Pending {
        since: Instant,
        held: Vec<MediaPacket>,
        bytes: usize,
    },
    /// Answered, or given up on: packets go straight to the ring.
    Released,
}

/// The session counters the ingest state shares with the Owner thread.
pub(crate) struct PublisherCounters {
    pub(crate) bytes_received: Arc<AtomicU64>,
    pub(crate) first_media_ms: Arc<AtomicU64>,
    pub(crate) ingest_metrics: Arc<StageMetrics>,
    pub(crate) last_progress_ms: Arc<AtomicU64>,
    pub(crate) keyframe_times: Arc<Mutex<Vec<i64>>>,
}

/// One MoQ publisher's ring-side state, owned by its connection on the
/// ingest Owner thread. Built on Tokio at admission.
pub(crate) struct PublisherMedia {
    registration: IngestRegistration,
    ring: Arc<RingBuffer>,
    counters: PublisherCounters,
    timestamp_mapper: InputTimestampMapper,
    standby_gop: StandbyGopCache,
    batch: Vec<MediaPacket>,
    probe: ProbeHold,
    limits: ProbeHoldLimits,
    /// Probe holds that ended on their byte or time bound.
    pub(crate) probe_hold_overflows: u64,
}

impl PublisherMedia {
    pub(crate) fn new(
        registration: IngestRegistration,
        ring: Arc<RingBuffer>,
        counters: PublisherCounters,
        limits: ProbeHoldLimits,
    ) -> Self {
        Self {
            registration,
            ring,
            counters,
            timestamp_mapper: InputTimestampMapper::default(),
            standby_gop: StandbyGopCache::default(),
            batch: Vec::with_capacity(1),
            probe: ProbeHold::NotSent,
            limits,
            probe_hold_overflows: 0,
        }
    }

    /// Account bytes received from the publisher (session counters).
    pub(crate) fn note_received(&self, bytes: usize) {
        let len = u64::try_from(bytes).unwrap_or(u64::MAX);
        let now_ms = MediaEngine::now_epoch_ms();
        crate::engine::note_media_bytes(
            &self.counters.bytes_received,
            &self.counters.first_media_ms,
            len,
            now_ms,
        );
        self.counters.ingest_metrics.record_in(len);
        self.counters
            .last_progress_ms
            .store(now_ms, Ordering::Relaxed);
    }

    /// Start holding packets: the probe is on its way to Tokio. `false` if
    /// a probe was already sent.
    pub(crate) fn begin_probe(&mut self) -> bool {
        if !matches!(self.probe, ProbeHold::NotSent) {
            return false;
        }
        self.probe = ProbeHold::Pending {
            since: Instant::now(),
            held: Vec::new(),
            bytes: 0,
        };
        true
    }

    /// Publish one packet, or hold it while the probe is pending. A hold that
    /// would pass its byte or time bound is released to the current ring.
    pub(crate) fn publish(&mut self, packet: MediaPacket) {
        if let ProbeHold::Pending { since, held, bytes } = &mut self.probe {
            let len = packet.payload.len();
            if since.elapsed() < self.limits.timeout && *bytes + len <= self.limits.bytes {
                *bytes += len;
                held.push(packet);
                return;
            }
            self.probe_hold_overflows += 1;
            self.release_held();
        }
        self.batch.push(packet);
        self.forward_batch();
    }

    /// Tokio applied the probe; `ring` replaces the publisher's ring. Held
    /// packets flow in order. A late answer still swaps the ring.
    pub(crate) fn probe_applied(&mut self, ring: Option<Arc<RingBuffer>>) {
        if let Some(ring) = ring {
            self.ring = ring;
        }
        self.release_held();
    }

    /// The session ended: publish whatever is held.
    pub(crate) fn flush(&mut self) {
        self.release_held();
    }

    fn release_held(&mut self) {
        if let ProbeHold::Pending { held, .. } =
            std::mem::replace(&mut self.probe, ProbeHold::Released)
        {
            self.batch = held;
            self.forward_batch();
        }
    }

    fn forward_batch(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        crate::srt::forward_ingest_packets(
            &mut self.batch,
            &mut self.ring,
            &self.registration,
            &mut self.timestamp_mapper,
            &mut self.standby_gop,
            Some(&self.counters.keyframe_times),
        );
        self.batch.clear();
    }
}

#[cfg(test)]
mod tests;
