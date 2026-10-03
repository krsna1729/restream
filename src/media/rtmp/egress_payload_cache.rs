//! Shard-wide memo of Raw → FLV payload conversions for RTMP egress.
//!
//! A Raw feed (SRT/TS ingest, transcoder output) carries Annex B video and
//! ADTS audio; every RTMP output must send AVCC/HVCC video and raw AAC. The
//! converted bytes depend only on the packet and the output's enhanced-HEVC
//! mode, never on per-output state, yet each output used to convert (and then
//! copy) every packet itself: at 100 outputs of one 8 Mbit/s SRT ingest that
//! was ~11% of Restream CPU, 100x the necessary work.
//!
//! All outputs on one egress shard read the same feed within a few units of
//! each other, so the shard thread keeps the most recent conversions in a
//! short FIFO searched newest-first: the first output converts, the rest find
//! the entry within a few comparisons and share the resulting `Bytes`. Each
//! entry keeps a clone of its source payload, so its key (payload address and
//! length plus timing flags) cannot be freed and reused while it is held. A
//! lagging output that misses simply converts again; the cache never changes
//! what an output sends.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use bytes::Bytes;

use super::egress_packets::cache_h264_parameter_sets;
use super::enhanced::cache_hevc_parameter_sets;
use crate::media::codec;
use crate::media::packet::{MediaPacket, MediaType};

/// Entries per shard. Outputs on a shard trail the feed head by a few units, so
/// 64 covers them while bounding retained converted video to 64 frames.
const ENTRIES: usize = 64;

pub(crate) type SharedRtmpPayloadCache = Rc<RefCell<RtmpPayloadCache>>;

/// One packet's conversion, shared by every output that reads it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ConvertedPayload {
    /// Parameter sets this video packet carries, which an output adopts for
    /// later keyframe sequence headers; `None` when it carries none.
    pub(crate) parameter_sets: Option<Bytes>,
    /// The FLV-ready payload; `None` when conversion found nothing to send.
    pub(crate) payload: Option<Bytes>,
}

struct Slot {
    source: Bytes,
    media_type: MediaType,
    enhanced_hevc: bool,
    is_keyframe: bool,
    pts: i64,
    dts: i64,
    converted: ConvertedPayload,
}

impl Slot {
    fn matches(&self, packet: &MediaPacket, enhanced_hevc: bool) -> bool {
        self.source.as_ptr() == packet.payload.as_ptr()
            && self.source.len() == packet.payload.len()
            && self.media_type == packet.media_type
            && self.enhanced_hevc == enhanced_hevc
            && self.is_keyframe == packet.is_keyframe
            && self.pts == packet.pts
            && self.dts == packet.dts
    }
}

pub(crate) struct RtmpPayloadCache {
    recent: VecDeque<Slot>,
    hits: u64,
    misses: u64,
}

impl Default for RtmpPayloadCache {
    fn default() -> Self {
        Self {
            recent: VecDeque::with_capacity(ENTRIES),
            hits: 0,
            misses: 0,
        }
    }
}

impl RtmpPayloadCache {
    pub(crate) fn shared() -> SharedRtmpPayloadCache {
        Rc::new(RefCell::new(Self::default()))
    }

    /// The FLV conversion of a Raw packet, computed once per shard.
    pub(crate) fn convert(
        &mut self,
        packet: &MediaPacket,
        enhanced_hevc: bool,
    ) -> ConvertedPayload {
        if let Some(slot) = self
            .recent
            .iter()
            .rev()
            .find(|slot| slot.matches(packet, enhanced_hevc))
        {
            self.hits += 1;
            return slot.converted.clone();
        }
        self.misses += 1;
        let converted = convert_raw(packet, enhanced_hevc);
        if self.recent.len() == ENTRIES {
            self.recent.pop_front();
        }
        self.recent.push_back(Slot {
            source: packet.payload.clone(),
            media_type: packet.media_type,
            enhanced_hevc,
            is_keyframe: packet.is_keyframe,
            pts: packet.pts,
            dts: packet.dts,
            converted: converted.clone(),
        });
        converted
    }

    #[cfg(test)]
    pub(crate) fn hits_and_misses(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
}

/// Exactly what an output used to compute inline: the production parameter-set
/// scan and the production Annex B / ADTS conversion, written straight into the
/// buffer that becomes the shared `Bytes` (no intermediate copy).
fn convert_raw(packet: &MediaPacket, enhanced_hevc: bool) -> ConvertedPayload {
    match packet.media_type {
        MediaType::Video => {
            let mut parameter_sets = Vec::new();
            if enhanced_hevc {
                cache_hevc_parameter_sets(&packet.payload, &mut parameter_sets);
            } else {
                cache_h264_parameter_sets(&packet.payload, &mut parameter_sets);
            }
            let composition = (packet.pts - packet.dts).clamp(-8_388_608, 8_388_607) as i32;
            let mut out = Vec::with_capacity(packet.payload.len() + 16);
            let encoded = if enhanced_hevc {
                codec::hevc_video_for_enhanced_rtmp_with_composition_into(
                    &packet.payload,
                    packet.is_keyframe,
                    composition,
                    &mut out,
                )
            } else {
                codec::video_for_rtmp_with_composition_into(
                    &packet.payload,
                    packet.is_keyframe,
                    composition,
                    &mut out,
                )
            };
            ConvertedPayload {
                parameter_sets: (!parameter_sets.is_empty()).then(|| Bytes::from(parameter_sets)),
                payload: encoded.then(|| Bytes::from(out)),
            }
        }
        MediaType::Audio => {
            let mut out = Vec::new();
            codec::audio_for_rtmp_into(&packet.payload, &mut out);
            ConvertedPayload {
                parameter_sets: None,
                payload: Some(Bytes::from(out)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::packet::PayloadFormat;
    use crate::media::rtmp::egress_engine::{RtmpMediaAction, RtmpMediaEncoder};

    const H264_KEYFRAME: &[u8] = &[
        0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce, 0x06,
        0xe2, 0, 0, 0, 1, 0x65, 0x88,
    ];

    fn raw(media_type: MediaType, payload: &[u8], keyframe: bool, pts: i64) -> MediaPacket {
        MediaPacket {
            media_type,
            format: PayloadFormat::Raw,
            is_keyframe: keyframe,
            track_index: 0,
            pts,
            dts: pts - 40,
            payload: Bytes::copy_from_slice(payload),
        }
    }

    fn adts_frame() -> Vec<u8> {
        // 7-byte ADTS header (no CRC) announcing a 10-byte frame, then 3 bytes.
        vec![0xFF, 0xF1, 0x50, 0x80, 0x01, 0x5F, 0xFC, 0x21, 0x10, 0x04]
    }

    #[test]
    fn cached_conversion_is_the_production_conversion() {
        let video = raw(MediaType::Video, H264_KEYFRAME, true, 1_000);
        let mut expected_video = Vec::new();
        assert!(codec::video_for_rtmp_with_composition_into(
            &video.payload,
            true,
            40,
            &mut expected_video
        ));
        let mut expected_sets = Vec::new();
        cache_h264_parameter_sets(&video.payload, &mut expected_sets);
        let converted = RtmpPayloadCache::default().convert(&video, false);
        assert_eq!(converted.payload.as_deref(), Some(&expected_video[..]));
        assert_eq!(
            converted.parameter_sets.as_deref(),
            Some(&expected_sets[..])
        );

        let audio = raw(MediaType::Audio, &adts_frame(), false, 1_000);
        let mut expected_audio = Vec::new();
        codec::audio_for_rtmp_into(&audio.payload, &mut expected_audio);
        let converted = RtmpPayloadCache::default().convert(&audio, false);
        assert_eq!(converted.payload.as_deref(), Some(&expected_audio[..]));
        assert_eq!(converted.parameter_sets, None);
    }

    #[test]
    fn outputs_sharing_a_cache_convert_once_and_send_the_same_bytes() {
        let packets = [
            raw(MediaType::Video, H264_KEYFRAME, true, 1_000),
            raw(MediaType::Audio, &adts_frame(), false, 1_010),
            raw(
                MediaType::Video,
                &[0, 0, 0, 1, 0x41, 0x9a, 0x01],
                false,
                1_040,
            ),
        ];
        let encode_all = |encoder: &mut RtmpMediaEncoder| {
            let mut actions = Vec::new();
            for packet in &packets {
                encoder.encode(packet, &mut actions);
            }
            actions
        };
        let private = encode_all(&mut RtmpMediaEncoder::new(false, Vec::new()));

        let shared = RtmpPayloadCache::shared();
        let mut outputs = Vec::new();
        for _ in 0..3 {
            let mut encoder = RtmpMediaEncoder::new(false, Vec::new());
            encoder.share_payload_cache(shared.clone());
            outputs.push(encode_all(&mut encoder));
        }
        for actions in &outputs {
            assert_eq!(actions, &private, "sharing must not change what is sent");
        }
        let payload_ptr = |action: &RtmpMediaAction| match action {
            RtmpMediaAction::Video { payload, .. } | RtmpMediaAction::Audio { payload, .. } => {
                payload.as_ptr()
            }
        };
        let last = private.len() - 1;
        assert_eq!(
            payload_ptr(&outputs[0][last]),
            payload_ptr(&outputs[2][last]),
            "later outputs reuse the first output's buffer instead of copying"
        );
        assert_eq!(
            shared.borrow().hits_and_misses(),
            (6, 3),
            "three packets converted once, reused by two more outputs"
        );
    }

    #[test]
    fn distinct_packets_with_equal_timing_never_share_a_conversion() {
        let mut cache = RtmpPayloadCache::default();
        let first = raw(MediaType::Audio, &adts_frame(), false, 500);
        let mut other_frame = adts_frame();
        other_frame[9] = 0x77;
        let second = raw(MediaType::Audio, &other_frame, false, 500);
        let a = cache.convert(&first, false);
        let b = cache.convert(&second, false);
        assert_ne!(a.payload, b.payload);
        assert_eq!(cache.hits_and_misses(), (0, 2));
        assert_ne!(
            cache.convert(&first, true).payload.map(|p| p.as_ptr()),
            a.payload.map(|p| p.as_ptr()),
            "a different enhanced-HEVC mode is a different conversion"
        );
    }

    #[test]
    fn an_evicted_packet_converts_again_to_the_same_bytes() {
        let mut cache = RtmpPayloadCache::default();
        let early = raw(MediaType::Video, H264_KEYFRAME, true, 0);
        let expected = cache.convert(&early, false);
        let later: Vec<MediaPacket> = (1..=(4 * ENTRIES as i64))
            .map(|n| raw(MediaType::Video, H264_KEYFRAME, true, n * 40))
            .collect();
        for packet in &later {
            cache.convert(packet, false);
        }
        assert_eq!(cache.convert(&early, false), expected);
    }
}
