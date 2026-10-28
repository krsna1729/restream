//! Canonical checked codec parsers and packet framing shared by every transport.
//!
//! Root media adapters, tests, fuzz targets and benchmarks use these same
//! implementations; transport crates must not keep private parser copies.
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

mod aac;
mod bits;
mod enhanced_rtmp_hevc;
mod h264_sps;
mod video;

pub use aac::*;
pub use bits::{BitReader, rbsp};
pub use enhanced_rtmp_hevc::*;
pub use h264_sps::{H264Sps, level_name, parse_h264_sps, profile_name};
pub use video::*;

/// Neutral codec descriptor shared by transport and domain contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodecKind {
    H264,
    Hevc,
    Unknown,
}

impl VideoCodecKind {
    pub fn from_codec_name(codec: &str) -> Self {
        if codec.eq_ignore_ascii_case("h264") || codec.eq_ignore_ascii_case("avc") {
            Self::H264
        } else if codec.eq_ignore_ascii_case("h265") || codec.eq_ignore_ascii_case("hevc") {
            Self::Hevc
        } else {
            Self::Unknown
        }
    }

    pub fn is_hevc(self) -> bool {
        matches!(self, Self::Hevc)
    }

    pub const fn as_stage_codec(self) -> Option<&'static str> {
        match self {
            Self::H264 => Some("h264"),
            Self::Hevc => Some("hevc"),
            Self::Unknown => None,
        }
    }
}
