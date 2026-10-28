//! The byte budget of a broadcast's moq-net cache.

use moq_net::{cache, origin};

const MARGIN: u64 = 2;

pub fn origin() -> (origin::Producer, origin::Driver, CacheBudget) {
    let config = origin::Config::default();
    let budget = CacheBudget {
        pool: config.pool.clone(),
        tracks: Vec::new(),
    };
    let (producer, driver) = origin::Producer::new(config);
    (producer, driver, budget)
}

#[derive(Debug, Clone, Copy, Default)]
struct TrackBytes {
    open: u64,
    last: u64,
}

#[derive(Debug)]
pub struct CacheBudget {
    pool: cache::Pool,
    tracks: Vec<TrackBytes>,
}

impl CacheBudget {
    pub fn frame(&mut self, track: usize, bytes: usize) {
        let track = self.track(track);
        track.open = track.open.saturating_add(bytes as u64);
    }

    pub fn group(&mut self, track: usize) {
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

    pub fn used(&self) -> u64 {
        self.pool.used()
    }

    fn track(&mut self, track: usize) -> &mut TrackBytes {
        if track >= self.tracks.len() {
            self.tracks.resize(track + 1, TrackBytes::default());
        }
        &mut self.tracks[track]
    }
}
