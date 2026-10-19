//! A MoQ broadcast fed from a ring, shared by the egress leaf (published to
//! a relay) and the origin (served to direct subscribers).
//!
//! moq-mux does the protocol work: its catalog producer publishes the hang
//! `catalog.json` (+ `catalog.json.z`) and the MSF `catalog` from one
//! catalog, and a `container::Producer` per media track manages groups:
//! video groups are GOPs (a keyframe opens one; frames before the first
//! keyframe are dropped), audio groups about one second. Every frame is
//! LOC, framed in place ([`LocBuffer::into_frame`]) and written as one
//! chunk, so every subscriber shares the same `Bytes`.
//!
//! Media tracks have `max_age` zero, so a subscriber skips a group as soon
//! as the next one starts: a late one joins at the current GOP and a slow
//! one skips whole old groups instead of falling behind. What the origin
//! keeps is bounded by its [`CacheBudget`] (about two GOPs; moq-net always
//! keeps the newest group).
//!
//! A track never rewinds behind a closed group's live edge (presentation
//! reordering inside a GOP is preserved). When the ring's timestamps do,
//! both tracks publish a discontinuity and share one timeline offset.

use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use hang::catalog::PRIORITY;
use moq_mux::container::{self, Frame, Kind, loc};
use moq_net::{Timestamp, broadcast, group, origin};

use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::journal::RingFeed;
use crate::media::moq::budget::CacheBudget;
use crate::media::moq::media::{AUDIO_TRACK, LocBuffer, TrackKind, VIDEO_TRACK, parse_loc_frame};
use crate::media::packet::{MediaPacket, MediaType};

use super::media::{MediaConverter, MediaFrame};

/// Audio group length.
const AUDIO_GROUP_US: u64 = 1_000_000;

/// Frames prepared in converter headroom, including an explicit LOC media
/// Timescale. Only the upstream producer's empty markers need framing here;
/// `loc::Wire` omits Timescale, which would instead mean Unix wall time.
struct Framed(loc::Wire);

impl container::Container for Framed {
    type Error = moq_mux::Error;

    fn write(&self, group: &mut group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
        for frame in frames {
            if frame.payload.is_empty() {
                let timestamp = frame.timestamp.convert(moq_net::Timescale::MICRO)?;
                let marker = LocBuffer::from_payload(&[])
                    .into_frame(timestamp.value())
                    .map_err(|_| moq_mux::Error::TimestampOverflow(moq_net::TimeOverflow))?;
                group.write_frame(frame.timestamp, marker)?;
            } else {
                group.write_frame(frame.timestamp, frame.payload.clone())?;
            }
        }
        Ok(())
    }

    fn poll_read(
        &self,
        group: &mut group::Consumer,
        waiter: &kio::Waiter,
    ) -> Poll<Result<Option<Vec<Frame>>, Self::Error>> {
        self.0.poll_read(group, waiter)
    }

    fn end(&self, frame: &Frame) -> Option<Timestamp> {
        self.0.end(frame)
    }

    fn kind(&self) -> Kind {
        self.0.kind()
    }

    fn finish_group(
        &self,
        group: &mut group::Producer,
        end: Option<Timestamp>,
    ) -> Result<(), Self::Error> {
        // Keep moq-mux's video duration-marker contract, but route its
        // marker through the same explicit media-time framing as media.
        if self.kind() == Kind::Video
            && let Some(timestamp) = end
        {
            self.write(
                group,
                &[Frame {
                    timestamp,
                    payload: bytes::Bytes::new(),
                    keyframe: false,
                    duration: None,
                }],
            )?;
        }
        Ok(())
    }
}

type MediaTrack = container::Producer<Framed>;

/// Why a frame was not written.
enum Refused {
    /// Below what the track already presented.
    Rewind,
    /// The broadcast is closed or broken.
    Fatal(String),
}

/// The broadcast timeline: ring timestamps plus `offset_us`, which grows
/// each time the ring's go back.
#[derive(Debug, Default)]
struct Timeline {
    offset_us: u64,
    /// Per track (video, audio): the furthest timestamp written, and the
    /// step that reached it.
    last: [Option<u64>; 2],
    step: [u64; 2],
}

impl Timeline {
    fn wrote(&mut self, track: usize, us: u64) {
        match self.last[track] {
            Some(last) if us <= last => {}
            last => {
                if let Some(last) = last {
                    self.step[track] = us - last;
                }
                self.last[track] = Some(us);
            }
        }
    }

    /// Just past what either track presented: its furthest timestamp plus
    /// one more step.
    fn end(&self) -> u64 {
        (0..2)
            .filter_map(|track| self.last[track].map(|last| last.saturating_add(self.step[track])))
            .max()
            .unwrap_or(0)
    }
}

/// The moq-net side of one broadcast.
pub(crate) struct HangBroadcast {
    broadcast: broadcast::Producer,
    catalog: moq_mux::catalog::Producer,
    video: Option<MediaTrack>,
    audio: Option<MediaTrack>,
    /// Where the open audio group started, on the broadcast timeline.
    audio_group_us: u64,
    timeline: Timeline,
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
        let mut broadcast = origin.create_broadcast(name)?;
        let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default())?;
        let media = |name: &str, priority: u8, kind: Kind| {
            broadcast
                .create_track(
                    name,
                    hang::container::track_info(priority).with_max_age(Duration::ZERO),
                )
                .map(|track| container::Producer::new(track, Framed(loc::Wire(kind))))
        };
        let video = video
            .then(|| media(VIDEO_TRACK, PRIORITY.video, Kind::Video))
            .transpose()?;
        let audio = audio
            .then(|| media(AUDIO_TRACK, PRIORITY.audio, Kind::Audio))
            .transpose()?;
        broadcast.announce(Default::default())?;
        Ok(Self {
            broadcast,
            catalog,
            video,
            audio,
            audio_group_us: 0,
            timeline: Timeline::default(),
            budget,
        })
    }

    /// Publish `catalog` on both catalog tracks (hang and MSF).
    pub(crate) fn publish_catalog(&mut self, catalog: &hang::Catalog) -> Result<(), String> {
        // The whole catalog, so no wall-clock mapping is advertised: ring
        // timestamps are media time.
        self.catalog
            .mutate(|published| *published = catalog.clone())
            .map_err(|error| error.to_string())
    }

    /// Write one frame, opening a group where the profile starts one.
    /// Returns whether it was written (video before its first keyframe, or
    /// after a discontinuity until the next one, is not).
    pub(crate) fn write(&mut self, frame: MediaFrame) -> Result<bool, String> {
        let has_track = match frame.track {
            TrackKind::Video => self.video.is_some(),
            TrackKind::Audio => self.audio.is_some(),
        };
        if !has_track {
            return Ok(false);
        }
        let us = frame.pts_us.saturating_add(self.timeline.offset_us);
        let framed = frame
            .data
            .into_frame(us)
            .map_err(|error| error.to_string())?;
        match self.write_framed(frame.track, frame.keyframe, us, framed.clone()) {
            Err(Refused::Rewind) => {}
            Err(Refused::Fatal(reason)) => return Err(reason),
            Ok(written) => return Ok(written),
        }
        // The ring went back: a discontinuity on both tracks, then resume
        // just past what either presented. Copies this one frame to frame
        // it again.
        self.discontinuity()?;
        let resume = self.timeline.end();
        self.timeline.offset_us = resume.saturating_sub(frame.pts_us);
        let (_, payload) = parse_loc_frame(framed).map_err(|error| error.to_string())?;
        let framed = LocBuffer::from_payload(&payload)
            .into_frame(resume)
            .map_err(|error| error.to_string())?;
        match self.write_framed(frame.track, frame.keyframe, resume, framed) {
            Ok(written) => Ok(written),
            Err(Refused::Rewind) => Ok(false),
            Err(Refused::Fatal(reason)) => Err(reason),
        }
    }

    fn write_framed(
        &mut self,
        track: TrackKind,
        keyframe: bool,
        us: u64,
        framed: bytes::Bytes,
    ) -> Result<bool, Refused> {
        let timestamp =
            Timestamp::from_micros(us).map_err(|error| Refused::Fatal(error.to_string()))?;
        let len = framed.len();
        let (producer, index, keyframe) = match track {
            TrackKind::Video => (self.video.as_mut(), 0, keyframe),
            TrackKind::Audio => {
                let producer = self.audio.as_mut();
                let start = self.audio_group_us;
                let next = producer
                    .as_ref()
                    .is_some_and(|producer| producer.needs_keyframe())
                    || us < start
                    || us - start >= AUDIO_GROUP_US;
                (producer, 1, next)
            }
        };
        let Some(producer) = producer else {
            return Ok(false);
        };
        let written = producer.write(Frame {
            timestamp,
            payload: framed,
            keyframe,
            duration: None,
        });
        match written {
            Ok(()) => {}
            Err(moq_mux::Error::MissingKeyframe(_)) => return Ok(false),
            Err(moq_mux::Error::TimestampRewind(_)) => return Err(Refused::Rewind),
            Err(error) => return Err(Refused::Fatal(error.to_string())),
        }
        if keyframe {
            self.budget.group(index);
            if index == 1 {
                self.audio_group_us = us;
            }
        }
        self.budget.frame(index, len);
        self.timeline.wrote(index, us);
        Ok(true)
    }

    /// Close every track's group with a discontinuity marker.
    fn discontinuity(&mut self) -> Result<(), String> {
        for (index, producer) in [(0, self.video.as_mut()), (1, self.audio.as_mut())] {
            if let Some(producer) = producer {
                producer
                    .discontinuity()
                    .map_err(|error| error.to_string())?;
                self.budget.group(index);
            }
        }
        Ok(())
    }

    /// End every track and close the broadcast: a clean end for every
    /// subscriber. Repeating is a no-op; dropping does it too, so a cancelled
    /// pump still ends its subscribers cleanly.
    pub(crate) fn finish(&mut self) {
        for producer in [self.video.as_mut(), self.audio.as_mut()]
            .into_iter()
            .flatten()
        {
            let _ = producer.finish();
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
    /// Units consumed (some may have produced no frame) and payload bytes
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
    frames: Vec<MediaFrame>,
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
            let len = frame.data.payload().len();
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
