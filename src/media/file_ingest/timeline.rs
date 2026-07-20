use crate::media::packet::MediaPacket;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(super) struct LoopTimestampState {
    offset_ms: i64,
    pass_base_timestamp_ms: Option<i64>,
    pass_max_timestamp_ms: Option<i64>,
    pass_packet_count: usize,
}

impl LoopTimestampState {
    fn packet_timestamp_ms(packet: &MediaPacket) -> i64 {
        if packet.dts >= 0 {
            packet.dts
        } else {
            packet.pts
        }
    }

    pub(super) fn begin_pass(&mut self) {
        self.pass_base_timestamp_ms = None;
        self.pass_max_timestamp_ms = None;
        self.pass_packet_count = 0;
    }

    pub(super) fn apply(&mut self, packet: &mut MediaPacket) {
        let pass_base_timestamp_ms = *self
            .pass_base_timestamp_ms
            .get_or_insert_with(|| Self::packet_timestamp_ms(packet));
        packet.pts = packet
            .pts
            .saturating_sub(pass_base_timestamp_ms)
            .saturating_add(self.offset_ms);
        packet.dts = packet
            .dts
            .saturating_sub(pass_base_timestamp_ms)
            .saturating_add(self.offset_ms);
        self.pass_packet_count += 1;
        let packet_max = packet.pts.max(packet.dts);
        self.pass_max_timestamp_ms = Some(
            self.pass_max_timestamp_ms
                .map_or(packet_max, |current| current.max(packet_max)),
        );
    }

    pub(super) fn finish_pass(&mut self) {
        if let Some(max_timestamp_ms) = self.pass_max_timestamp_ms {
            self.offset_ms = max_timestamp_ms.saturating_add(1);
        }
    }

    pub(super) fn pass_packet_count(&self) -> usize {
        self.pass_packet_count
    }
}

#[derive(Default)]
pub(crate) struct ContinuousTimestampState {
    offset_ms: i64,
    last_timestamp_ms_by_stream: HashMap<u64, i64>,
}

impl ContinuousTimestampState {
    fn stream_key(packet: &MediaPacket) -> u64 {
        ((packet.media_type as u64) << 32) | u64::from(packet.track_index)
    }

    fn continuity_timestamp_ms(packet: &MediaPacket) -> i64 {
        if packet.dts >= 0 {
            packet.dts
        } else {
            packet.pts
        }
    }

    pub(crate) fn apply(&mut self, packet: &mut MediaPacket) {
        let stream_key = Self::stream_key(packet);
        let raw_timestamp_ms = Self::continuity_timestamp_ms(packet);
        if let Some(last_timestamp_ms) = self.last_timestamp_ms_by_stream.get(&stream_key).copied()
        {
            let adjusted_timestamp_ms = raw_timestamp_ms.saturating_add(self.offset_ms);
            if adjusted_timestamp_ms <= last_timestamp_ms {
                self.offset_ms = last_timestamp_ms
                    .saturating_add(1)
                    .saturating_sub(raw_timestamp_ms);
            }
        }

        packet.pts = packet.pts.saturating_add(self.offset_ms);
        packet.dts = packet.dts.saturating_add(self.offset_ms);
        let adjusted_timestamp_ms = Self::continuity_timestamp_ms(packet);
        self.last_timestamp_ms_by_stream.insert(
            stream_key,
            self.last_timestamp_ms_by_stream
                .get(&stream_key)
                .copied()
                .map_or(adjusted_timestamp_ms, |current| {
                    current.max(adjusted_timestamp_ms)
                }),
        );
    }
}

pub(super) fn pace_packet(
    cancel: &CancellationToken,
    anchor: &mut Option<(i64, Instant)>,
    packet_ts_ms: i64,
) {
    if packet_ts_ms < 0 {
        return;
    }

    if anchor.is_none() {
        *anchor = Some((packet_ts_ms, Instant::now()));
        return;
    }

    let (base_ts_ms, start_instant) = anchor.expect("anchor initialized above");
    // Interleaved streams can deliver a packet timestamped slightly before the
    // anchor (e.g. audio that starts earlier than the first video packet in
    // mux order). A negative delta must clamp to zero — casting it straight
    // to u64 would wrap into a near-infinite sleep and hang the ingest.
    let desired_ms = packet_ts_ms.saturating_sub(base_ts_ms).max(0) as u64;
    let desired = Duration::from_millis(desired_ms);
    let elapsed = start_instant.elapsed();
    if elapsed >= desired {
        return;
    }

    let mut remaining = desired - elapsed;
    while remaining > Duration::ZERO && !cancel.is_cancelled() {
        let slice = remaining.min(Duration::from_millis(25));
        std::thread::sleep(slice);
        remaining = desired.saturating_sub(start_instant.elapsed());
    }
}
