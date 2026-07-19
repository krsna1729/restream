use bytes::Bytes;
use rml_rtmp::chunk_io::ChunkSerializer;
use rml_rtmp::messages::RtmpMessage;
use rml_rtmp::rml_amf0::Amf0Value;
use rml_rtmp::sessions::ClientSessionConfig;
use rml_rtmp::time::RtmpTimestamp;
use std::collections::HashMap;

use crate::media::codec;

pub(super) fn enhanced_rtmp_connect_packet(
    config: &ClientSessionConfig,
    app_name: &str,
) -> Result<Vec<u8>, String> {
    let mut properties = HashMap::new();
    properties.insert(
        "app".to_string(),
        Amf0Value::Utf8String(app_name.to_string()),
    );
    properties.insert(
        "flashVer".to_string(),
        Amf0Value::Utf8String(config.flash_version.clone()),
    );
    properties.insert("objectEncoding".to_string(), Amf0Value::Number(0.0));
    if let Some(tc_url) = config.tc_url.as_ref() {
        properties.insert("tcUrl".to_string(), Amf0Value::Utf8String(tc_url.clone()));
    }
    properties.insert(
        "fourCcList".to_string(),
        Amf0Value::StrictArray(vec![
            Amf0Value::Utf8String("hvc1".to_string()),
            Amf0Value::Utf8String("avc1".to_string()),
            Amf0Value::Utf8String("mp4a".to_string()),
        ]),
    );

    let mut video_fourcc_info = HashMap::new();
    video_fourcc_info.insert("hvc1".to_string(), Amf0Value::Number(0x06 as f64));
    video_fourcc_info.insert("avc1".to_string(), Amf0Value::Number(0x06 as f64));
    properties.insert(
        "videoFourCcInfoMap".to_string(),
        Amf0Value::Object(video_fourcc_info),
    );

    let mut audio_fourcc_info = HashMap::new();
    audio_fourcc_info.insert("mp4a".to_string(), Amf0Value::Number(0x06 as f64));
    properties.insert(
        "audioFourCcInfoMap".to_string(),
        Amf0Value::Object(audio_fourcc_info),
    );

    let message = RtmpMessage::Amf0Command {
        command_name: "connect".to_string(),
        transaction_id: 1.0,
        command_object: Amf0Value::Object(properties),
        additional_arguments: Vec::new(),
    };
    let payload = message
        .into_message_payload(RtmpTimestamp::new(0), 0)
        .map_err(|error| format!("failed to build enhanced RTMP connect: {error:?}"))?;
    ChunkSerializer::new()
        .serialize(&payload, false, false)
        .map(|packet| packet.bytes)
        .map_err(|error| format!("failed to serialize enhanced RTMP connect: {error:?}"))
}

pub(super) fn cache_hevc_parameter_sets(payload: &[u8], cache: &mut Vec<u8>) {
    let Some(parameter_sets) = codec::annexb_parameter_sets(payload) else {
        return;
    };
    if codec::build_hevc_enhanced_rtmp_sequence_header(&parameter_sets).is_some() {
        *cache = parameter_sets;
    }
}

pub(super) fn raw_packet_starts_with_hevc_parameter_set(payload: &[u8]) -> bool {
    let Some(first_nalu) = codec::split_annexb_nalus(payload).first().copied() else {
        return false;
    };
    first_nalu
        .first()
        .is_some_and(|byte| matches!((byte >> 1) & 0x3F, 32..=34))
        && codec::build_hevc_enhanced_rtmp_sequence_header(payload).is_some()
}

pub(super) fn hevc_sequence_header_for_keyframe(
    payload: &[u8],
    parameter_sets_cache: &[u8],
) -> Option<(Bytes, Option<Vec<u8>>)> {
    let sequence_header = codec::build_hevc_enhanced_rtmp_sequence_header(payload)
        .or_else(|| codec::build_hevc_enhanced_rtmp_sequence_header(parameter_sets_cache))?;
    let config = codec::annexb_parameter_sets(payload)
        .or_else(|| codec::annexb_parameter_sets(parameter_sets_cache));
    Some((sequence_header, config))
}
