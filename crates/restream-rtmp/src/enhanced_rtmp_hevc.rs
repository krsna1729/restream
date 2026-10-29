//! Enhanced RTMP HEVC framing uses the canonical checked core implementation.
pub use restream_core::codec::{
    EX_CODED_FRAMES, EX_CODED_FRAMES_X, EX_SEQUENCE_START, ExVideoTag, HevcProfileTierLevel,
    HvccRecord, build_hevc_enhanced_rtmp_sequence_header, ex_video_tag,
    h265_annexb_to_length_prefixed_into, h265_nal_type,
    hevc_video_for_enhanced_rtmp_with_composition_into, hvcc_record, parse_hevc_profile_tier_level,
};
