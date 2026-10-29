//! Frame-unit metadata recorded by the ring for every publication.
//!
//! Each slot carries, next to its packet, the configuration generation of the
//! packet's track at publication, in one `AtomicU64` per slot tagged with the
//! low bits of the packet's sequence, so the side table is keyed by ring
//! sequence, is bounded by the ring capacity and is overwritten together with
//! the packet, and a reader detects the overwrite from the word itself.
//! `MediaPacket` keeps its pinned 56-byte layout.
//!
//! Track configurations (codec configuration bytes, e.g. Annex B parameter
//! sets) live in a fixed table per ring. A generation increments only when a
//! track's bytes change; the last [`CONFIG_HISTORY`] configurations of a track
//! stay resolvable, so a reader behind a configuration change still gets the
//! bytes that apply to its packet.

use arc_swap::ArcSwapOption;
use bytes::Bytes;
use restream_core::{CONFIG_HISTORY, CONFIG_TRACKS_PER_TYPE, MediaType, NotRetained, TrackConfig};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

struct TrackConfigSlot {
    /// Latest generation; written by the producer, read on its hot path.
    generation: AtomicU32,
    current: ArcSwapOption<TrackConfig>,
    history: [ArcSwapOption<TrackConfig>; CONFIG_HISTORY],
}

impl TrackConfigSlot {
    fn new() -> Self {
        Self {
            generation: AtomicU32::new(0),
            current: ArcSwapOption::empty(),
            history: std::array::from_fn(|_| ArcSwapOption::empty()),
        }
    }

    fn history_slot(&self, generation: u32) -> &ArcSwapOption<TrackConfig> {
        &self.history[generation as usize % CONFIG_HISTORY]
    }
}

/// Per-track configurations of one ring. Single writer: the ring's producer.
pub struct TrackConfigs {
    tracks: [TrackConfigSlot; 2 * CONFIG_TRACKS_PER_TYPE],
}

impl Default for TrackConfigs {
    fn default() -> Self {
        Self::new()
    }
}

impl TrackConfigs {
    pub fn new() -> Self {
        Self {
            tracks: std::array::from_fn(|_| TrackConfigSlot::new()),
        }
    }

    fn slot(&self, media_type: MediaType, track_index: u32) -> Option<&TrackConfigSlot> {
        let track = usize::try_from(track_index)
            .ok()
            .filter(|track| *track < CONFIG_TRACKS_PER_TYPE)?;
        self.tracks
            .get(media_type as usize * CONFIG_TRACKS_PER_TYPE + track)
    }

    /// The generation a packet of this track publishes with. Producer only.
    #[inline]
    pub fn generation(&self, media_type: MediaType, track_index: u32) -> u32 {
        self.slot(media_type, track_index)
            .map_or(0, |slot| slot.generation.load(Ordering::Relaxed))
    }

    /// Record `bytes` as the track's configuration. Same bytes keep the
    /// generation; different bytes start the next one. Returns the track's
    /// generation afterwards (0 for a track outside the table).
    pub fn set(&self, media_type: MediaType, track_index: u32, bytes: Vec<u8>) -> u32 {
        let Some(slot) = self.slot(media_type, track_index) else {
            return 0;
        };
        let current = slot.generation.load(Ordering::Relaxed);
        if slot
            .current
            .load()
            .as_ref()
            .is_some_and(|config| config.bytes == bytes)
        {
            return current;
        }
        let next = current.wrapping_add(1);
        let config = Arc::new(TrackConfig {
            generation: next,
            bytes: Bytes::from(bytes),
        });
        slot.history_slot(next).store(Some(Arc::clone(&config)));
        slot.current.store(Some(config));
        slot.generation.store(next, Ordering::Release);
        next
    }

    /// Remove the current configuration without reusing its generation when
    /// the track returns. Historical packets keep resolving their own bytes.
    pub fn clear(&self, media_type: MediaType, track_index: u32) {
        let Some(slot) = self.slot(media_type, track_index) else {
            return;
        };
        if slot.current.swap(None).is_some() {
            slot.generation.fetch_add(1, Ordering::Release);
        }
    }

    /// The configuration of `generation`, if the track still retains it.
    pub fn get(
        &self,
        media_type: MediaType,
        track_index: u32,
        generation: u32,
    ) -> Option<Arc<TrackConfig>> {
        if generation == 0 {
            return None;
        }
        let config = self
            .slot(media_type, track_index)?
            .history_slot(generation)
            .load_full()?;
        (config.generation == generation).then_some(config)
    }

    pub fn current(&self, media_type: MediaType, track_index: u32) -> Option<Arc<TrackConfig>> {
        self.slot(media_type, track_index)?.current.load_full()
    }

    /// Take over every track's generation and retained configurations from
    /// the ring this one replaces, before this ring is reachable.
    pub fn copy_from(&self, previous: &Self) {
        for (slot, old) in self.tracks.iter().zip(&previous.tracks) {
            for (entry, old_entry) in slot.history.iter().zip(&old.history) {
                entry.store(old_entry.load_full());
            }
            slot.current.store(old.current.load_full());
            slot.generation
                .store(old.generation.load(Ordering::Acquire), Ordering::Release);
        }
    }
}

/// Copies a source ring's video configuration into a ring that republishes
/// the source's video packets (audio router, recirculation), so the target
/// starts a generation at the packet where the source did.
#[derive(Debug, Default)]
pub struct VideoConfigFollower {
    /// Source generation last copied into the target.
    generation: u32,
}

impl VideoConfigFollower {
    /// Call before queueing the source's video packet published at
    /// `sequence` for the target. When its configuration generation differs
    /// from the one last copied, `pending` (the packets queued ahead of it)
    /// is published first and the configuration recorded. A sequence (or a
    /// generation) the source no longer retains copies the source's current
    /// configuration.
    pub fn follow(
        &mut self,
        source: &crate::ring_buffer::RingBuffer,
        sequence: usize,
        target: &crate::ring_buffer::RingBuffer,
        pending: &mut Vec<restream_core::MediaPacket>,
    ) {
        let config = match source.frame_unit(sequence) {
            Some(unit) if unit.config_generation == self.generation => return,
            Some(_) => match source.track_config_at(sequence, MediaType::Video, 0) {
                Ok(config) => config,
                Err(NotRetained) => source.video_config(),
            },
            None => source.video_config(),
        };
        let Some(config) = config.filter(|config| config.generation != self.generation) else {
            return;
        };
        target.push_drained_batch_capped(pending);
        target.set_video_parameter_sets(config.bytes.to_vec());
        self.generation = config.generation;
    }
}
