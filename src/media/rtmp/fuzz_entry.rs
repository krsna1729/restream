//! Entry points for the cargo-fuzz targets in `fuzz/` (built only with
//! `--cfg fuzzing`). Each one runs the production RTMP parsers on untrusted
//! bytes exactly as the ingest or egress path calls them; the parsers stay
//! private to `media::rtmp`.

use super::egress_connection::{RtmpSessionCore, RtmpSessionEvent};
use super::egress_transport::RtmpUrlParts;
use super::flv::{
    FlvVideoPacketKind, classify_flv_video_packet, flv_avcc_config_annexb_parameter_sets,
    flv_video_composition_time_ms, parse_flv_audio_meta, parse_flv_video_meta,
};

/// One FLV video tag body as `RtmpPublisherMedia::on_video` probes it.
pub fn flv_video_tag(data: &[u8]) {
    let kind = classify_flv_video_packet(data);
    let _ = flv_video_composition_time_ms(data);
    if let Some(sets) = flv_avcc_config_annexb_parameter_sets(data) {
        assert_eq!(kind, Some(FlvVideoPacketKind::SequenceHeader));
        assert!(
            sets.starts_with(&[0, 0, 0, 1]),
            "parameter sets are Annex-B"
        );
    }
    let _ = parse_flv_video_meta(data);
}

/// One FLV audio tag body as `RtmpPublisherMedia::on_audio` probes it
/// (AudioSpecificConfig for AAC).
pub fn flv_audio_tag(data: &[u8]) {
    if let Some(meta) = parse_flv_audio_meta(data) {
        assert!(
            meta.channels >= 1,
            "a probed stream has at least one channel"
        );
    }
}

/// Server bytes after the handshake, delivered to the egress client in the
/// given reads after it sent its connect request.
pub fn rtmp_server_responses<'a>(reads: impl IntoIterator<Item = &'a [u8]>) {
    let parts = RtmpUrlParts {
        host: "127.0.0.1".to_string(),
        port: 1935,
        app: "live".to_string(),
        stream_key: "key".to_string(),
        tls: false,
    };
    let mut session = RtmpSessionCore::new(parts, 4096).expect("RTMP session");
    let _ = session.take_initial_packets();
    session.request_connection(false).expect("connect request");
    let mut published = false;
    for read in reads {
        // A protocol error ends the session, as it closes the leaf.
        let Ok((_, events)) = session.handle_server_input(read) else {
            return;
        };
        for event in events {
            if event == RtmpSessionEvent::ConnectionRequestAccepted {
                assert!(!published, "connect is accepted before publish");
            }
            published |= event == RtmpSessionEvent::PublishRequestAccepted;
        }
    }
}

/// Bytes from an RTMP publisher after the handshake, delivered to the
/// ingest `ServerSession` in the given reads, with Restream's message-size
/// limit. A request event is accepted the way the control session does, so
/// the fuzzer reaches the publish and media states.
pub fn rtmp_client_requests<'a>(reads: impl IntoIterator<Item = &'a [u8]>) {
    use rml_rtmp::sessions::{
        ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
    };

    let mut config = ServerSessionConfig::new();
    config.max_message_length = 1 << 20;
    let (mut session, _) = ServerSession::new(config).expect("RTMP server session");
    for read in reads {
        let Ok(results) = session.handle_input(read) else {
            return;
        };
        for result in results {
            let ServerSessionResult::RaisedEvent(
                ServerSessionEvent::ConnectionRequested { request_id, .. }
                | ServerSessionEvent::PublishStreamRequested { request_id, .. }
                | ServerSessionEvent::PlayStreamRequested { request_id, .. },
            ) = result
            else {
                continue;
            };
            if session.accept_request(request_id).is_err() {
                return;
            }
        }
    }
}
