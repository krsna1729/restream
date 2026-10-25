//! The stream inspector: per track, how the source's timestamps behave and
//! how audio keeps time with video, read from the pipeline's source ring.
//!
//! One OS thread for every pipeline ([`StreamInspector`]) wakes once a
//! second and reads the packet headers each source ring gained since it
//! last looked, with the time each was published. The ring's writer does
//! nothing for it: no counter, no copy, no wake. It reads only packets at
//! least a quarter of the ring behind the writer, and keeps one only while
//! the ring's sequence tag ([`RingBuffer::frame_unit`]) still names it; a
//! packet it could not reach in time is counted as unread, never guessed.
//!
//! Per track it counts, over the last minute:
//! - DTS going backwards (an encoder or relay reset its clock);
//! - jumps: DTS moving more than [`JUMP_MS`] further, or less far, than the
//!   time between arrivals (a gap in the media clock, not in the network:
//!   a publisher that stalls and resumes moves both);
//! - PTS before DTS (a frame shown before it can be decoded).
//!
//! Two audio timings:
//! - interleave: how much later than the first video track each audio track
//!   arrives against its timestamps (the smallest `arrival - DTS` over the
//!   last few seconds, the packet that met the least network delay). A
//!   muxer sends packets in timestamp order, so this is how far apart it
//!   keeps the two; a large one makes players wait.
//! - clock drift: whether an AAC track's timestamps keep pace with its
//!   samples. Every AAC frame is 1024 samples, so N frames must span
//!   N x 1024 / rate of timestamps. An encoder that stamps audio from the
//!   system clock while the sound card runs at its own rate drifts here,
//!   and lip sync slides by as much. A muxer's interleaving hides this from
//!   arrival times; the sample count does not.
//!
//! Per video track it also reads closed captions (CEA-608/708 in SEI,
//! [`crate::media::codec::frame_captions`], which stops at the frame's
//! first picture slice): which kinds were seen and how long ago, so a
//! source whose captions stop is noticed.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::media::packet::MediaType;
use crate::media::ring_buffer::RingBuffer;

/// DTS and arrival moving apart by more than this, between two packets of a
/// track, is a jump in the media clock (milliseconds).
pub const JUMP_MS: i64 = 1_000;
/// Counts cover this much time.
pub const WINDOW_MS: u64 = 60_000;
/// The audio/video offset is the smallest seen over this long.
const OFFSET_WINDOW_MS: u64 = 5_000;
/// Clock drift needs this much continuous audio to say anything.
pub const DRIFT_MIN_SPAN_MS: i64 = 20_000;
/// Timestamps that disagree with the sample count by more than this share
/// are not drift (a frame size the inspector got wrong, such as HE-AAC
/// declared at its core rate): nothing is reported.
const DRIFT_PLAUSIBLE: f64 = 0.05;
/// Samples in one AAC frame.
pub const AAC_FRAME_SAMPLES: f64 = 1024.0;
/// How often the thread reads the rings.
pub const SCAN_INTERVAL: Duration = Duration::from_secs(1);

/// One track's timestamps over the last minute.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackInspection {
    /// `video` or `audio`.
    pub media: String,
    pub track: u32,
    pub packets_per_sec: f64,
    /// DTS that went backwards.
    pub backwards: u32,
    /// DTS that moved more than [`JUMP_MS`] apart from arrival time.
    pub jumps: u32,
    /// Packets whose PTS came before their DTS.
    pub pts_before_dts: u32,
    /// The largest jump, signed: forward is positive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub largest_jump_ms: Option<i64>,
    /// Audio only: how much later than the video its media arrives,
    /// relative to its timestamps (milliseconds; negative is earlier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub av_offset_ms: Option<i64>,
    /// AAC only: how far its timestamps move from its sample count each
    /// minute (positive: they run fast), over its clock's last unbroken
    /// run of at least [`DRIFT_MIN_SPAN_MS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_drift_ms_per_min: Option<f64>,
    /// Video only: the closed captions in its SEI, once any were seen on
    /// this source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captions: Option<CaptionsInspection>,
}

/// A video track's closed captions (CEA-608/708 in SEI).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionsInspection {
    pub cea608: bool,
    pub cea708: bool,
    /// Seconds since a frame last carried captions (0: in the last read).
    pub last_seen_sec: f64,
    /// Seconds since a frame last carried caption text rather than padding;
    /// absent until one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_text_sec: Option<f64>,
}

/// A pipeline's source as the inspector last read it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInspection {
    /// Video tracks first, then audio, each in track order.
    pub tracks: Vec<TrackInspection>,
    /// Seconds the counts cover (up to a minute, less after a restart).
    pub window_sec: f64,
    /// Packets the inspector could not read before the ring reused their
    /// slots, over the window: the counts miss what those held.
    pub unread: u64,
}

/// One read of one track: what it saw since the previous read.
#[derive(Debug, Clone, Copy, Default)]
struct Read {
    at_ms: u64,
    packets: u32,
    backwards: u32,
    jumps: u32,
    pts_before_dts: u32,
    largest_jump_ms: Option<i64>,
    /// Smallest `arrival - DTS` in this read (milliseconds).
    min_offset_ms: Option<i64>,
    /// What this read's frames carried as captions.
    captions: crate::media::codec::FrameCaptions,
}

/// A video track's captions since the inspector started reading the
/// source: kinds seen, and when (read times, ms) they and text were last
/// seen. Kept past the window, so a dropout keeps being reported.
#[derive(Debug, Clone, Copy, Default)]
struct CaptionsSeen {
    cea608: bool,
    cea708: bool,
    last_ms: Option<u64>,
    last_text_ms: Option<u64>,
}

/// An audio track's clock since it last broke: its first frame's DTS, the
/// frames after it, the last DTS and the frame length (ms).
#[derive(Debug, Clone, Copy)]
struct ClockRun {
    anchor_dts: i64,
    frames: u64,
    last_dts: i64,
    frame_ms: f64,
}

impl ClockRun {
    /// Timestamps minus sample time per minute, once the run is long
    /// enough and the two agree within reason.
    fn drift_per_min(&self) -> Option<f64> {
        let span = self.last_dts - self.anchor_dts;
        if span < DRIFT_MIN_SPAN_MS {
            return None;
        }
        let expected = self.frames as f64 * self.frame_ms;
        let drift = span as f64 - expected;
        (drift.abs() <= expected * DRIFT_PLAUSIBLE).then(|| drift * 60_000.0 / span as f64)
    }
}

#[derive(Debug, Default)]
struct Track {
    /// DTS and arrival (µs) of the last packet, while they are continuous.
    last: Option<(i64, u64)>,
    clock: Option<ClockRun>,
    captions: CaptionsSeen,
    current: Read,
    reads: VecDeque<Read>,
}

impl Track {
    /// One packet; `frame_ms` is set for a whole AAC frame (its length).
    fn observe(&mut self, pts: i64, dts: i64, arrival_us: u64, frame_ms: Option<f64>) {
        let continuous = self.last.is_some();
        let read = &mut self.current;
        let before = (read.backwards, read.jumps);
        read.packets = read.packets.saturating_add(1);
        if pts < dts {
            read.pts_before_dts = read.pts_before_dts.saturating_add(1);
        }
        if let Some((last_dts, last_arrival_us)) = self.last {
            if dts < last_dts {
                read.backwards = read.backwards.saturating_add(1);
            } else {
                let media_ms = dts - last_dts;
                let wall_ms = (arrival_us.saturating_sub(last_arrival_us) / 1_000) as i64;
                let jump_ms = media_ms - wall_ms;
                if jump_ms.abs() > JUMP_MS {
                    read.jumps = read.jumps.saturating_add(1);
                    if read
                        .largest_jump_ms
                        .is_none_or(|largest| jump_ms.abs() > largest.abs())
                    {
                        read.largest_jump_ms = Some(jump_ms);
                    }
                }
            }
        }
        let offset_ms = (arrival_us / 1_000) as i64 - dts;
        read.min_offset_ms = Some(
            read.min_offset_ms
                .map_or(offset_ms, |min| min.min(offset_ms)),
        );
        let broke = !continuous || (read.backwards, read.jumps) != before;
        self.last = Some((dts, arrival_us));
        if let Some(frame_ms) = frame_ms {
            self.clock = match self.clock {
                Some(run) if !broke && run.frame_ms == frame_ms => Some(ClockRun {
                    frames: run.frames + 1,
                    last_dts: dts,
                    ..run
                }),
                _ => Some(ClockRun {
                    anchor_dts: dts,
                    frames: 0,
                    last_dts: dts,
                    frame_ms,
                }),
            };
        }
    }

    /// Closes the current read at `at_ms` and drops reads older than the
    /// window.
    fn close(&mut self, at_ms: u64) {
        let mut read = std::mem::take(&mut self.current);
        read.at_ms = at_ms;
        if read.captions.any() {
            let seen = &mut self.captions;
            seen.cea608 |= read.captions.cea608;
            seen.cea708 |= read.captions.cea708;
            seen.last_ms = Some(at_ms);
            if read.captions.text {
                seen.last_text_ms = Some(at_ms);
            }
        }
        self.reads.push_back(read);
        while self
            .reads
            .front()
            .is_some_and(|oldest| at_ms.saturating_sub(oldest.at_ms) > WINDOW_MS)
        {
            self.reads.pop_front();
        }
    }

    /// The smallest offset over the reads in `(from_ms, to_ms]`.
    fn min_offset(&self, from_ms: u64, to_ms: u64) -> Option<i64> {
        self.reads
            .iter()
            .filter(|read| read.at_ms > from_ms && read.at_ms <= to_ms)
            .filter_map(|read| read.min_offset_ms)
            .min()
    }
}

/// One pipeline's tracks, fed packet by packet and read once per scan.
#[derive(Debug, Default)]
pub struct Timeline {
    /// Keyed by (`kind_rank`, track index): video sorts first.
    tracks: HashMap<(u8, u32), Track>,
    /// When the counts start (the first read).
    since_ms: Option<u64>,
    unread: VecDeque<(u64, u64)>,
    pending_unread: u64,
}

const VIDEO: u8 = 0;
const AUDIO: u8 = 1;

fn kind_rank(media: MediaType) -> u8 {
    match media {
        MediaType::Video => VIDEO,
        MediaType::Audio => AUDIO,
    }
}

impl Timeline {
    /// One packet of `media`/`track`, published `arrival_us` into the
    /// ring's life. Timestamps are milliseconds; `frame_ms` is the length of
    /// a whole AAC frame (`None` for anything else).
    pub fn observe(
        &mut self,
        media: MediaType,
        track: u32,
        pts: i64,
        dts: i64,
        arrival_us: u64,
        frame_ms: Option<f64>,
    ) {
        self.tracks
            .entry((kind_rank(media), track))
            .or_default()
            .observe(pts, dts, arrival_us, frame_ms);
    }

    /// What one video frame of `track` carried as captions (after
    /// [`Self::observe`] for the same packet).
    pub fn observe_captions(&mut self, track: u32, captions: crate::media::codec::FrameCaptions) {
        if let Some(track) = self.tracks.get_mut(&(VIDEO, track)) {
            let read = &mut track.current.captions;
            read.cea608 |= captions.cea608;
            read.cea708 |= captions.cea708;
            read.text |= captions.text;
        }
    }

    /// `count` packets could not be read: what follows is not continuous
    /// with what came before, so no jump is counted across it.
    pub fn skipped(&mut self, count: u64) {
        self.pending_unread = self.pending_unread.saturating_add(count);
        for track in self.tracks.values_mut() {
            track.last = None;
            track.clock = None;
        }
    }

    /// Closes this read at `now_ms` and reports the window.
    pub fn inspect(&mut self, now_ms: u64) -> StreamInspection {
        let since_ms = *self.since_ms.get_or_insert(now_ms);
        self.unread
            .push_back((now_ms, std::mem::take(&mut self.pending_unread)));
        while self
            .unread
            .front()
            .is_some_and(|(at, _)| now_ms.saturating_sub(*at) > WINDOW_MS)
        {
            self.unread.pop_front();
        }
        for track in self.tracks.values_mut() {
            track.close(now_ms);
        }
        // A track with nothing in the window is gone (a new publisher with
        // fewer tracks).
        self.tracks
            .retain(|_, track| track.reads.iter().any(|read| read.packets > 0));

        let window_ms = now_ms.saturating_sub(since_ms).clamp(1, WINDOW_MS);
        let window_sec = window_ms as f64 / 1_000.0;
        let recent_from = now_ms.saturating_sub(OFFSET_WINDOW_MS);
        let mut keys: Vec<(u8, u32)> = self.tracks.keys().copied().collect();
        keys.sort_unstable();
        let video = keys
            .iter()
            .find(|(kind, _)| *kind == VIDEO)
            .and_then(|key| self.tracks.get(key));
        let video_now = video.and_then(|video| video.min_offset(recent_from, now_ms));
        let tracks = keys
            .iter()
            .filter_map(|key| {
                let track = self.tracks.get(key)?;
                let mut inspection = TrackInspection {
                    media: if key.0 == VIDEO { "video" } else { "audio" }.to_string(),
                    track: key.1,
                    ..TrackInspection::default()
                };
                let mut packets = 0u64;
                for read in &track.reads {
                    packets += u64::from(read.packets);
                    inspection.backwards = inspection.backwards.saturating_add(read.backwards);
                    inspection.jumps = inspection.jumps.saturating_add(read.jumps);
                    inspection.pts_before_dts = inspection
                        .pts_before_dts
                        .saturating_add(read.pts_before_dts);
                    if let Some(jump) = read.largest_jump_ms
                        && inspection
                            .largest_jump_ms
                            .is_none_or(|largest| jump.abs() > largest.abs())
                    {
                        inspection.largest_jump_ms = Some(jump);
                    }
                }
                inspection.packets_per_sec = packets as f64 / window_sec;
                let seen = track.captions;
                if key.0 == VIDEO
                    && let Some(last_ms) = seen.last_ms
                {
                    let age = |at_ms: u64| now_ms.saturating_sub(at_ms) as f64 / 1_000.0;
                    inspection.captions = Some(CaptionsInspection {
                        cea608: seen.cea608,
                        cea708: seen.cea708,
                        last_seen_sec: age(last_ms),
                        last_text_sec: seen.last_text_ms.map(age),
                    });
                }
                if key.0 == AUDIO {
                    let now = track.min_offset(recent_from, now_ms);
                    inspection.av_offset_ms =
                        now.zip(video_now).map(|(audio, video)| audio - video);
                    inspection.clock_drift_ms_per_min =
                        track.clock.and_then(|run| run.drift_per_min());
                }
                Some(inspection)
            })
            .collect();
        StreamInspection {
            tracks,
            window_sec,
            unread: self.unread.iter().map(|(_, count)| count).sum(),
        }
    }
}

/// Where a pipeline's reading of its ring stands.
struct Cursor {
    ring: Weak<RingBuffer>,
    next: usize,
    timeline: Timeline,
}

/// Reads the packets `ring` gained since `cursor.next` into the timeline.
/// Only packets at least a quarter of the ring behind the writer are read,
/// and only while the slot still holds them.
fn scan(
    ring: &RingBuffer,
    next: &mut usize,
    timeline: &mut Timeline,
    aac_frame_ms: &HashMap<u32, f64>,
) {
    let write_idx = ring.get_write_idx();
    let capacity = ring.capacity();
    let hevc = crate::domain::output_spec::VideoCodecKind::from_codec_name(ring.codec_hint_str())
        == crate::domain::output_spec::VideoCodecKind::Hevc;
    let readable_from = write_idx.saturating_sub(capacity - capacity / 4);
    if *next < readable_from {
        timeline.skipped((readable_from - *next) as u64);
        *next = readable_from;
    }
    while *next < write_idx {
        let sequence = *next;
        *next += 1;
        // Arrival first, the packet, then the tag: a slot the writer
        // reused while it was being read fails the tag.
        let arrival_us = ring.published_at_us(sequence);
        let packet = ring.read_at(sequence);
        match packet {
            Some(packet) if ring.frame_unit(sequence).is_some() => {
                let frame_ms = (packet.media_type == MediaType::Audio
                    && !is_flv_aac_config(&packet))
                .then(|| aac_frame_ms.get(&packet.track_index).copied())
                .flatten();
                timeline.observe(
                    packet.media_type,
                    packet.track_index,
                    packet.pts,
                    packet.dts,
                    arrival_us,
                    frame_ms,
                );
                if packet.media_type == MediaType::Video {
                    timeline.observe_captions(
                        packet.track_index,
                        crate::media::codec::frame_captions(packet.format, &packet.payload, hevc),
                    );
                }
            }
            _ => timeline.skipped(1),
        }
    }
}

/// An FLV AAC sequence header: configuration, not a frame.
fn is_flv_aac_config(packet: &crate::media::packet::MediaPacket) -> bool {
    packet.format == crate::media::packet::PayloadFormat::Flv
        && matches!(packet.payload.as_ref(), [tag, 0, ..] if tag >> 4 == 10)
}

/// The length of one AAC frame at `sample_rate`, ms.
pub fn aac_frame_ms(sample_rate: u32) -> Option<f64> {
    (sample_rate > 0).then(|| AAC_FRAME_SAMPLES * 1_000.0 / f64::from(sample_rate))
}

/// A pipeline the inspector reads: its source ring, and the AAC frame
/// length of each AAC audio track by index.
#[derive(Debug, Clone)]
pub struct Watched {
    pub pipeline_id: String,
    pub ring: Weak<RingBuffer>,
    pub aac_frame_ms: HashMap<u32, f64>,
}

#[derive(Default)]
struct Shared {
    /// The rings to read, set by the flight recorder each tick.
    rings: Mutex<Vec<Watched>>,
    inspections: Mutex<HashMap<String, StreamInspection>>,
    stop: AtomicBool,
}

/// The inspector thread and what it last found. Dropping it stops the
/// thread.
pub struct StreamInspector {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StreamInspector {
    /// Starts the thread; `None` when the OS refuses one (the inspector
    /// then reports nothing, and nothing else changes).
    pub fn start() -> Option<Self> {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("stream-inspector".to_string())
            .spawn(move || {
                // A panic here costs the inspection, never the engine.
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&worker)));
                if outcome.is_err() {
                    tracing::error!(
                        event_class = "lifecycle",
                        event_type = "restream.inspector.panicked",
                        "stream inspector stopped after a panic"
                    );
                }
            })
            .map_err(|error| {
                tracing::warn!(error = %error, "stream inspector thread did not start");
            })
            .ok()?;
        Some(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// The pipelines to read from now on.
    pub fn watch(&self, rings: Vec<Watched>) {
        *crate::sync::lock(&self.shared.rings) = rings;
    }

    /// A pipeline's latest inspection.
    pub fn inspection(&self, pipeline_id: &str) -> Option<StreamInspection> {
        crate::sync::lock(&self.shared.inspections)
            .get(pipeline_id)
            .cloned()
    }
}

impl Drop for StreamInspector {
    /// Asks the thread to stop and wakes it; it exits within one scan.
    /// Not joined: this runs on the recorder's Tokio task.
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
        }
    }
}

fn run(shared: &Shared) {
    let mut cursors: HashMap<String, Cursor> = HashMap::new();
    while !shared.stop.load(Ordering::Acquire) {
        std::thread::park_timeout(SCAN_INTERVAL);
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let rings = crate::sync::lock(&shared.rings).clone();
        let now_ms = crate::media::engine::MediaEngine::now_epoch_ms();
        let mut found = HashMap::with_capacity(rings.len());
        for Watched {
            pipeline_id,
            ring: weak,
            aac_frame_ms,
        } in rings
        {
            let Some(ring) = weak.upgrade() else {
                continue;
            };
            let cursor = cursors
                .entry(pipeline_id.clone())
                .or_insert_with(|| Cursor {
                    ring: weak.clone(),
                    // Start at the writer: history before the first look is
                    // not counted as unread.
                    next: ring.get_write_idx(),
                    timeline: Timeline::default(),
                });
            if !Weak::ptr_eq(&cursor.ring, &weak) {
                // A new ring (grown, or a new source): arrival times count
                // from its creation, so offsets read before do not compare.
                cursor.ring = weak;
                cursor.next = ring.get_write_idx();
                cursor.timeline = Timeline::default();
            }
            scan(&ring, &mut cursor.next, &mut cursor.timeline, &aac_frame_ms);
            found.insert(pipeline_id, cursor.timeline.inspect(now_ms));
        }
        cursors.retain(|pipeline_id, _| found.contains_key(pipeline_id));
        *crate::sync::lock(&shared.inspections) = found;
    }
}

#[cfg(test)]
#[path = "stream_inspector_tests.rs"]
mod tests;
