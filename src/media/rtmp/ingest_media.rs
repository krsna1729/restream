//! RTMP publish media, run to completion on the RTMP ingress owner thread:
//! FLV classification, sequence-header caching, input gating, timestamp
//! mapping, standby GOP caching and promotion, and ring publication, for each
//! decoded audio/video message as the owner parses it (WI11 step 2). Nothing
//! per message crosses to Tokio.
//!
//! Tokio authorizes and registers the publisher, builds this state and hands
//! it to the owner in the publish reply. The only things the owner reports
//! back are the one-time stream probes ([`RtmpMediaEvent`]), which update
//! ingest metadata. Sequence headers go straight into the ingest session's
//! shared cells, so play, egress and promotion read the same values as before.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::input_gate::{InputForwardState, InputPacketBoundary, InputTimestampMapper};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_metrics::StageMetrics;
use crate::media::standby_gop::StandbyGopCache;

use super::flv::{
    FlvVideoPacketKind, classify_flv_video_packet, flv_avcc_config_annexb_parameter_sets,
    flv_video_composition_time_ms, parse_flv_audio_meta, parse_flv_video_meta,
};

/// A one-time stream probe the owner reports to Tokio (ingest metadata).
pub(super) enum RtmpMediaEvent {
    Video(VideoMeta),
    Audio(AudioMeta),
}

/// One RTMP publisher's media state, owned by the RTMP ingress owner.
pub(super) struct RtmpPublisherMedia {
    pub(super) registration: IngestRegistration,
    pub(super) ring: Arc<RingBuffer>,
    pub(super) bytes_received: Arc<AtomicU64>,
    pub(super) ingest_metrics: Arc<StageMetrics>,
    pub(super) last_progress_ms: Arc<AtomicU64>,
    pub(super) keyframe_times: Arc<Mutex<Vec<i64>>>,
    pub(super) video_sequence_header: Arc<Mutex<Option<Bytes>>>,
    pub(super) audio_sequence_header: Arc<Mutex<Option<Bytes>>>,
    pub(super) timestamp_mapper: InputTimestampMapper,
    pub(super) standby_gop: StandbyGopCache,
    pub(super) video_probed: bool,
    pub(super) audio_probed: bool,
}

impl RtmpPublisherMedia {
    fn account(&self, len: usize) {
        self.bytes_received.fetch_add(len as u64, Ordering::Relaxed);
        self.ingest_metrics.record_in(len as u64);
        self.last_progress_ms
            .store(MediaEngine::now_epoch_ms(), Ordering::Relaxed);
    }

    /// One decoded video message. Returns the stream's first complete video
    /// probe, for Tokio to record as ingest metadata.
    pub(super) fn on_video(&mut self, data: Bytes, timestamp: u32) -> Option<RtmpMediaEvent> {
        self.account(data.len());
        let packet_kind = classify_flv_video_packet(&data);
        let is_keyframe = matches!(packet_kind, Some(FlvVideoPacketKind::Keyframe));
        let dts = timestamp as i64;
        let pts = dts + flv_video_composition_time_ms(&data) as i64;
        let parameter_sets = flv_avcc_config_annexb_parameter_sets(&data);
        if matches!(packet_kind, Some(FlvVideoPacketKind::SequenceHeader)) && (data[0] & 0x0F) == 7
        {
            *lock(&self.video_sequence_header) = Some(data.clone());
        }
        let mut event = None;
        if !self.video_probed
            && let Some(meta) = parse_flv_video_meta(&data)
        {
            if meta.width > 0 {
                self.video_probed = true;
            }
            event = Some(RtmpMediaEvent::Video(meta));
        }

        let mut packet = MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts,
            dts,
            is_keyframe,
            format: PayloadFormat::Flv,
            payload: data,
        };
        let boundary = if is_keyframe {
            InputPacketBoundary::VideoKeyframe
        } else {
            InputPacketBoundary::Other
        };
        if let Some(preview_ring) = self.registration.preview_ring.load_full() {
            if let Some(parameter_sets) = parameter_sets.clone() {
                preview_ring.set_video_parameter_sets(parameter_sets);
            }
            preview_ring.push(packet.clone());
        }
        if self.registration.gate.state() == InputForwardState::Active {
            let Some(lease) = self.registration.gate.try_enter(boundary) else {
                return event;
            };
            self.timestamp_mapper.map_packet(
                &mut packet,
                false,
                &self.registration.last_forwarded_dts,
            );
            if let Some(parameter_sets) = parameter_sets {
                self.ring.set_video_parameter_sets(parameter_sets);
            }
            let keyframe_pts = is_keyframe.then_some(packet.pts);
            InputTimestampMapper::record_forwarded(&packet, &self.registration.last_forwarded_dts);
            self.ring.push(packet);
            drop(lease);
            if let Some(pts) = keyframe_pts {
                self.record_keyframe(pts);
            }
        } else {
            self.standby_gop.push(packet);
            self.try_promote();
        }
        event
    }

    /// One decoded audio message. Returns the stream's first complete audio
    /// probe, for Tokio to record as ingest metadata and audio tracks.
    pub(super) fn on_audio(&mut self, data: Bytes, timestamp: u32) -> Option<RtmpMediaEvent> {
        self.account(data.len());
        if data.len() >= 2 && (data[0] >> 4) == 10 && data[1] == 0 {
            *lock(&self.audio_sequence_header) = Some(data.clone());
        }
        let mut event = None;
        if !self.audio_probed {
            let format_id = data.first().map(|byte| (byte >> 4) & 0x0f).unwrap_or(0xff);
            let has_complete_config = format_id != 10 || (data.len() >= 3 && data[1] == 0);
            if has_complete_config && let Some(meta) = parse_flv_audio_meta(&data) {
                self.audio_probed = true;
                event = Some(RtmpMediaEvent::Audio(meta));
            }
        }

        let mut packet = MediaPacket {
            media_type: MediaType::Audio,
            track_index: 0,
            pts: timestamp as i64,
            dts: timestamp as i64,
            is_keyframe: false,
            format: PayloadFormat::Flv,
            payload: data,
        };
        if let Some(preview_ring) = self.registration.preview_ring.load_full() {
            preview_ring.push(packet.clone());
        }
        if self.registration.gate.state() == InputForwardState::Active {
            let Some(lease) = self.registration.gate.try_enter(InputPacketBoundary::Other) else {
                return event;
            };
            self.timestamp_mapper.map_packet(
                &mut packet,
                false,
                &self.registration.last_forwarded_dts,
            );
            InputTimestampMapper::record_forwarded(&packet, &self.registration.last_forwarded_dts);
            self.ring.push(packet);
            drop(lease);
        } else {
            self.standby_gop.push(packet);
            self.try_promote();
        }
        event
    }

    fn record_keyframe(&self, pts: i64) {
        let mut times = lock(&self.keyframe_times);
        times.push(pts);
        if times.len() > 30 {
            times.remove(0);
        }
    }

    /// Promote a standby publisher once its cached GOP is replayable and the
    /// gate is waiting for a keyframe: replay the GOP (sequence headers first
    /// when this is the activation) into the ring.
    fn try_promote(&mut self) {
        if !self.standby_gop.is_replay_ready()
            || self.registration.gate.state() != InputForwardState::AwaitingKeyframe
        {
            return;
        }
        let promotion_headers = (
            lock(&self.video_sequence_header).clone(),
            lock(&self.audio_sequence_header).clone(),
        );
        let Some(lease) = self
            .registration
            .gate
            .try_enter(InputPacketBoundary::ReplayReady)
        else {
            return;
        };

        let mut replay = self.standby_gop.take_replay();
        for (index, packet) in replay.iter_mut().enumerate() {
            self.timestamp_mapper.map_packet(
                packet,
                lease.activated() && index == 0,
                &self.registration.last_forwarded_dts,
            );
        }
        let first_dts = replay.first().map(|packet| packet.dts);
        if lease.activated()
            && let Some(first_dts) = first_dts
        {
            push_promotion_headers(&self.ring, promotion_headers, first_dts.saturating_sub(1));
        }
        if let Some(last) = replay.iter().max_by_key(|packet| packet.dts) {
            InputTimestampMapper::record_forwarded(last, &self.registration.last_forwarded_dts);
        }
        let keyframe_pts: Vec<i64> = replay
            .iter()
            .filter(|packet| packet.media_type == MediaType::Video && packet.is_keyframe)
            .map(|packet| packet.pts)
            .collect();
        self.ring.push_drained_batch_capped(&mut replay);
        drop(lease);
        for pts in keyframe_pts {
            self.record_keyframe(pts);
        }
    }
}

fn lock<T>(cell: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    crate::sync::lock(cell)
}

pub(super) fn push_promotion_headers(
    ring: &RingBuffer,
    (video, audio): (Option<Bytes>, Option<Bytes>),
    timestamp: i64,
) {
    if let Some(payload) = video {
        ring.push(MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: timestamp,
            dts: timestamp,
            is_keyframe: false,
            format: PayloadFormat::Flv,
            payload,
        });
    }
    if let Some(payload) = audio {
        ring.push(MediaPacket {
            media_type: MediaType::Audio,
            track_index: 0,
            pts: timestamp,
            dts: timestamp,
            is_keyframe: false,
            format: PayloadFormat::Flv,
            payload,
        });
    }
}
