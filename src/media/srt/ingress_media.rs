//! SRT ingest media, run to completion on the ingress Owner thread: TS demux,
//! input gating, timestamp mapping, standby GOP caching and ring publication
//! for every publisher, plus direct SRT read/play. A received payload is
//! parsed on the thread that received it; nothing per packet crosses to Tokio
//! (`docs/srt-compio-roadmap.md` WI11, two-pool plan).
//!
//! Tokio keeps session lifecycle: it authenticates a `Connected` peer, builds
//! its [`SrtPublisherMedia`] (engine registration) and attaches it with a
//! command; it applies the one-time stream probe; it does the ingest
//! bookkeeping on `Disconnected`, after the Owner has flushed the media.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use srt_transport::advanced::admission::LogicalPeerId;

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::input_gate::InputTimestampMapper;
use crate::media::mpegts::{DemuxProbe, TsDemuxer};
use crate::media::packet::MediaPacket;
use crate::media::ring_buffer::RingBuffer;
use crate::media::snapshots::SrtIngressOwnerStats;
use crate::media::standby_gop::StandbyGopCache;
use crate::media::ts_chunk_ring::TsChunkReader;

#[path = "ingest_packets.rs"]
mod ingest_packets;

pub(crate) const SRT_MESSAGE_PAYLOAD_MAX: usize = 1316;

/// Media for a peer Tokio has not attached yet (admission is async) is held,
/// bounded: about 2 s at 8 Mbit/s per peer, plus a total cap. Overflow drops
/// the oldest payloads (counted in `mediaUnattachedDropped`).
const UNATTACHED_PER_PEER: usize = 2048;
const UNATTACHED_TOTAL: usize = 16_384;

/// Drained packet groups held while Tokio applies a publisher's stream probe
/// (the ring may be replaced). Past this, held groups go to the current ring
/// and the overflow is counted.
const PROBE_HOLD_GROUPS: usize = 4096;

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
    pub(super) ingest_metrics: Arc<crate::media::stage_metrics::StageMetrics>,
    pub(super) last_progress_ms: Arc<std::sync::atomic::AtomicU64>,
    pub(super) probe_sent: bool,
}

impl SrtPublisherMedia {
    fn forward(&mut self, packets: &mut Vec<MediaPacket>, keyframes: bool) {
        ingest_packets::forward_ingest_packets(
            packets,
            &self.ring_buffer,
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
}

impl SrtReaderMedia {
    pub(super) fn new(muxer: TsChunkReader) -> Self {
        Self {
            muxer,
            packets: Vec::with_capacity(crate::media::ring_buffer::MEDIA_PULL_BURST_PACKETS),
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
            .pull_burst(
                &mut self.packets,
                crate::media::ring_buffer::MEDIA_PULL_BURST_PACKETS,
            )
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
    probe_pending: bool,
    held: VecDeque<Vec<MediaPacket>>,
}

/// Every publisher's and reader's media state on the Owner thread, keyed by
/// the session handle (`LogicalPeerId` in production).
pub(crate) struct IngressMedia<K = LogicalPeerId> {
    publishers: HashMap<K, PublisherSlot>,
    unattached: HashMap<K, VecDeque<Bytes>>,
    unattached_total: usize,
    pub(super) readers: HashMap<K, SrtReaderMedia>,
}

impl<K> Default for IngressMedia<K> {
    fn default() -> Self {
        Self {
            publishers: HashMap::new(),
            unattached: HashMap::new(),
            unattached_total: 0,
            readers: HashMap::new(),
        }
    }
}

impl<K: std::hash::Hash + Eq + Copy> IngressMedia<K> {
    /// One received payload. Returns the publisher's first stream probe, which
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
        let probe = Self::accept(slot, &payload, stats);
        stats.media_payloads.fetch_add(1, Ordering::Relaxed);
        probe
    }

    fn accept(
        slot: &mut PublisherSlot,
        payload: &Bytes,
        stats: &SrtIngressOwnerStats,
    ) -> Option<DemuxProbe> {
        let media = &mut slot.media;
        media.demuxer.feed(payload.as_ref());
        if media.demuxer.drain_into(&mut media.packets) > 0 {
            if !slot.probe_pending {
                media.forward_drained();
            } else if slot.held.len() < PROBE_HOLD_GROUPS {
                slot.held.push_back(std::mem::take(&mut media.packets));
            } else {
                // Tokio is not answering: stop holding and publish to the
                // current ring rather than grow without bound.
                stats
                    .media_probe_hold_overflows
                    .fetch_add(1, Ordering::Relaxed);
                slot.probe_pending = false;
                while let Some(mut group) = slot.held.pop_front() {
                    media.forward(&mut group, true);
                }
                media.forward_drained();
            }
        }
        let len = payload.len() as u64;
        media.bytes_received.fetch_add(len, Ordering::Relaxed);
        media.ingest_metrics.record_in(len);
        media
            .last_progress_ms
            .store(MediaEngine::now_epoch_ms(), Ordering::Relaxed);
        if media.probe_sent {
            return None;
        }
        let probe = media.demuxer.take_probe()?;
        media.probe_sent = true;
        slot.probe_pending = true;
        Some(probe)
    }

    fn hold_unattached(&mut self, peer: K, payload: Bytes, stats: &SrtIngressOwnerStats) {
        let queue = self.unattached.entry(peer).or_default();
        if queue.len() >= UNATTACHED_PER_PEER || self.unattached_total >= UNATTACHED_TOTAL {
            stats
                .media_unattached_dropped
                .fetch_add(1, Ordering::Relaxed);
            if queue.pop_front().is_some() {
                self.unattached_total -= 1;
            } else {
                return;
            }
        }
        queue.push_back(payload);
        self.unattached_total += 1;
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
        let mut slot = PublisherSlot {
            media,
            probe_pending: false,
            held: VecDeque::new(),
        };
        let mut probe = None;
        if let Some(early) = self.unattached.remove(&peer) {
            self.unattached_total -= early.len();
            for payload in early {
                if let Some(found) = Self::accept(&mut slot, &payload, stats) {
                    probe = Some(found);
                }
                stats.media_payloads.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.publishers.insert(peer, slot);
        probe
    }

    pub(crate) fn attach_reader(&mut self, peer: K, reader: SrtReaderMedia) {
        self.forget_unattached(peer);
        self.readers.insert(peer, reader);
    }

    /// Tokio applied the stream probe; `ring` replaces the publisher's ring.
    /// Held packets flow in their original order.
    pub(crate) fn probe_applied(&mut self, peer: K, ring: Option<Arc<RingBuffer>>) {
        let Some(slot) = self.publishers.get_mut(&peer) else {
            return;
        };
        if let Some(ring) = ring {
            slot.media.ring_buffer = ring;
        }
        slot.probe_pending = false;
        while let Some(mut group) = slot.held.pop_front() {
            slot.media.forward(&mut group, true);
        }
    }

    /// The peer ended: flush its publisher's media into the ring and drop
    /// every piece of state held for it.
    pub(crate) fn detach(&mut self, peer: K) {
        self.forget_unattached(peer);
        self.readers.remove(&peer);
        let Some(mut slot) = self.publishers.remove(&peer) else {
            return;
        };
        while let Some(mut group) = slot.held.pop_front() {
            slot.media.forward(&mut group, true);
        }
        let mut media = slot.media;
        media.demuxer.flush();
        if media.demuxer.drain_into(&mut media.packets) > 0 {
            let mut packets = std::mem::take(&mut media.packets);
            media.forward(&mut packets, false);
        }
    }

    fn forget_unattached(&mut self, peer: K) {
        if let Some(early) = self.unattached.remove(&peer) {
            self.unattached_total -= early.len();
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

#[cfg(test)]
#[path = "ingress_media_tests.rs"]
mod tests;
