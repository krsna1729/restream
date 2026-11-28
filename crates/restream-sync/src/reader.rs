use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::{debug, info, warn};

use crate::reader_info::{ReaderInfo, burst_bucket};
use crate::ring_buffer::{ReaderWait, RingBuffer, VideoConfigFollower};
use restream_core::{AudioMeta, MediaPacket};

const OVERFLOW: &str = "Overflow: reader lagged and was fast-forwarded";

pub struct Reader {
    pub(crate) buffer: Arc<RingBuffer>,
    pub info: Arc<ReaderInfo>,
    pub(crate) read_idx: usize,
    pub(crate) migration_preroll_packets: usize,
    /// Set by every overflow resync: until the next video keyframe, video
    /// interframes are skipped (they reference frames this reader never
    /// delivered); audio passes. A resync onto a keyframe clears it at once.
    awaiting_keyframe: bool,
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.buffer.deregister_reader(&self.info);
        info!(
            target: "restream_engine::ring_buffer::reader",
            reader = %self.info.name,
            overflows = self.info.overflow_count.load(Ordering::Relaxed),
            "ring reader deregistered"
        );
    }
}

impl Reader {
    fn register(
        name: String,
        buffer: Arc<RingBuffer>,
        start_idx: usize,
        migration_preroll_packets: usize,
    ) -> Self {
        let info = Arc::new(ReaderInfo::new(name.clone(), start_idx));
        buffer.register_reader(&info);

        Self {
            buffer,
            info,
            read_idx: start_idx,
            migration_preroll_packets,
            awaiting_keyframe: false,
        }
    }

    /// The codec hint of the ring this reader reads now (it follows the
    /// reader across a resize).
    pub fn codec_hint_str(&self) -> &str {
        self.buffer.codec_hint_str()
    }

    /// The raw video parameter sets of the ring this reader reads now.
    pub fn video_parameter_sets(&self) -> Option<Vec<u8>> {
        self.buffer.video_parameter_sets()
    }

    /// The audio tracks of the ring this reader reads now.
    pub fn audio_tracks(&self) -> Option<Vec<AudioMeta>> {
        self.buffer.audio_tracks()
    }

    /// [`VideoConfigFollower::follow`] for the packet this reader pulled at
    /// `sequence`, from the ring it pulled it from.
    pub fn follow_video_config(
        &self,
        follower: &mut VideoConfigFollower,
        sequence: usize,
        target: &RingBuffer,
        pending: &mut Vec<MediaPacket>,
    ) {
        follower.follow(&self.buffer, sequence, target, pending);
    }

    /// Whether this reader reads `ring` now (it moves to a successor once it
    /// drains a sealed ring).
    pub fn is_reading(&self, ring: &RingBuffer) -> bool {
        std::ptr::eq(Arc::as_ptr(&self.buffer), ring)
    }

    /// Ring sequence of the next packet this reader pulls; a successful
    /// `pull_burst` returns a contiguous run of packets ending just before
    /// the new `next_sequence()`. After an overflow the run can start past
    /// the old value: video interframes ahead of the next keyframe are
    /// skipped.
    pub fn next_sequence(&self) -> usize {
        self.read_idx
    }

    pub fn is_caught_up_to_end_of_stream(&self) -> bool {
        self.buffer.is_end_of_stream() && self.buffer.get_write_idx() == self.read_idx
    }

    pub fn new(name: String, buffer: Arc<RingBuffer>) -> Self {
        let current_write = buffer.get_write_idx();
        let start_idx = buffer.fast_forward(current_write);
        let reader = Self::register(name, buffer, start_idx, 0);
        info!(target: "restream_engine::ring_buffer::reader", reader = %reader.info.name, start_idx, "ring reader registered");
        reader
    }

    pub fn new_with_keyframe_preroll(
        name: String,
        buffer: Arc<RingBuffer>,
        preroll_packets: usize,
    ) -> Self {
        let current_write = buffer.get_write_idx();
        let keyframe_start = buffer.fast_forward(current_write);
        let oldest_available = current_write.saturating_sub(buffer.capacity.saturating_sub(1));
        let start_idx = if keyframe_start < current_write {
            keyframe_start
                .saturating_sub(preroll_packets)
                .max(oldest_available)
        } else {
            keyframe_start
        };
        let reader = Self::register(name, buffer, start_idx, 0);
        info!(
            target: "restream_engine::ring_buffer::reader",
            reader = %reader.info.name,
            start_idx,
            preroll_packets,
            "ring reader registered (keyframe preroll)"
        );
        reader
    }

    pub fn new_stage_input(name: String, buffer: Arc<RingBuffer>, preroll_packets: usize) -> Self {
        let current_write = buffer.get_write_idx();
        let keyframe_start = buffer.fast_forward(current_write);
        let oldest_available = current_write.saturating_sub(buffer.capacity.saturating_sub(1));
        let start_idx = if keyframe_start < current_write {
            keyframe_start
                .saturating_sub(preroll_packets)
                .max(oldest_available)
        } else {
            keyframe_start
        };
        let reader = Self::register(name, buffer, start_idx, preroll_packets);
        info!(
            target: "restream_engine::ring_buffer::reader",
            reader = %reader.info.name,
            start_idx,
            preroll_packets,
            "ring reader registered (stage input)"
        );
        reader
    }

    pub fn new_live(name: String, buffer: Arc<RingBuffer>) -> Self {
        let current_write = buffer.get_write_idx();
        let reader = Self::register(name, buffer, current_write, 0);
        info!(
            target: "restream_engine::ring_buffer::reader",
            reader = %reader.info.name,
            start_idx = current_write,
            "ring reader registered (live edge)"
        );
        reader
    }

    pub fn sync_read_idx(&mut self, read_idx: usize) {
        self.read_idx = read_idx;
        self.info.read_idx.store(read_idx, Ordering::Relaxed);
    }

    /// Resynchronize after falling `write_idx - read_idx` behind, or after
    /// finding the slot at `read_idx` already holds a later lap's packet.
    fn resync(&mut self, write_idx: usize) -> usize {
        let lag = write_idx.saturating_sub(self.read_idx);
        self.read_idx = self.buffer.fast_forward(write_idx);
        self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
        self.info.overflow_count.fetch_add(1, Ordering::Relaxed);
        self.awaiting_keyframe = true;
        lag
    }

    fn lags_a_lap(&self, write_idx: usize) -> bool {
        write_idx > self.read_idx && write_idx - self.read_idx >= self.buffer.capacity
    }

    #[inline]
    pub fn pull(&mut self) -> Result<Option<Arc<MediaPacket>>, &'static str> {
        loop {
            let write_idx = self.buffer.get_write_idx();
            if self.read_idx == write_idx {
                return Ok(None);
            }
            let packet = if self.lags_a_lap(write_idx) {
                None
            } else {
                self.buffer.read_at(self.read_idx)
            };
            let Some(packet) = packet else {
                let lag = self.resync(write_idx);
                warn!(
                    target: "restream_engine::ring_buffer::reader",
                    reader = %self.info.name,
                    lag_packets = lag,
                    "ring reader overflowed — fast-forwarding to keyframe"
                );
                return Err(OVERFLOW);
            };
            self.read_idx += 1;
            self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
            if self.awaiting_keyframe {
                if packet.is_video_interframe() {
                    continue;
                }
                self.awaiting_keyframe = !packet.is_video_keyframe();
            }
            return Ok(Some(packet));
        }
    }

    pub fn pull_burst(
        &mut self,
        output: &mut Vec<Arc<MediaPacket>>,
        max_packets: usize,
    ) -> Result<usize, &'static str> {
        if max_packets == 0 {
            return Ok(0);
        }

        let write_idx = self.buffer.get_write_idx();
        if self.lags_a_lap(write_idx) {
            self.resync(write_idx);
            return Err(OVERFLOW);
        }

        let start_len = output.len();
        let next = if self.awaiting_keyframe {
            self.load_after_resync(output, write_idx, max_packets)
        } else {
            let available = write_idx.saturating_sub(self.read_idx).min(max_packets);
            output.reserve(available);
            self.load(output, self.read_idx, self.read_idx + available)
        };
        let Some(next) = next else {
            // A publication lapped a slot before moving the write index.
            output.truncate(start_len);
            self.resync(write_idx);
            return Err(OVERFLOW);
        };

        let loaded = output.len() - start_len;
        self.read_idx = next;
        self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
        if loaded > 0 {
            self.info.burst_count.fetch_add(1, Ordering::Relaxed);
            self.info
                .packet_sum
                .fetch_add(loaded as u64, Ordering::Relaxed);
            self.info.burst_hist[burst_bucket(loaded)].fetch_add(1, Ordering::Relaxed);
        }
        Ok(loaded)
    }

    /// Push the packets of `start..end` onto `output`; the sequence after
    /// the last, or `None` when a slot no longer holds its packet.
    #[inline]
    fn load(&self, output: &mut Vec<Arc<MediaPacket>>, start: usize, end: usize) -> Option<usize> {
        for sequence in start..end {
            output.push(self.buffer.read_at(sequence)?);
        }
        Some(end)
    }

    /// `load` while awaiting a keyframe: skip the video interframes at the
    /// read position (up to the write index), then load a contiguous run of
    /// at most `max_packets` that stops before the next interframe to skip.
    /// The first video keyframe ends the wait.
    fn load_after_resync(
        &mut self,
        output: &mut Vec<Arc<MediaPacket>>,
        write_idx: usize,
        max_packets: usize,
    ) -> Option<usize> {
        let mut sequence = self.read_idx;
        let mut loaded = 0;
        while sequence < write_idx && loaded < max_packets {
            let packet = self.buffer.read_at(sequence)?;
            if self.awaiting_keyframe {
                if packet.is_video_interframe() {
                    if loaded > 0 {
                        break;
                    }
                    sequence += 1;
                    continue;
                }
                self.awaiting_keyframe = !packet.is_video_keyframe();
            }
            output.push(packet);
            loaded += 1;
            sequence += 1;
        }
        Some(sequence)
    }

    pub async fn wait_for_data(&mut self) {
        loop {
            let notify = self.buffer.get_notify();
            if self.buffer.get_write_idx() > self.read_idx {
                return;
            }
            if let Some(old_ring) = RingBuffer::follow_successor(&mut self.buffer) {
                self.migrated_from(&old_ring);
                continue;
            }
            let notified = notify.notified();
            match self.buffer.reader_wait(self.read_idx) {
                ReaderWait::Ready => return,
                ReaderWait::Superseded => continue,
                ReaderWait::Park => notified.await,
            }
        }
    }

    /// The reader has just followed `old_ring`'s successor: join the new
    /// ring and, for a stage input caught up at the handoff, start it at the
    /// new ring's keyframe preroll.
    fn migrated_from(&mut self, old_ring: &RingBuffer) {
        let old_read_idx = self.read_idx;
        let new_ring = &self.buffer;
        new_ring.register_reader(&self.info);
        let new_write_idx = new_ring.get_write_idx();
        if self.migration_preroll_packets > 0 && old_read_idx == new_write_idx {
            let keyframe_start = new_ring.fast_forward(new_write_idx);
            if keyframe_start < new_write_idx {
                let oldest_available =
                    new_write_idx.saturating_sub(new_ring.capacity.saturating_sub(1));
                self.read_idx = keyframe_start
                    .saturating_sub(self.migration_preroll_packets)
                    .max(oldest_available);
                self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
            }
        }
        old_ring.prune_readers();
        debug!(
            target: "restream_engine::ring_buffer::reader",
            read_idx = self.read_idx,
            name = %self.info.name,
            "reader migrated to resized ring"
        );
    }

    pub fn lag(&self) -> usize {
        self.buffer.get_write_idx().saturating_sub(self.read_idx)
    }
}
