use crate::{egress_connection, egress_transport};
use proptest::prelude::*;

// RTMP server responses to the egress client (`RtmpSessionCore`): the bytes
// a destination server sends after the handshake arrive in arbitrary TCP
// segments, so negotiation must not depend on where they are split.

fn egress_session() -> egress_connection::RtmpSessionCore {
    let parts = egress_transport::RtmpUrlParts {
        host: "127.0.0.1".to_string(),
        port: 1935,
        app: "live".to_string(),
        stream_key: "key".to_string(),
        tls: false,
    };
    egress_connection::RtmpSessionCore::new(parts, 4096).expect("RTMP session")
}

/// Runs the egress client against a real `rml_rtmp` server session that
/// accepts connect and publish. Returns the client's opening bytes and the
/// server's bytes per round: round N+1 only exists after the client answered
/// round N, so a test may split inside a round but never across rounds.
fn server_response_rounds() -> (Vec<u8>, Vec<Vec<u8>>) {
    let (_, opening, rounds) = negotiated_session(false);
    (opening, rounds)
}

/// The client, its opening bytes and the server's rounds, once the server
/// accepted connect (enhanced RTMP when `enhanced`) and publish.
fn negotiated_session(
    enhanced: bool,
) -> (egress_connection::RtmpSessionCore, Vec<u8>, Vec<Vec<u8>>) {
    use rml_rtmp::sessions::{
        ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
    };

    let mut client = egress_session();
    let mut opening: Vec<u8> = client.take_initial_packets().concat();
    opening.extend_from_slice(&client.request_connection(enhanced).expect("connect"));
    let (mut server, initial) = ServerSession::new(ServerSessionConfig::new()).expect("server");

    let mut to_server = opening.clone();
    let mut pending = Vec::new();
    for result in initial {
        if let ServerSessionResult::OutboundResponse(packet) = result {
            pending.extend_from_slice(&packet.bytes);
        }
    }
    let mut rounds = Vec::new();
    for _ in 0..8 {
        for result in server.handle_input(&to_server).expect("server input") {
            let request_id = match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    pending.extend_from_slice(&packet.bytes);
                    continue;
                }
                ServerSessionResult::RaisedEvent(
                    ServerSessionEvent::ConnectionRequested { request_id, .. }
                    | ServerSessionEvent::PublishStreamRequested { request_id, .. },
                ) => request_id,
                _ => continue,
            };
            for accepted in server.accept_request(request_id).expect("accept") {
                if let ServerSessionResult::OutboundResponse(packet) = accepted {
                    pending.extend_from_slice(&packet.bytes);
                }
            }
        }
        let (packets, events) = client.handle_server_input(&pending).expect("client input");
        rounds.push(std::mem::take(&mut pending));
        if events.contains(&egress_connection::RtmpSessionEvent::PublishRequestAccepted) {
            return (client, opening, rounds);
        }
        to_server = packets.concat();
    }
    panic!("publish was never accepted");
}

/// The first published media message establishes the stream id, after which
/// the engine sends media as zero-copy wire messages over the shared payload
/// instead of re-serializing every frame per output through `rml_rtmp`.
/// The enhanced connect spans several default-size (128-byte) chunks: the
/// session announces its own chunk size only after connect is accepted.
/// The frame spans several chunks too (a 1080p keyframe always does).
#[test]
fn the_first_media_message_establishes_the_media_stream_id() {
    for enhanced in [false, true] {
        let (mut client, _, _) = negotiated_session(enhanced);
        assert_eq!(client.media_stream_id(), None);
        let mut frame = vec![0x17, 1, 0, 0, 0];
        frame.resize(3 * 4096 + 100, 0xab);
        client
            .publish_video_data(
                bytes::Bytes::from(frame),
                rml_rtmp::time::RtmpTimestamp::new(0),
                false,
            )
            .expect("video packet");
        assert!(
            client.media_stream_id().is_some(),
            "no media stream id after an {} connect",
            if enhanced { "enhanced" } else { "plain" }
        );
    }
}

proptest! {
    /// Connect and publish negotiate to the same events, and the client
    /// answers with the same number of packets, wherever the server's
    /// responses are split into reads.
    #[test]
    fn egress_negotiation_does_not_depend_on_server_read_boundaries(
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..24),
    ) {
        let (_, rounds) = server_response_rounds();

        let mut whole = egress_session();
        let _ = whole.take_initial_packets();
        let _ = whole.request_connection(false).expect("connect");
        let mut split = egress_session();
        let _ = split.take_initial_packets();
        let _ = split.request_connection(false).expect("connect");

        let (mut whole_events, mut whole_packets) = (Vec::new(), 0);
        let (mut split_events, mut split_packets) = (Vec::new(), 0);
        for round in &rounds {
            let (packets, events) = whole.handle_server_input(round).expect("whole");
            whole_packets += packets.len();
            whole_events.extend(events);

            let mut points: Vec<usize> = cuts.iter().map(|cut| cut.index(round.len() + 1)).collect();
            points.extend([0, round.len()]);
            points.sort_unstable();
            points.dedup();
            for window in points.windows(2) {
                let (packets, events) =
                    split.handle_server_input(&round[window[0]..window[1]]).expect("split");
                split_packets += packets.len();
                split_events.extend(events);
            }
        }

        prop_assert_eq!(
            &whole_events,
            &vec![
                egress_connection::RtmpSessionEvent::ConnectionRequestAccepted,
                egress_connection::RtmpSessionEvent::PublishRequestAccepted,
            ]
        );
        prop_assert_eq!(split_events, whole_events);
        prop_assert_eq!(split_packets, whole_packets);
    }
}
