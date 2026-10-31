//! SRT ingest media, run to completion on the ingress Owner thread: TS demux,
//! input gating, timestamp mapping, standby GOP caching and ring publication
//! for every publisher, plus direct SRT read/play. A received payload is
//! parsed on the thread that received it; nothing per packet crosses to Tokio
//! (two-pool plan).
//!
//! Tokio keeps session lifecycle: it authenticates a `Connected` peer, builds
//! its [`SrtPublisherMedia`] (engine registration) and attaches it with a
//! command; it applies initial probes and later container topology updates;
//! it does ingest bookkeeping on `Disconnected`, after the Owner flushes media.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use srt_transport::advanced::admission::LogicalPeerId;

use crate::engine::{IngestRegistration, MediaEngine};
use crate::input_gate::InputTimestampMapper;
use crate::snapshots::SrtIngressOwnerStats;
use crate::ts_chunk_ring::TsChunkReader;
use restream_core::MediaPacket;
use restream_mpegts::{DemuxProbe, TopologyChanges, TsDemuxer};
use restream_sync::RingBuffer;
use restream_sync::standby_gop::StandbyGopCache;

#[path = "ingest_packets.rs"]
pub(super) mod ingest_packets;

pub(crate) const SRT_MESSAGE_PAYLOAD_MAX: usize = 1316;

/// Byte and time bounds for media the Owner holds on Tokio's behalf.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MediaLimits {
    /// Media received while Tokio still admits a peer: per peer (about 2.5 s
    /// at 8 Mbit/s) and Owner-wide. A peer over its quota drops its own
    /// oldest payload; when the Owner-wide cap is full, the largest queue
    /// gives up its oldest, so a newly arriving peer always gets room.
    pub(crate) unattached_peer_bytes: usize,
    pub(crate) unattached_total_bytes: usize,
    /// Packets decoded after a publisher's probe or topology update are held
    /// until Tokio answers (the ring may be replaced): per publisher, Owner-wide
    /// and for at most `probe_hold_timeout`.
    pub(crate) probe_hold_peer_bytes: usize,
    pub(crate) probe_hold_total_bytes: usize,
    pub(crate) probe_hold_timeout: Duration,
}

impl Default for MediaLimits {
    fn default() -> Self {
        Self {
            unattached_peer_bytes: 2_500_000,
            unattached_total_bytes: 32 << 20,
            probe_hold_peer_bytes: 4 << 20,
            probe_hold_total_bytes: 64 << 20,
            probe_hold_timeout: Duration::from_secs(2),
        }
    }
}

/// One SRT publisher's media state, owned by the ingress Owner thread.
pub(crate) struct SrtPublisherMedia {
    pub(super) registration: IngestRegistration,
    pub(super) ring_buffer: Arc<RingBuffer>,
    pub(super) demuxer: TsDemuxer,
    pub(super) timestamp_mapper: InputTimestampMapper,
    pub(super) standby_gop: StandbyGopCache,
    pub(super) packets: Vec<MediaPacket>,
    pub(super) keyframe_times: Arc<Mutex<Vec<i64>>>,
    pub(super) bytes_received: Arc<std::sync::atomic::AtomicU64>,
    /// The session's first-media wall clock, shared with the ingest state.
    pub(super) first_media_ms: Arc<std::sync::atomic::AtomicU64>,
    pub(super) ingest_metrics: Arc<crate::stage_metrics::StageMetrics>,
    pub(super) last_progress_ms: Arc<std::sync::atomic::AtomicU64>,
}

impl SrtPublisherMedia {
    fn forward(&mut self, packets: &mut Vec<MediaPacket>, keyframes: bool) {
        ingest_packets::forward_ingest_packets(
            packets,
            &mut self.ring_buffer,
            &self.registration,
            &mut self.timestamp_mapper,
            &mut self.standby_gop,
            keyframes.then_some(&self.keyframe_times),
        );
    }

    /// Forward what the demuxer completed.
    fn forward_drained(&mut self) {
        let mut packets = std::mem::take(&mut self.packets);
        self.forward(&mut packets, true);
        self.packets = packets;
    }
}

/// One direct SRT read/play session (debug path), owned by the Owner thread.
pub(crate) struct SrtReaderMedia {
    pub(super) muxer: TsChunkReader,
    pub(super) packets: Vec<Arc<MediaPacket>>,
    pub(super) pending: VecDeque<Bytes>,
    /// The player's lease on the shared muxer; it stops when the last ends.
    _muxer_lease: crate::stage_lease::StageLease,
}

impl SrtReaderMedia {
    pub(super) fn new(muxer: TsChunkReader, muxer_lease: crate::stage_lease::StageLease) -> Self {
        Self {
            muxer,
            _muxer_lease: muxer_lease,
            packets: Vec::with_capacity(restream_sync::MEDIA_PULL_BURST_PACKETS),
            pending: VecDeque::new(),
        }
    }

    /// Fill `pending` with the next burst's SRT-sized fragments when empty.
    pub(super) fn refill(&mut self) {
        if !self.pending.is_empty() {
            return;
        }
        self.packets.clear();
        if self
            .muxer
            .pull_burst(&mut self.packets, restream_sync::MEDIA_PULL_BURST_PACKETS)
            .unwrap_or(0)
            == 0
        {
            return;
        }
        for packet in &self.packets {
            for fragment in packet.payload.chunks(SRT_MESSAGE_PAYLOAD_MAX) {
                if !fragment.is_empty() {
                    self.pending.push_back(packet.payload.slice_ref(fragment));
                }
            }
        }
    }
}

struct PublisherSlot {
    media: Box<SrtPublisherMedia>,
    /// Waiting for Tokio to apply the probe; drained groups are held in order.
    probe_revision: u64,
    probe_pending: bool,
    held: VecDeque<Vec<MediaPacket>>,
    held_bytes: usize,
    /// First outstanding probe's send time: absolute hold timeout and ack latency.
    probe_sent_at: Option<Instant>,
}

/// Identifies a container update within exactly one ingest attempt. A late
/// control response must not release a newer hold or redirect a replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProbeIdentity {
    pub(crate) attempt_id: u64,
    pub(crate) revision: u64,
}

#[derive(Default)]
struct Unattached {
    payloads: VecDeque<Bytes>,
    bytes: usize,
}

/// Every publisher's and reader's media state on the Owner thread, keyed by
/// the session handle (`LogicalPeerId` in production).
pub(crate) struct IngressMedia<K = LogicalPeerId> {
    limits: MediaLimits,
    publishers: HashMap<K, PublisherSlot>,
    unattached: HashMap<K, Unattached>,
    unattached_bytes: usize,
    held_bytes: usize,
    pub(super) readers: HashMap<K, SrtReaderMedia>,
}

impl<K> Default for IngressMedia<K> {
    fn default() -> Self {
        Self::with_limits(MediaLimits::default())
    }
}

impl<K> IngressMedia<K> {
    pub(crate) fn with_limits(limits: MediaLimits) -> Self {
        Self {
            limits,
            publishers: HashMap::new(),
            unattached: HashMap::new(),
            unattached_bytes: 0,
            held_bytes: 0,
            readers: HashMap::new(),
        }
    }
}

fn group_bytes(packets: &[MediaPacket]) -> usize {
    packets.iter().map(|packet| packet.payload.len()).sum()
}

impl<K: std::hash::Hash + Eq + Copy> IngressMedia<K> {
    /// One received payload. Returns a stream probe or topology update, which
    /// Tokio must apply ([`Self::probe_applied`]) before held packets flow.
    pub(crate) fn on_payload(
        &mut self,
        peer: K,
        payload: Bytes,
        stats: &SrtIngressOwnerStats,
    ) -> Option<DemuxProbe> {
        let Some(slot) = self.publishers.get_mut(&peer) else {
            if !self.readers.contains_key(&peer) {
                self.hold_unattached(peer, payload, stats);
            }
            return None;
        };
        let probe = Self::accept(slot, &payload, &self.limits, &mut self.held_bytes, stats);
        stats.media_payloads.fetch_add(1, Ordering::Relaxed);
        probe
    }

    /// Demux one payload and publish (or hold) what it completes.
    ///
    /// Packets completed by the payload yielding a probe or topology update
    /// reach the current ring; only later ones are held.
    /// A hold ends when Tokio answers ([`Self::probe_applied`]) or, as an
    /// explicit overflow outcome, when it would exceed the publisher's or the
    /// Owner's byte budget or outlive `probe_hold_timeout`: held packets are
    /// then published to the current ring and holding stops (counted in
    /// `mediaProbeHoldOverflows`). A late answer still applies the probe and
    /// any ring replacement; the only packets the replacement can miss are
    /// those published to the old ring after Tokio adapted it.
    fn accept(
        slot: &mut PublisherSlot,
        payload: &Bytes,
        limits: &MediaLimits,
        held_total: &mut usize,
        stats: &SrtIngressOwnerStats,
    ) -> Option<DemuxProbe> {
        #[cfg(test)]
        if payload.starts_with(INJECTED_PANIC_PAYLOAD) {
            std::panic::resume_unwind(Box::new("injected SRT media panic"));
        }
        slot.media.demuxer.feed(payload.as_ref());
        // Consume every accepted PMT identity change, even if later PMTs in
        // this same feed restore the original layout or no probe is complete.
        let changes = slot.media.demuxer.take_topology_changes();
        if changes.audio || changes.video {
            Self::invalidate_buffered(slot, changes, held_total);
        }
        if slot.media.demuxer.drain_into(&mut slot.media.packets) > 0 {
            if !slot.probe_pending {
                slot.media.forward_drained();
            } else {
                let bytes = group_bytes(&slot.media.packets);
                let expired = slot
                    .probe_sent_at
                    .is_some_and(|at| at.elapsed() >= limits.probe_hold_timeout);
                if !expired
                    && slot.held_bytes + bytes <= limits.probe_hold_peer_bytes
                    && *held_total + bytes <= limits.probe_hold_total_bytes
                {
                    slot.held_bytes += bytes;
                    *held_total += bytes;
                    slot.held.push_back(std::mem::take(&mut slot.media.packets));
                } else {
                    stats
                        .media_probe_hold_overflows
                        .fetch_add(1, Ordering::Relaxed);
                    Self::release_held(slot, held_total);
                    slot.media.forward_drained();
                }
            }
        }
        let media = &mut slot.media;
        let len = payload.len() as u64;
        let now_ms = MediaEngine::now_epoch_ms();
        crate::engine::note_media_bytes(&media.bytes_received, &media.first_media_ms, len, now_ms);
        media.ingest_metrics.record_in(len);
        media.last_progress_ms.store(now_ms, Ordering::Relaxed);
        let probe = media.demuxer.take_probe()?;
        // A slow control response can leave packets from an older topology
        // held here. Never replay those removed tracks into the updated ring.
        let keep = |packet: &MediaPacket| match packet.media_type {
            restream_core::MediaType::Video => probe.video.is_some(),
            restream_core::MediaType::Audio => probe
                .audio_tracks
                .iter()
                .any(|track| track.track_index == packet.track_index),
        };
        media.standby_gop.retain(keep);
        let previous_bytes = slot.held_bytes;
        for group in &mut slot.held {
            group.retain(keep);
        }
        // Empty batches still own Vec capacity; free them with their credits.
        slot.held.retain(|group| !group.is_empty());
        slot.held_bytes = slot.held.iter().map(|group| group_bytes(group)).sum();
        *held_total -= previous_bytes - slot.held_bytes;
        slot.probe_revision = slot.probe_revision.wrapping_add(1);
        if !slot.probe_pending {
            slot.probe_sent_at = Some(Instant::now());
        }
        slot.probe_pending = true;
        Some(probe)
    }

    fn invalidate_buffered(
        slot: &mut PublisherSlot,
        changes: TopologyChanges,
        held_total: &mut usize,
    ) {
        let keep = |packet: &MediaPacket| match packet.media_type {
            restream_core::MediaType::Audio => !changes.audio,
            restream_core::MediaType::Video => !changes.video,
        };
        slot.media.standby_gop.retain(keep);
        let previous = slot.held_bytes;
        for group in &mut slot.held {
            group.retain(keep);
        }
        slot.held.retain(|group| !group.is_empty());
        slot.held_bytes = slot.held.iter().map(|group| group_bytes(group)).sum();
        *held_total -= previous - slot.held_bytes;
    }

    /// Publish every held group to the current ring, in order, and stop
    /// holding.
    fn release_held(slot: &mut PublisherSlot, held_total: &mut usize) {
        // A superseded revision may already have sealed the source ring.
        // Overflow and detach must also flush into its current successor.
        while let Some(next) = slot.media.ring_buffer.next.load_full() {
            slot.media.ring_buffer = next;
        }
        slot.probe_pending = false;
        while let Some(mut group) = slot.held.pop_front() {
            slot.media.forward(&mut group, true);
        }
        *held_total -= slot.held_bytes;
        slot.held_bytes = 0;
    }

    fn hold_unattached(&mut self, peer: K, payload: Bytes, stats: &SrtIngressOwnerStats) {
        let len = payload.len();
        let dropped = |stats: &SrtIngressOwnerStats| {
            stats
                .media_unattached_dropped
                .fetch_add(1, Ordering::Relaxed);
        };
        // Per-peer quota: the peer gives up its own oldest payloads.
        let queue = self.unattached.entry(peer).or_default();
        while queue.bytes + len > self.limits.unattached_peer_bytes {
            let Some(oldest) = queue.payloads.pop_front() else {
                break;
            };
            queue.bytes -= oldest.len();
            self.unattached_bytes -= oldest.len();
            dropped(stats);
        }
        // Owner-wide cap: the largest queue gives up its oldest, so a newly
        // arriving peer is never starved by earlier ones.
        while self.unattached_bytes + len > self.limits.unattached_total_bytes {
            let Some(victim) = self
                .unattached
                .iter()
                .filter(|(_, queue)| !queue.payloads.is_empty())
                .max_by_key(|(_, queue)| queue.bytes)
                .map(|(key, _)| *key)
            else {
                break;
            };
            let queue = self.unattached.get_mut(&victim).expect("victim exists");
            if let Some(oldest) = queue.payloads.pop_front() {
                queue.bytes -= oldest.len();
                self.unattached_bytes -= oldest.len();
                dropped(stats);
            }
        }
        let queue = self.unattached.entry(peer).or_default();
        queue.bytes += len;
        queue.payloads.push_back(payload);
        self.unattached_bytes += len;
    }

    /// Tokio admitted the peer as a publisher: take over its media state and
    /// replay anything received while admission ran. Returns the stream probe
    /// if the replayed media completed one.
    pub(crate) fn attach_publisher(
        &mut self,
        peer: K,
        media: Box<SrtPublisherMedia>,
        stats: &SrtIngressOwnerStats,
    ) -> Option<DemuxProbe> {
        // Inserted before the replay, so the shared held-bytes budget always
        // matches the slots in the map, even if the replay unwinds and the
        // peer is discarded (see `discard`).
        let slot = self
            .publishers
            .entry(peer)
            .insert_entry(PublisherSlot {
                media,
                probe_pending: false,
                probe_revision: 0,
                held: VecDeque::new(),
                held_bytes: 0,
                probe_sent_at: None,
            })
            .into_mut();
        let mut probe = None;
        if let Some(early) = self.unattached.remove(&peer) {
            self.unattached_bytes -= early.bytes;
            for payload in early.payloads {
                if let Some(found) =
                    Self::accept(slot, &payload, &self.limits, &mut self.held_bytes, stats)
                {
                    probe = Some(found);
                }
                stats.media_payloads.fetch_add(1, Ordering::Relaxed);
            }
        }
        probe
    }

    pub(crate) fn attach_reader(&mut self, peer: K, reader: SrtReaderMedia) {
        self.forget_unattached(peer);
        self.readers.insert(peer, reader);
    }

    pub(crate) fn probe_identity(&self, peer: K) -> Option<ProbeIdentity> {
        let slot = self.publishers.get(&peer)?;
        Some(ProbeIdentity {
            attempt_id: slot.media.registration.attempt_id,
            revision: slot.probe_revision,
        })
    }

    /// Tokio applied the stream probe; `ring` replaces the publisher's ring.
    /// Held packets flow in their original order.
    pub(crate) fn probe_applied(
        &mut self,
        peer: K,
        identity: ProbeIdentity,
        ring: Option<Arc<RingBuffer>>,
        stats: &SrtIngressOwnerStats,
    ) {
        let Some(slot) = self.publishers.get_mut(&peer) else {
            return;
        };
        if identity.attempt_id != slot.media.registration.attempt_id
            || identity.revision != slot.probe_revision
        {
            return;
        }
        if let Some(sent_at) = slot.probe_sent_at.take() {
            let waited = u64::try_from(sent_at.elapsed().as_micros()).unwrap_or(u64::MAX);
            stats
                .media_probe_ack_max_us
                .fetch_max(waited, Ordering::Relaxed);
        }
        if let Some(ring) = ring {
            slot.media.ring_buffer = ring;
        }
        Self::release_held(slot, &mut self.held_bytes);
    }

    /// The peer ended: flush its publisher's media into the ring and drop
    /// every piece of state held for it.
    pub(crate) fn detach(&mut self, peer: K) {
        self.forget_unattached(peer);
        self.readers.remove(&peer);
        let Some(mut slot) = self.publishers.remove(&peer) else {
            return;
        };
        Self::release_held(&mut slot, &mut self.held_bytes);
        let mut media = slot.media;
        media.demuxer.flush();
        if media.demuxer.drain_into(&mut media.packets) > 0 {
            let mut packets = std::mem::take(&mut media.packets);
            media.forward(&mut packets, false);
        }
    }

    /// The peer's media work unwound: drop all of its state without the
    /// flush `detach` does (its demuxer state is suspect) and give back its
    /// share of the Owner-wide byte budgets, so other peers are unaffected.
    pub(crate) fn discard(&mut self, peer: K) {
        self.forget_unattached(peer);
        self.readers.remove(&peer);
        if let Some(slot) = self.publishers.remove(&peer) {
            self.held_bytes -= slot.held_bytes;
        }
    }

    fn forget_unattached(&mut self, peer: K) {
        if let Some(early) = self.unattached.remove(&peer) {
            self.unattached_bytes -= early.bytes;
        }
    }

    pub(crate) fn has_readers(&self) -> bool {
        !self.readers.is_empty()
    }

    /// Every attached publisher (for shutdown).
    pub(crate) fn publisher_peers(&self) -> Vec<K> {
        self.publishers.keys().copied().collect()
    }
}

/// Test-only fault point: a payload with this prefix unwinds out of the
/// demux step, as a parser bug would. `resume_unwind` skips the panic hook.
#[cfg(test)]
pub(crate) const INJECTED_PANIC_PAYLOAD: &[u8] = b"restream-test-injected-panic";

#[cfg(test)]
mod tests;
