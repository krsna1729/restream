//! Application payload adapters for MPEG-TS muxing and caption inspection.
//!
//! Codec parsing and RTMP framing live in `restream-core` and `restream-rtmp`.

mod aac;
mod captions;
mod video;

pub use aac::{audio_for_rtmp, audio_for_ts, audio_for_ts_into};
pub use captions::{FrameCaptions, frame_captions};
pub use video::{video_for_ts, video_for_ts_into};

#[cfg(test)]
mod tests;
