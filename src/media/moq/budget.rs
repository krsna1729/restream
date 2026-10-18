//! The byte budget of a broadcast's moq-net cache.
//!
//! moq-net frees a cached group only when its pool is over its byte budget
//! or nobody has read or written the group for the pool's idle window
//! (`cache::DEFAULT_EXPIRY`, 30 s). A track's `max_age` bounds what a
//! subscriber waits for, not what is kept, so an origin without a budget
//! holds about 30 s of every track: 15 MB per broadcast at 4 Mbit/s,
//! proportionally more at higher rates.
//!
//! A live track's latest group is never evicted, whatever the budget, so a
//! late subscriber always joins at the current group. The budget bounds
//! everything else: [`MARGIN`] times the last complete group of every
//! track, i.e. the groups being written plus as much again for a reader
//! still draining the previous ones. It follows the media (one GOP of
//! video, one group of audio), so a broadcast keeps about two GOPs at any
//! bitrate instead of 30 s. Under the budget the pool's idle window still
//! applies.

use moq_net::{cache, origin};

/// Budget per byte of the last complete group of every track: the current
/// group plus one more.
const MARGIN: u64 = 2;

/// A moq-net origin for one broadcast, with the [`CacheBudget`] that sizes
/// its cache.
pub(crate) fn origin() -> (origin::Producer, origin::Driver, CacheBudget) {
    // A random hop and the default idle window.
    let config = origin::Config::default();
    let budget = CacheBudget {
        pool: config.pool.clone(),
        tracks: Vec::new(),
    };
    let (producer, driver) = origin::Producer::new(config);
    (producer, driver, budget)
}

/// Bytes of one track's groups.
#[derive(Debug, Clone, Copy, Default)]
struct TrackBytes {
    open: u64,
    last: u64,
}

/// Sizes an origin's cache from the groups its tracks carry: every frame
/// and group boundary of every track is reported here.
#[derive(Debug)]
pub(crate) struct CacheBudget {
    pool: cache::Pool,
    tracks: Vec<TrackBytes>,
}

impl CacheBudget {
    /// `bytes` were written into `track`'s open group.
    pub(crate) fn frame(&mut self, track: usize, bytes: usize) {
        let track = self.track(track);
        track.open = track.open.saturating_add(bytes as u64);
    }

    /// `track` opened its next group: the open one is complete.
    pub(crate) fn group(&mut self, track: usize) {
        let track = self.track(track);
        if track.open == 0 {
            return;
        }
        track.last = std::mem::take(&mut track.open);
        let groups = self
            .tracks
            .iter()
            .fold(0u64, |sum, track| sum.saturating_add(track.last));
        self.pool.resize(groups.saturating_mul(MARGIN));
    }

    /// Bytes the pool holds.
    #[cfg(test)]
    pub(crate) fn used(&self) -> u64 {
        self.pool.used()
    }

    fn track(&mut self, track: usize) -> &mut TrackBytes {
        if track >= self.tracks.len() {
            self.tracks.resize(track + 1, TrackBytes::default());
        }
        &mut self.tracks[track]
    }
}
