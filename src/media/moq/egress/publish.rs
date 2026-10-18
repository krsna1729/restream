//! A hang broadcast fed from a ring, shared by the egress leaf (published
//! to a relay) and the origin (served to direct subscribers).
//!
//! The broadcast carries `catalog.json` and the `video`/`audio` tracks.
//! Video groups are GOPs (a keyframe opens one; frames before the first
//! keyframe are dropped), audio groups about one second. Media tracks have
//! `max_age` zero, so a subscriber skips a group as soon as the next one
//! starts: a late one joins at the current GOP and a slow one skips whole
//! old groups instead of falling behind. What the origin keeps is bounded
//! by its [`CacheBudget`] (about two GOPs; moq-net always keeps the newest
//! group). Frames are written as one chunk each, so every subscriber shares
//! the same `Bytes`.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use hang::catalog::PRIORITY;
use moq_net::{Timestamp, broadcast, group, origin, track};

use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::journal::RingFeed;
use crate::media::moq::budget::CacheBudget;
use crate::media::moq::media::{AUDIO_TRACK, TrackKind, VIDEO_TRACK};
use crate::media::packet::{MediaPacket, MediaType};

use super::media::{HangFrame, MediaConverter};

/// Audio group length.
const AUDIO_GROUP_US: u64 = 1_000_000;

/// One media track and its open group.
struct TrackWriter {
    track: track::Producer,
    group: Option<group::Producer>,
    group_start_us: u64,
}

impl TrackWriter {
    fn new(track: track::Producer) -> Self {
        Self {
            track,
            group: None,
            group_start_us: 0,
        }
    }

    /// Close the open group and open the next one at `pts_us`.
    fn next_group(&mut self, pts_us: u64) -> Result<(), moq_net::Error> {
        if let Some(group) = self.group.take() {
            group.finish()?;
        }
        self.group_start_us = pts_us;
        self.group = Some(self.track.append_group()?);
        Ok(())
    }

    /// Close the open group and end the track (repeating is a no-op).
    fn finish(&mut self) {
        if let Some(group) = self.group.take() {
            let _ = group.finish();
        }
        let _ = self.track.finish();
    }
}

/// The moq-net side of one hang broadcast.
pub(crate) struct HangBroadcast {
    broadcast: broadcast::Producer,
    catalog: track::Producer,
    video: Option<TrackWriter>,
    audio: Option<TrackWriter>,
    budget: CacheBudget,
}

impl HangBroadcast {
    /// Create and announce broadcast `name` under `origin`, with the tracks
    /// asked for. `budget` is `origin`'s ([`crate::media::moq::budget::origin`]).
    pub(crate) fn create(
        origin: &origin::Producer,
        budget: CacheBudget,
        name: &str,
        video: bool,
        audio: bool,
    ) -> Result<Self, moq_net::Error> {
        let broadcast = origin.create_broadcast(name)?;
        let catalog = broadcast.create_track(
            hang::Catalog::DEFAULT_NAME,
            hang::Catalog::default_track_info(),
        )?;
        let media = |name: &str, priority: u8| {
            broadcast
                .create_track(
                    name,
                    hang::container::track_info(priority).with_max_age(Duration::ZERO),
                )
                .map(TrackWriter::new)
        };
        let video = video
            .then(|| media(VIDEO_TRACK, PRIORITY.video))
            .transpose()?;
        let audio = audio
            .then(|| media(AUDIO_TRACK, PRIORITY.audio))
            .transpose()?;
        broadcast.announce(Default::default())?;
        Ok(Self {
            broadcast,
            catalog,
            video,
            audio,
            budget,
        })
    }

    /// Publish `catalog` as the catalog track's next group.
    pub(crate) fn publish_catalog(&mut self, catalog: &hang::Catalog) -> Result<(), String> {
        let json = catalog.to_json().map_err(|error| error.to_string())?;
        let mut group = self
            .catalog
            .append_group()
            .map_err(|error| error.to_string())?;
        group
            .write_frame(Timestamp::now(), Bytes::from(json))
            .and_then(|()| group.finish())
            .map_err(|error| error.to_string())
    }

    /// Write one frame, opening a group where the profile starts one.
    /// Returns whether it was written (video before its first keyframe is
    /// not).
    pub(crate) fn write(&mut self, frame: HangFrame) -> Result<bool, String> {
        let timestamp = Timestamp::from_micros(frame.pts_us).map_err(|error| error.to_string())?;
        let (writer, budget_track) = match frame.track {
            TrackKind::Video => (self.video.as_mut(), 0),
            TrackKind::Audio => (self.audio.as_mut(), 1),
        };
        let Some(writer) = writer else {
            return Ok(false);
        };
        let open = writer.group.is_some();
        let next_group = match frame.track {
            TrackKind::Video if frame.keyframe => true,
            TrackKind::Video => false,
            // A timestamp jump backwards (a new ingest session) also starts
            // a group.
            TrackKind::Audio => {
                !open
                    || frame.pts_us < writer.group_start_us
                    || frame.pts_us - writer.group_start_us >= AUDIO_GROUP_US
            }
        };
        if next_group {
            writer
                .next_group(frame.pts_us)
                .map_err(|error| error.to_string())?;
            self.budget.group(budget_track);
        }
        // No open group: video before its first keyframe.
        let Some(group) = writer.group.as_mut() else {
            return Ok(false);
        };
        let len = frame.data.len();
        group
            .write_frame(timestamp, frame.data)
            .map_err(|error| error.to_string())?;
        self.budget.frame(budget_track, len);
        Ok(true)
    }

    /// End every track and close the broadcast: a clean end for every
    /// subscriber. Repeating is a no-op; dropping does it too, so a cancelled
    /// pump still ends its subscribers cleanly.
    pub(crate) fn finish(&mut self) {
        for writer in [self.video.as_mut(), self.audio.as_mut()]
            .into_iter()
            .flatten()
        {
            writer.finish();
        }
        let _ = self.catalog.finish();
        self.broadcast.close();
    }
}

impl Drop for HangBroadcast {
    fn drop(&mut self) {
        self.finish();
    }
}

/// What one [`RingPublisher::pump`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pump {
    /// Units consumed (some may have produced no frame) and frame bytes
    /// written.
    Sent { bytes: usize, units: usize },
    /// Nothing new in the feed.
    Empty,
    /// The cursor fell out of the feed; resynchronize at a sync point.
    Overrun,
    /// The broadcast refused a write: it is closed.
    Failed(String),
}

/// Ring units → [`HangBroadcast`].
pub(crate) struct RingPublisher {
    converter: MediaConverter,
    broadcast: HangBroadcast,
    /// The converter catalog version last published.
    published: Option<u64>,
    units: Vec<Arc<MediaPacket>>,
    frames: Vec<HangFrame>,
    skipped: u64,
}

impl RingPublisher {
    pub(crate) fn new(converter: MediaConverter, broadcast: HangBroadcast) -> Self {
        Self {
            converter,
            broadcast,
            published: None,
            units: Vec::new(),
            frames: Vec::new(),
            skipped: 0,
        }
    }

    /// Units the converter refused (unsupported or truncated tags).
    #[cfg(test)]
    pub(crate) fn skipped_units(&self) -> u64 {
        self.skipped
    }

    pub(crate) fn broadcast_mut(&mut self) -> &mut HangBroadcast {
        &mut self.broadcast
    }

    /// Read up to `budget` units at `cursor` and publish them; `cursor`
    /// advances past every unit read.
    pub(crate) fn pump(
        &mut self,
        feed: &RingFeed,
        cursor: &mut FeedCursor,
        budget: ReadBudget,
    ) -> Pump {
        let mut units = std::mem::take(&mut self.units);
        let next_cursor = match feed.read_from_into(*cursor, budget, &mut units) {
            FeedRead::Units { next_cursor, .. } => next_cursor,
            FeedRead::Empty => {
                self.units = units;
                return Pump::Empty;
            }
            FeedRead::Overrun { .. } | FeedRead::EpochMismatch { .. } => {
                self.units = units;
                return Pump::Overrun;
            }
        };
        let first = cursor.next_sequence;
        let mut outcome = Pump::Sent {
            bytes: 0,
            units: units.len(),
        };
        for (sequence, packet) in (first..).zip(units.iter()) {
            match self.publish_unit(feed, sequence, packet) {
                Ok(bytes) => {
                    if let Pump::Sent { bytes: sent, .. } = &mut outcome {
                        *sent += bytes;
                    }
                }
                Err(pump) => {
                    outcome = pump;
                    break;
                }
            }
        }
        units.clear();
        self.units = units;
        if matches!(outcome, Pump::Sent { .. }) {
            *cursor = next_cursor;
        }
        outcome
    }

    fn publish_unit(
        &mut self,
        feed: &RingFeed,
        sequence: u64,
        packet: &MediaPacket,
    ) -> Result<usize, Pump> {
        let config = if self.converter.wants_track_config(packet) {
            feed.track_config_at(sequence, MediaType::Video, 0)
                .map_err(|_| Pump::Overrun)?
        } else {
            None
        };
        if let Err(error) = self
            .converter
            .convert(packet, config.as_deref(), &mut self.frames)
        {
            self.skipped += 1;
            tracing::debug!(%error, "MoQ broadcast skipped a ring unit");
        }
        if self.published != Some(self.converter.catalog_version()) {
            match self.converter.catalog() {
                Ok(Some(catalog)) => {
                    self.broadcast
                        .publish_catalog(&catalog)
                        .map_err(Pump::Failed)?;
                    self.published = Some(self.converter.catalog_version());
                }
                Ok(None) => {}
                Err(error) => {
                    // A record hang refuses: keep the last catalog; frames
                    // only flow once one was published.
                    tracing::debug!(%error, "MoQ catalog not publishable");
                }
            }
        }
        let mut bytes = 0;
        for frame in self.frames.drain(..) {
            if self.published.is_none() {
                continue;
            }
            let len = frame.data.len();
            if self.broadcast.write(frame).map_err(Pump::Failed)? {
                bytes += len;
            }
        }
        Ok(bytes)
    }
}

/// Where a reader of `feed` starts: its latest sync point, else the live
/// edge (the egress fabric's own rule).
pub(crate) fn live_start(feed: &RingFeed) -> FeedCursor {
    let edge = feed.head_sequence();
    feed.latest_sync_point()
        .unwrap_or_else(|| FeedCursor::new(feed.epoch(), edge))
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
