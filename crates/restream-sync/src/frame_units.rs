//! Frame-unit metadata recorded by the ring for every publication.

use arc_swap::ArcSwapOption;
use bytes::Bytes;
use restream_core::{CONFIG_HISTORY, CONFIG_TRACKS_PER_TYPE, MediaType, NotRetained, TrackConfig};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

struct TrackConfigSlot {
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

    #[inline]
    pub fn generation(&self, media_type: MediaType, track_index: u32) -> u32 {
        self.slot(media_type, track_index)
            .map_or(0, |slot| slot.generation.load(Ordering::Relaxed))
    }

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

    pub fn clear(&self, media_type: MediaType, track_index: u32) {
        let Some(slot) = self.slot(media_type, track_index) else {
            return;
        };
        if slot.current.swap(None).is_some() {
            slot.generation.fetch_add(1, Ordering::Release);
        }
    }

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

#[derive(Debug, Default)]
pub struct VideoConfigFollower {
    generation: u32,
}

impl VideoConfigFollower {
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
