//! Payload format conversions for the 2×3×2 ingest/egress matrix.
//!
//! Four entry points cover every path:
//!   - `video_for_ts` / `audio_for_ts` — prepare payloads for MPEG-TS muxing
//!   - `video_for_rtmp` / `audio_for_rtmp` — prepare payloads for RTMP publishing
//!
//! Lower-level AVCC, Annex B, AAC, and ADTS helpers remain available through
//! this façade for sequence-header synthesis and tests.

mod aac;
mod enhanced_rtmp_hevc;
mod video;

pub use aac::{
    adts_frame_count, audio_for_rtmp, audio_for_rtmp_into, audio_for_ts, audio_for_ts_into,
    build_aac_sequence_header, build_adts_header, strip_adts,
};
pub use enhanced_rtmp_hevc::{
    build_hevc_enhanced_rtmp_sequence_header, hevc_video_for_enhanced_rtmp_with_composition_into,
};
pub(crate) use video::{
    AnnexbParameterSetAccumulator, annexb_parameter_sets, raw_annexb_is_keyframe,
};
pub use video::{
    annexb_to_avcc, annexb_to_avcc_into, annexb_to_avcc_with_scratch, avcc_to_annexb,
    avcc_to_annexb_into, build_avcc_sequence_header, find_annexb_start_codes, parse_avcc_config,
    split_annexb_nalus, video_for_rtmp, video_for_rtmp_into, video_for_rtmp_with_composition_into,
    video_for_ts, video_for_ts_into,
};

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
