//! Minimal RTMP server session for `RESTREAM_SINK_MODE=1`: completes real
//! `connect`/`createStream`/`publish` protocol negotiation so a genuine
//! RTMP client (restream's own egress fabric included) proceeds past its
//! handshake and starts sending media, then discards every media message
//! without decoding it. Reuses `rml_rtmp::sessions::ServerSession` — the
//! same state machine real ingest drives (`ingest.rs`) — for the small
//! number of events sink mode must answer; every other event (video/audio
//! data, metadata, play requests) is intentionally dropped unread, since
//! discarding is the entire point of a sink peer.

use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Drive one accepted, already-handshaken RTMP connection until the peer
/// closes it or a read/write error occurs. `leading` is any chunk-stream
/// bytes the handshake already read past C2.
pub(super) async fn run_sink_rtmp_session(stream: &mut TcpStream, buf: &mut [u8], leading: &[u8]) {
    let config = ServerSessionConfig::new();
    let Ok((mut session, initial_results)) = ServerSession::new(config) else {
        return;
    };
    if write_outbound(stream, initial_results).await.is_err() {
        return;
    }
    if !leading.is_empty() {
        let Ok(results) = session.handle_input(leading) else {
            return;
        };
        if !accept_and_respond(stream, &mut session, results).await {
            return;
        }
    }
    loop {
        let n = match stream.read(buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let Ok(results) = session.handle_input(&buf[..n]) else {
            return;
        };
        if !accept_and_respond(stream, &mut session, results).await {
            return;
        }
    }
}

/// Writes any outbound response bytes and accepts `connect`/`publish`
/// requests so the peer's own state machine advances past them. Returns
/// `false` on a write failure (connection is dead; caller should stop).
async fn accept_and_respond(
    stream: &mut TcpStream,
    session: &mut ServerSession,
    results: Vec<ServerSessionResult>,
) -> bool {
    for result in results {
        match result {
            ServerSessionResult::OutboundResponse(packet) => {
                if stream.write_all(&packet.bytes).await.is_err() {
                    return false;
                }
            }
            ServerSessionResult::RaisedEvent(ServerSessionEvent::ConnectionRequested {
                request_id,
                ..
            }) => {
                let Ok(response) = session.accept_request(request_id) else {
                    return false;
                };
                if write_outbound(stream, response).await.is_err() {
                    return false;
                }
            }
            ServerSessionResult::RaisedEvent(ServerSessionEvent::PublishStreamRequested {
                request_id,
                ..
            }) => {
                let Ok(response) = session.accept_request(request_id) else {
                    return false;
                };
                if write_outbound(stream, response).await.is_err() {
                    return false;
                }
            }
            // Media, metadata, play requests, and everything else: sink
            // mode discards without decoding.
            ServerSessionResult::RaisedEvent(_)
            | ServerSessionResult::UnhandleableMessageReceived(_) => {}
        }
    }
    true
}

async fn write_outbound(
    stream: &mut TcpStream,
    results: Vec<ServerSessionResult>,
) -> Result<(), ()> {
    for result in results {
        if let ServerSessionResult::OutboundResponse(packet) = result {
            stream.write_all(&packet.bytes).await.map_err(|_| ())?;
        }
    }
    Ok(())
}
