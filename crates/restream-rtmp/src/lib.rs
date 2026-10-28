//! Low-level RTMP protocol handling, egress engine, and payload formatting.

pub mod client_slots;
pub mod egress_connection;
pub mod egress_engine;
pub mod egress_metadata;
pub mod egress_packets;
pub mod egress_payload_cache;
pub mod egress_transport;
pub mod enhanced;
pub mod enhanced_rtmp_hevc;
pub mod flv;
pub mod parser_budget;
pub mod timestamps;

pub use client_slots::ClientSlots;
pub use egress_connection::{RtmpSessionCore, RtmpSessionError, RtmpSessionEvent};
pub use egress_engine::{RtmpMediaAction, RtmpMediaEncoder};
pub use egress_metadata::{
    RTMP_METADATA_VIDEO_CODEC_ID_AVC, RTMP_METADATA_VIDEO_CODEC_ID_HEVC,
    build_rtmp_stream_metadata, rtmp_output_video_codec, validate_rtmp_output_audio_tracks,
};
pub use egress_packets::{
    resolve_deferred_audio_sequence_header, rtmp_output_waits_for_video,
    rtmp_video_packet_can_be_dropped, should_defer_audio_until_video_ready,
    should_send_startup_audio_sequence_header, startup_video_sequence_header,
    validate_rtmp_output_audio_packet_track, video_sequence_header,
};
pub use egress_payload_cache::{RtmpPayloadCache, SharedRtmpPayloadCache};
pub use egress_transport::{RtmpUrlParts, parse_rtmp_url};
pub use enhanced::enhanced_rtmp_connect_packet;
pub use enhanced_rtmp_hevc::{
    EX_CODED_FRAMES, EX_CODED_FRAMES_X, EX_SEQUENCE_START, ExVideoTag, HevcProfileTierLevel,
    HvccRecord, build_hevc_enhanced_rtmp_sequence_header, ex_video_tag,
    hevc_video_for_enhanced_rtmp_with_composition_into, hvcc_record, parse_hevc_profile_tier_level,
};
pub use flv::{
    AvccRecord, FlvVideoPacketKind, annexb_to_avcc_into, audio_for_rtmp_into, avcc_record,
    classify_flv_video_packet, enhanced_video_meta, flv_config_annexb_parameter_sets,
    flv_video_composition_time_ms, flv_video_config, parse_flv_audio_meta, parse_flv_video_meta,
    strip_adts, video_for_rtmp_into, video_for_rtmp_with_composition_into,
};
pub use parser_budget::{ParserBudget, ParserCharge, held_bytes};
pub use timestamps::{RtmpTimestampGuard, refreshed_video_sequence_header_timestamp};
