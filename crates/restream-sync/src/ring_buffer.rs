//! Lock-free single-producer multi-consumer ring buffer for media packet fan-out.

use arc_swap::ArcSwapOption;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use tracing::debug;

use restream_core::{
    AudioMeta, CONFIG_TRACKS_PER_TYPE, FrameUnit, MediaPacket, MediaType, NotRetained,
    PayloadFormat, TrackConfig, VideoCodecKind, annexb_parameter_sets,
};

use crate::frame_units::TrackConfigs;
use crate::lock::lock;
use crate::reader::{ReaderInfo, ReaderSnapshot};
use crate::subscribers::PublishSubscribers;

pub const DEFAULT_RING_CAPACITY: usize = 1024;
pub const MEDIA_PULL_BURST_PACKETS: usize = 32;
pub const MEDIA_PRODUCER_BATCH_PACKETS: usize = MEDIA_PULL_BURST_PACKETS;
pub const DEFAULT_TRANSCODER_RING_CAPACITY: usize = 512;

pub struct RingSlot {
    data: ArcSwapOption<MediaPacket>,
    published_at_us: AtomicU64,
    unit: AtomicU64,
}

#[repr(align(64))]
pub struct AlignedAtomicUsize {
    pub val: AtomicUsize,
}

pub const BURST_HIST_BUCKETS: usize = 6;
pub(crate) const fn burst_bucket(n: usize) -> usize {
    match n {
        1 => 0,
        2 => 1,
        3..=4 => 2,
        5..=8 => 3,
        9..=16 => 4,
        _ => 5,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PayloadStats {
    pub slots: usize,
    pub payload_bytes: usize,
    pub video_bytes: usize,
    pub audio_bytes: usize,
    pub min_payload_bytes: usize,
    pub max_payload_bytes: usize,
}

pub struct RingBuffer {
    pub(crate) slots: Vec<RingSlot>,
    pub write_idx: AlignedAtomicUsize,
    pub last_keyframe_idx: AlignedAtomicUsize,
    published_bytes: AtomicU64,
    pub(crate) capacity: usize,
    created_at: Instant,
    notify: Arc<tokio::sync::Notify>,
    pub readers: std::sync::Mutex<Vec<std::sync::Weak<ReaderInfo>>>,
    codec_hint: AtomicU8,
    initial_codec_hint: std::sync::OnceLock<String>,
    track_configs: TrackConfigs,
    pub audio_tracks: ArcSwapOption<Vec<AudioMeta>>,
    pub estimated_pkt_rate: std::sync::atomic::AtomicU32,
    end_of_stream: AtomicBool,
    pub next: ArcSwapOption<RingBuffer>,
    pub publish_subscribers: std::sync::OnceLock<Arc<PublishSubscribers>>,
    metadata_subscribers: std::sync::OnceLock<Arc<PublishSubscribers>>,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        let mut slots = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            slots.push(RingSlot {
                data: ArcSwapOption::empty(),
                published_at_us: AtomicU64::new(0),
                unit: AtomicU64::new(0),
            });
        }
        debug!(capacity, "ring buffer created");
        Self {
            slots,
            write_idx: AlignedAtomicUsize {
                val: AtomicUsize::new(0),
            },
            last_keyframe_idx: AlignedAtomicUsize {
                val: AtomicUsize::new(usize::MAX),
            },
            published_bytes: AtomicU64::new(0),
            capacity,
            created_at: Instant::now(),
            notify: Arc::new(tokio::sync::Notify::new()),
            readers: std::sync::Mutex::new(Vec::new()),
            codec_hint: AtomicU8::new(0),
            initial_codec_hint: std::sync::OnceLock::new(),
            track_configs: TrackConfigs::new(),
            audio_tracks: ArcSwapOption::empty(),
            estimated_pkt_rate: std::sync::atomic::AtomicU32::new(0),
            end_of_stream: AtomicBool::new(false),
            next: ArcSwapOption::empty(),
            publish_subscribers: std::sync::OnceLock::new(),
            metadata_subscribers: std::sync::OnceLock::new(),
        }
    }

    pub fn publication_subscribers(&self) -> Arc<PublishSubscribers> {
        Arc::clone(
            self.publish_subscribers
                .get_or_init(|| Arc::new(PublishSubscribers::new())),
        )
    }

    pub fn metadata_subscribers(&self) -> Arc<PublishSubscribers> {
        Arc::clone(
            self.metadata_subscribers
                .get_or_init(|| Arc::new(PublishSubscribers::new())),
        )
    }

    #[inline]
    fn wake_publication_subscribers(&self) {
        if let Some(subscribers) = self.publish_subscribers.get() {
            subscribers.wake_all();
        }
    }

    #[inline]
    fn wake_metadata_subscribers(&self) {
        if let Some(subscribers) = self.metadata_subscribers.get() {
            subscribers.wake_all();
        }
    }

    pub fn new_continuing(capacity: usize, start_write_idx: usize) -> Self {
        let ring = Self::new(capacity);
        ring.write_idx.val.store(start_write_idx, Ordering::Relaxed);
        ring
    }

    pub fn seed_readable_tail_from(&self, old_ring: &RingBuffer) -> usize {
        let old_write_idx = old_ring.get_write_idx();
        let tail_capacity = self
            .capacity
            .saturating_sub(1)
            .min(old_ring.capacity.saturating_sub(1));
        let start_idx = old_write_idx.saturating_sub(tail_capacity);
        let mut copied = 0usize;
        let mut last_keyframe_idx = usize::MAX;

        for idx in start_idx..old_write_idx {
            let Some(packet) = old_ring.read_at(idx) else {
                continue;
            };
            let slot_idx = idx % self.capacity;
            let unit = old_ring.slots[idx % old_ring.capacity]
                .unit
                .load(Ordering::Acquire);
            self.slots[slot_idx]
                .published_at_us
                .store(self.elapsed_us().max(1), Ordering::Release);
            self.slots[slot_idx].data.store(Some(packet.clone()));
            self.slots[slot_idx].unit.store(unit, Ordering::Release);
            if packet.media_type == MediaType::Video && packet.is_keyframe {
                last_keyframe_idx = idx;
            }
            copied += 1;
        }

        if last_keyframe_idx != usize::MAX {
            self.last_keyframe_idx
                .val
                .store(last_keyframe_idx, Ordering::Release);
        }
        self.track_configs.copy_from(&old_ring.track_configs);
        self.write_idx.val.store(old_write_idx, Ordering::Release);
        copied
    }

    pub fn seal_and_forward(&self, new_ring: Arc<RingBuffer>) {
        let _ = new_ring
            .publish_subscribers
            .set(self.publication_subscribers());
        if let Some(metadata) = self.metadata_subscribers.get() {
            let _ = new_ring.metadata_subscribers.set(Arc::clone(metadata));
        }
        self.next.store(Some(new_ring));
        self.notify.notify_waiters();
        self.wake_publication_subscribers();
        self.wake_metadata_subscribers();
    }

    pub fn mark_end_of_stream(&self) {
        self.end_of_stream.store(true, Ordering::Release);
        self.notify.notify_waiters();
        self.wake_publication_subscribers();
        self.wake_metadata_subscribers();
    }

    pub fn is_end_of_stream(&self) -> bool {
        self.end_of_stream.load(Ordering::Acquire)
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn active_reader_count(&self) -> usize {
        let mut readers = lock(&self.readers);
        readers.retain(|w| w.upgrade().is_some());
        readers.len()
    }

    pub fn set_estimated_pkt_rate(&self, pkt_per_sec: f64) {
        self.estimated_pkt_rate
            .store(pkt_per_sec.round() as u32, Ordering::Relaxed);
    }

    pub fn estimated_pkt_rate(&self) -> f64 {
        self.estimated_pkt_rate.load(Ordering::Relaxed) as f64
    }

    pub fn buffer_depth_secs(&self) -> Option<f64> {
        let rate = self.estimated_pkt_rate.load(Ordering::Relaxed);
        if rate == 0 {
            return None;
        }
        Some(self.capacity as f64 / rate as f64)
    }

    pub fn set_codec_hint(&self, codec: &str) {
        let initial = self.initial_codec_hint.get_or_init(|| codec.to_string());
        let hint = if codec.is_empty() {
            0
        } else if codec == initial {
            1
        } else {
            match VideoCodecKind::from_codec_name(codec) {
                VideoCodecKind::H264 => 2,
                VideoCodecKind::Hevc => 3,
                VideoCodecKind::Unknown => 1,
            }
        };
        if self.codec_hint.swap(hint, Ordering::AcqRel) != hint {
            self.wake_metadata_subscribers();
        }
    }

    pub fn codec_hint_str(&self) -> &str {
        match self.codec_hint.load(Ordering::Acquire) {
            0 => "",
            2 => "h264",
            3 => "hevc",
            _ => self
                .initial_codec_hint
                .get()
                .map(String::as_str)
                .unwrap_or(""),
        }
    }

    pub fn remove_video_track(&self) {
        self.track_configs.clear(MediaType::Video, 0);
        self.codec_hint.store(0, Ordering::Release);
        self.wake_metadata_subscribers();
    }

    pub fn set_track_config(&self, media_type: MediaType, track_index: u32, bytes: Vec<u8>) -> u32 {
        if bytes.is_empty() {
            return self.track_configs.generation(media_type, track_index);
        }
        let previous = self.track_configs.generation(media_type, track_index);
        let generation = self.track_configs.set(media_type, track_index, bytes);
        if generation != previous {
            self.wake_metadata_subscribers();
        }
        generation
    }

    pub fn track_config(
        &self,
        media_type: MediaType,
        track_index: u32,
    ) -> Option<Arc<TrackConfig>> {
        self.track_configs.current(media_type, track_index)
    }

    pub fn set_video_parameter_sets(&self, parameter_sets: Vec<u8>) {
        self.set_track_config(MediaType::Video, 0, parameter_sets);
    }

    pub fn video_config(&self) -> Option<Arc<TrackConfig>> {
        self.track_config(MediaType::Video, 0)
    }

    pub fn video_parameter_sets(&self) -> Option<Vec<u8>> {
        self.video_config().map(|config| config.bytes.to_vec())
    }

    pub fn frame_unit(&self, sequence: usize) -> Option<FrameUnit> {
        let write_idx = self.write_idx.val.load(Ordering::Acquire);
        if sequence >= write_idx || write_idx - sequence >= self.capacity {
            return None;
        }
        FrameUnit::unpack(
            self.slots[sequence % self.capacity]
                .unit
                .load(Ordering::Acquire),
            sequence,
        )
    }

    pub fn track_config_at(
        &self,
        sequence: usize,
        media_type: MediaType,
        track_index: u32,
    ) -> Result<Option<Arc<TrackConfig>>, NotRetained> {
        let unit = self.frame_unit(sequence).ok_or(NotRetained)?;
        if unit.config_generation == 0 {
            return Ok(None);
        }
        self.track_configs
            .get(media_type, track_index, unit.config_generation)
            .map(Some)
            .ok_or(NotRetained)
    }

    #[inline]
    fn next_frame_unit(&self, packet: &MediaPacket, sequence: usize) -> u64 {
        FrameUnit {
            config_generation: self
                .track_configs
                .generation(packet.media_type, packet.track_index),
        }
        .pack(sequence)
    }

    fn record_in_band_config(&self, packet: &MediaPacket) {
        if packet.media_type == MediaType::Video
            && packet.format == PayloadFormat::Raw
            && let Some(parameter_sets) = annexb_parameter_sets(&packet.payload)
        {
            self.set_track_config(MediaType::Video, packet.track_index, parameter_sets);
        }
    }

    pub fn set_audio_tracks(&self, tracks: Vec<AudioMeta>) {
        for track_index in 0..CONFIG_TRACKS_PER_TYPE as u32 {
            if !tracks.iter().any(|track| track.track_index == track_index) {
                self.track_configs.clear(MediaType::Audio, track_index);
            }
        }
        let count = tracks.len();
        if tracks.is_empty() {
            self.audio_tracks.store(None);
            debug!("ring audio tracks cleared");
        } else {
            self.audio_tracks.store(Some(Arc::new(tracks)));
            debug!(count, "ring audio tracks set");
        }
        self.wake_metadata_subscribers();
    }

    pub fn audio_tracks_snapshot(&self) -> Option<Arc<Vec<AudioMeta>>> {
        self.audio_tracks.load_full()
    }

    pub fn audio_tracks(&self) -> Option<Vec<AudioMeta>> {
        self.audio_tracks.load_full().map(|arc| (*arc).clone())
    }

    pub fn push(&self, packet: MediaPacket) {
        let idx = self.write_idx.val.load(Ordering::Relaxed);
        let slot_idx = idx % self.capacity;
        let is_keyframe = packet.media_type == MediaType::Video && packet.is_keyframe;
        let payload_bytes = packet.payload.len() as u64;

        let unit = self.next_frame_unit(&packet, idx);
        self.slots[slot_idx]
            .published_at_us
            .store(self.elapsed_us().max(1), Ordering::Release);
        self.slots[slot_idx].unit.store(unit, Ordering::Release);
        self.slots[slot_idx].data.store(Some(Arc::new(packet)));

        if is_keyframe {
            self.last_keyframe_idx.val.store(idx, Ordering::Release);
        }
        self.add_published_bytes(payload_bytes);

        self.write_idx.val.store(idx + 1, Ordering::Release);
        self.notify.notify_waiters();
        self.wake_publication_subscribers();
    }

    pub fn push_batch<I>(&self, packets: I) -> usize
    where
        I: IntoIterator<Item = MediaPacket>,
    {
        let start_idx = self.write_idx.val.load(Ordering::Relaxed);
        let mut count = 0usize;
        let mut payload_bytes = 0u64;

        for packet in packets {
            let idx = start_idx + count;
            let slot_idx = idx % self.capacity;
            let is_keyframe = packet.media_type == MediaType::Video && packet.is_keyframe;
            payload_bytes += packet.payload.len() as u64;

            let unit = self.next_frame_unit(&packet, idx);
            self.slots[slot_idx]
                .published_at_us
                .store(self.elapsed_us().max(1), Ordering::Release);
            self.slots[slot_idx].unit.store(unit, Ordering::Release);
            self.slots[slot_idx].data.store(Some(Arc::new(packet)));
            if is_keyframe {
                self.last_keyframe_idx.val.store(idx, Ordering::Release);
            }
            count += 1;
        }

        if count > 0 {
            self.add_published_bytes(payload_bytes);
            self.write_idx
                .val
                .store(start_idx + count, Ordering::Release);
            self.notify.notify_waiters();
            self.wake_publication_subscribers();
        }

        count
    }

    pub fn push_drained_batch_capped(&self, packets: &mut Vec<MediaPacket>) -> usize {
        let mut total = 0usize;
        let mut drained = packets.drain(..);
        loop {
            let published = self.push_batch(drained.by_ref().take(MEDIA_PRODUCER_BATCH_PACKETS));
            if published == 0 {
                break;
            }
            total += published;
        }
        total
    }

    pub fn push_batch_recording_config<I>(&self, packets: I) -> usize
    where
        I: IntoIterator<Item = MediaPacket>,
    {
        self.push_batch(
            packets
                .into_iter()
                .inspect(|packet| self.record_in_band_config(packet)),
        )
    }

    pub fn push_drained_batch_recording_config(&self, packets: &mut Vec<MediaPacket>) -> usize {
        let mut total = 0usize;
        let mut drained = packets.drain(..);
        loop {
            let published = self
                .push_batch_recording_config(drained.by_ref().take(MEDIA_PRODUCER_BATCH_PACKETS));
            if published == 0 {
                break;
            }
            total += published;
        }
        total
    }

    pub fn read_at(&self, idx: usize) -> Option<Arc<MediaPacket>> {
        let slot_idx = idx % self.capacity;
        self.slots[slot_idx].data.load_full()
    }

    pub fn published_at_us(&self, sequence: usize) -> u64 {
        self.slots[sequence % self.capacity]
            .published_at_us
            .load(Ordering::Acquire)
    }

    fn add_published_bytes(&self, bytes: u64) {
        let total = self.published_bytes.load(Ordering::Relaxed);
        self.published_bytes
            .store(total.wrapping_add(bytes), Ordering::Relaxed);
    }

    pub fn published_bytes(&self) -> u64 {
        self.published_bytes.load(Ordering::Relaxed)
    }

    fn elapsed_us(&self) -> u64 {
        self.created_at.elapsed().as_micros().min(u64::MAX as u128) as u64
    }

    pub fn get_write_idx(&self) -> usize {
        self.write_idx.val.load(Ordering::Acquire)
    }

    pub fn get_notify(&self) -> Arc<tokio::sync::Notify> {
        self.notify.clone()
    }

    pub fn min_read_idx(&self) -> usize {
        let write_idx = self.write_idx.val.load(Ordering::Relaxed);
        let readers = lock(&self.readers);
        let mut min_idx = write_idx;
        let mut has_readers = false;
        for w in readers.iter() {
            if let Some(info) = w.upgrade() {
                let r_idx = info.read_idx.load(Ordering::Relaxed);
                min_idx = min_idx.min(r_idx);
                has_readers = true;
            }
        }
        if has_readers { min_idx } else { write_idx }
    }

    pub fn fill_and_capacity(&self) -> (usize, usize) {
        let write_idx = self.write_idx.val.load(Ordering::Relaxed);
        let readers = lock(&self.readers);
        let mut min_idx = write_idx;
        let mut has_readers = false;
        for w in readers.iter() {
            if let Some(info) = w.upgrade() {
                let r_idx = info.read_idx.load(Ordering::Relaxed);
                min_idx = min_idx.min(r_idx);
                has_readers = true;
            }
        }
        let fill = if has_readers {
            write_idx.saturating_sub(min_idx).min(self.capacity)
        } else {
            write_idx.min(self.capacity)
        };
        (fill, self.capacity)
    }

    pub fn payload_stats(&self) -> PayloadStats {
        let mut stats = PayloadStats::default();
        let mut min_payload = usize::MAX;

        for slot in &self.slots {
            let Some(packet) = slot.data.load_full() else {
                continue;
            };
            let len = packet.payload.len();
            stats.slots += 1;
            stats.payload_bytes = stats.payload_bytes.saturating_add(len);
            min_payload = min_payload.min(len);
            stats.max_payload_bytes = stats.max_payload_bytes.max(len);
            match packet.media_type {
                MediaType::Video => {
                    stats.video_bytes = stats.video_bytes.saturating_add(len);
                }
                MediaType::Audio => {
                    stats.audio_bytes = stats.audio_bytes.saturating_add(len);
                }
            }
        }

        if stats.slots > 0 {
            stats.min_payload_bytes = min_payload;
        }

        stats
    }

    fn payload_by_dts(&self) -> Vec<(i64, u64)> {
        let mut samples = Vec::with_capacity(self.capacity);
        for slot in &self.slots {
            let Some(packet) = slot.data.load_full() else {
                continue;
            };
            samples.push((packet.dts, packet.payload.len() as u64));
        }
        samples.sort_unstable_by_key(|(dts, _)| *dts);

        let mut grouped: Vec<(i64, u64)> = Vec::with_capacity(samples.len());
        for (dts, bytes) in samples {
            if let Some((last_dts, last_bytes)) = grouped.last_mut()
                && *last_dts == dts
            {
                *last_bytes = last_bytes.saturating_add(bytes);
                continue;
            }
            grouped.push((dts, bytes));
        }
        grouped
    }

    pub fn retained_payload_bitrate_bps(&self) -> Option<u64> {
        average_payload_bps(&self.payload_by_dts())
    }

    pub fn observed_payload_bitrate_bps(&self) -> Option<u64> {
        const PEAK_WINDOW_MS: i64 = 250;

        let grouped = self.payload_by_dts();
        let average_bps = average_payload_bps(&grouped)?;
        let mut peak_bps = 0u64;
        let mut window_bytes = 0u64;
        let mut end = 0usize;
        for start in 0..grouped.len() {
            let window_end_dts = grouped[start].0.saturating_add(PEAK_WINDOW_MS);
            while end < grouped.len() && grouped[end].0 < window_end_dts {
                window_bytes = window_bytes.saturating_add(grouped[end].1);
                end += 1;
            }

            if window_bytes > 0 {
                let effective_span_ms = grouped[end.saturating_sub(1)]
                    .0
                    .saturating_sub(grouped[start].0)
                    .max(PEAK_WINDOW_MS);
                let candidate_bps = window_bytes
                    .saturating_mul(8)
                    .saturating_mul(1_000)
                    .div_ceil(effective_span_ms as u64);
                peak_bps = peak_bps.max(candidate_bps);
            }

            window_bytes = window_bytes.saturating_sub(grouped[start].1);
        }

        Some(average_bps.max(peak_bps))
    }

    pub fn reader_snapshots(&self) -> Vec<ReaderSnapshot> {
        let write_idx = self.get_write_idx();
        let now_us = self.elapsed_us();
        let mut snapshots = Vec::new();

        let mut readers = lock(&self.readers);
        readers.retain(|weak_ref| {
            let Some(info) = weak_ref.upgrade() else {
                return false;
            };

            let read_idx = info.read_idx.load(Ordering::Acquire);
            let lag_slots = write_idx.saturating_sub(read_idx);
            let packet_age_ms = if lag_slots == 0 || lag_slots >= self.capacity {
                None
            } else {
                let slot = &self.slots[read_idx % self.capacity];
                if slot.data.load_full().is_some() {
                    let published_at_us = slot.published_at_us.load(Ordering::Acquire);
                    (published_at_us > 0).then(|| now_us.saturating_sub(published_at_us) / 1000)
                } else {
                    None
                }
            };
            let (avg_burst_size, median_burst_size, burst_count) = info.burst_stats();

            snapshots.push(ReaderSnapshot {
                name: info.name.clone(),
                read_idx,
                write_idx,
                lag_slots,
                overflow_count: info.overflow_count.load(Ordering::Relaxed),
                packet_age_ms,
                burst_count,
                avg_burst_size,
                median_burst_size,
            });

            true
        });

        snapshots
    }

    pub fn fast_forward(&self, current_write_idx: usize) -> usize {
        let kf_idx = self.last_keyframe_idx.val.load(Ordering::Acquire);
        let kf_known = kf_idx != usize::MAX;
        if kf_known && current_write_idx.saturating_sub(kf_idx) < self.capacity {
            return kf_idx;
        }
        current_write_idx
    }
}

#[derive(Debug, Clone)]
pub struct DtsEnforcer {
    last_dts: Vec<i64>,
}

impl DtsEnforcer {
    pub fn new(num_streams: usize) -> Self {
        Self {
            last_dts: vec![i64::MIN; num_streams],
        }
    }

    pub fn enforce(&mut self, stream_idx: usize, pts: i64, dts: i64) -> (i64, i64) {
        let mut dts = dts;
        if let Some(prev) = self.last_dts.get(stream_idx)
            && dts <= *prev
        {
            dts = prev.saturating_add(1);
        }
        let pts = if pts < dts { dts } else { pts };
        if let Some(slot) = self.last_dts.get_mut(stream_idx) {
            *slot = dts;
        }
        (pts, dts)
    }
}

fn average_payload_bps(grouped: &[(i64, u64)]) -> Option<u64> {
    const MIN_OBSERVATION_MS: i64 = 250;

    if grouped.len() < 2 {
        return None;
    }
    let span_ms = grouped.last()?.0.checked_sub(grouped.first()?.0)?;
    let payload_bytes = grouped
        .iter()
        .fold(0u64, |acc, (_, bytes)| acc.saturating_add(*bytes));
    if payload_bytes == 0 || span_ms < MIN_OBSERVATION_MS {
        return None;
    }
    Some(
        payload_bytes
            .saturating_mul(8)
            .saturating_mul(1_000)
            .div_ceil(span_ms as u64),
    )
}
