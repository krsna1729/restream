//! Core media vocabulary shared across all Restream subsystems.
//!
//! These types define the data-path invariants:
//! - [`MediaPacket`]: The 56-byte cache-line aligned packet envelope carrying `bytes::Bytes`.
//! - [`MediaType`] and [`PayloadFormat`]: Ingest and egress format descriptors.
//! - [`FrameUnit`], [`TrackConfig`], and [`NotRetained`]: Side-band track configuration history.
#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod codec;
pub use codec::*;
pub mod metadata;
pub use metadata::{AudioMeta, DeclaredEncoder, VideoMeta};

use bytes::Bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MediaType {
    Video = 0,
    Audio = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PayloadFormat {
    Flv = 0,
    Raw = 1,
}

/// Tracks per media type that can carry a configuration. A packet with a
/// higher track index publishes with configuration generation 0.
pub const CONFIG_TRACKS_PER_TYPE: usize = 4;

/// Configurations retained per track. A packet whose generation is this many
/// changes old no longer resolves ([`NotRetained`]).
pub const CONFIG_HISTORY: usize = 4;

/// Set in every recorded unit, so an unwritten slot (raw value 0) reads as no
/// unit at all.
const PRESENT: u64 = 1 << 63;
/// The low 31 bits of the sequence that wrote a unit, stored above the
/// generation.
const TAG_MASK: u64 = (1 << 31) - 1;

/// The sequence tag stored beside a published unit's configuration generation.
#[inline]
pub fn unit_tag(sequence: usize) -> u64 {
    sequence as u64 & TAG_MASK
}

/// Metadata recorded by the ring for one published packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameUnit {
    /// Generation of this packet's track configuration when it was published;
    /// 0 when its producer had recorded none.
    pub config_generation: u32,
}

impl FrameUnit {
    /// The unit word for the packet published at `sequence`.
    #[inline]
    pub fn pack(self, sequence: usize) -> u64 {
        PRESENT | unit_tag(sequence) << 32 | u64::from(self.config_generation)
    }

    /// The unit in `raw` if it was written for `sequence`; `None` for an
    /// unwritten slot or one a later publication has overwritten.
    #[inline]
    pub fn unpack(raw: u64, sequence: usize) -> Option<Self> {
        (raw & PRESENT != 0 && (raw >> 32) & TAG_MASK == unit_tag(sequence)).then_some(Self {
            config_generation: raw as u32,
        })
    }
}

/// One generation of a track's codec configuration.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct TrackConfig {
    pub generation: u32,
    pub bytes: Bytes,
}

/// The packet at the requested sequence was overwritten (or never written),
/// or its configuration generation fell out of [`CONFIG_HISTORY`]. A reader
/// treats this like a ring overrun.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotRetained;

/// 56-byte media packet. `#[repr(C)]` pins field order for cache-line alignment.
#[derive(Clone, Debug)]
#[repr(C)]
pub struct MediaPacket {
    pub media_type: MediaType,
    pub format: PayloadFormat,
    pub is_keyframe: bool,
    pub track_index: u32,
    pub pts: i64,
    pub dts: i64,
    pub payload: Bytes,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_packet_layout_hot_fields_in_first_cache_line() {
        assert_eq!(
            std::mem::size_of::<MediaPacket>(),
            56,
            "MediaPacket must remain exactly 56 bytes"
        );

        let packet = MediaPacket {
            media_type: MediaType::Video,
            format: PayloadFormat::Raw,
            is_keyframe: false,
            track_index: 0xDEAD_BEEF,
            pts: 0,
            dts: 0,
            payload: Bytes::new(),
        };
        let base = &packet as *const MediaPacket as usize;
        let media_type_offset = &packet.media_type as *const MediaType as usize - base;
        let payload_offset = &packet.payload as *const Bytes as usize - base;

        assert_eq!(media_type_offset, 0, "media_type must remain first");
        assert!(
            payload_offset >= 24,
            "payload must remain after the timestamps"
        );
        assert_eq!(std::mem::size_of::<MediaType>(), 1);
        assert_eq!(std::mem::size_of::<PayloadFormat>(), 1);
    }

    #[test]
    fn frame_unit_pack_unpack() {
        let unit = FrameUnit {
            config_generation: 42,
        };
        let packed = unit.pack(100);
        assert_eq!(FrameUnit::unpack(packed, 100), Some(unit));
        assert_eq!(FrameUnit::unpack(packed, 101), None);
    }

    #[test]
    fn parse_avcc_config_fails_closed_on_truncated_pps() {
        use crate::codec::parse_avcc_config;
        // Truncated PPS: PPS count is 1, length is 2, but only 1 byte (104) is provided.
        let truncated = [1, 66, 0, 30, 255, 225, 0, 1, 103, 1, 0, 2, 104];
        let (nalu_len_size, annexb) = parse_avcc_config(&truncated);
        assert_eq!(nalu_len_size, 4);
        assert!(annexb.is_empty(), "must fail closed when PPS is truncated");

        // Complete record: SPS (len 1: [103]) and PPS (len 1: [104]).
        let complete = [1, 66, 0, 30, 255, 225, 0, 1, 103, 1, 0, 1, 104];
        let (nalu_len_size, annexb) = parse_avcc_config(&complete);
        assert_eq!(nalu_len_size, 4);
        assert_eq!(annexb, [0, 0, 0, 1, 103, 0, 0, 0, 1, 104]);
    }

    #[test]
    fn avcc_builders_accept_sps_without_pps() {
        let annexb = [0, 0, 0, 1, 0x67, 0x42, 0, 0x1E, 0xAB];
        let record = build_avcc_record(&annexb).expect("SPS without PPS is representable");
        let parsed = avcc_record(&record).expect("complete AVC record");
        assert_eq!(parsed.length_size_minus_one, 3);
        assert_eq!(parsed.sps, [vec![0x67, 0x42, 0, 0x1E, 0xAB].as_slice()]);
        assert!(parsed.pps.is_empty());
        assert_eq!(parse_avcc_config(&record), (4, annexb.to_vec()));

        let header = build_avcc_sequence_header(&annexb).expect("SPS-only FLV header");
        let (framing, record) = header.split_at(5);
        assert_eq!(framing, [0x17, 0, 0, 0, 0]);
        assert_eq!(parse_avcc_config(record), (4, annexb.to_vec()));
    }
}
