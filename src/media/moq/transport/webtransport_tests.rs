use bytes::Bytes;
use compio::quic::{Connection, ConnectionError, VarInt};
use futures_util::future::join;
use moq_net::transport::poll::Session as _;
use web_transport_trait::poll::Session as _;

use super::session_tests::{
    dropped_stream_is_reset, exchange_streams, last_drop_closes, moq_lite_round_trip,
    peer_close_is_observed,
};
use super::test_support::{LOOPBACK_SERVER_NAME, Loopback, loopback_client_tls, run};
use super::webtransport::{
    H3_MESSAGE_ERROR, H3_REQUEST_REJECTED, H3_STREAM_CREATION_ERROR, WT_BUFFERED_STREAM_REJECTED,
};
use super::*;

const URL: &str = "https://localhost/live/key?token=abc";

async fn pair(loopback: &Loopback) -> (MoqSession, MoqSession) {
    let (client, accepted) = loopback.webtransport(URL, CLIENT_ALPNS).await;
    (
        client.expect("WebTransport connect"),
        accepted.expect("WebTransport accept").session,
    )
}

/// Keeps `session` accepting uni and bi streams for as long as the returned
/// handles live, so its header checks run.
fn keep_accepting(session: &MoqSession) -> [compio::runtime::JoinHandle<()>; 2] {
    let mut uni = session.clone();
    let mut bi = session.clone();
    [
        compio::runtime::spawn(async move { while uni.accept_uni().await.is_ok() {} }),
        compio::runtime::spawn(async move { while bi.accept_bi().await.is_ok() {} }),
    ]
}

async fn send_raw_uni(conn: &Connection, bytes: &'static [u8]) -> Option<VarInt> {
    let mut raw = conn.open_uni_wait().await.unwrap();
    raw.write_all_chunks(&mut [Bytes::from_static(bytes)])
        .await
        .unwrap();
    raw.stopped().await.unwrap()
}

#[test]
fn connect_picks_the_servers_preferred_offered_subprotocol() {
    run(async {
        let loopback = Loopback::new().await;
        let (client, accepted) = loopback
            .webtransport(URL, &["moq-transport-17", "moq-lite-03", "moq-lite-06"])
            .await;
        let (client, accepted) = (client.unwrap(), accepted.unwrap());
        assert_eq!(accepted.mode, TransportMode::WebTransport);
        assert_eq!(accepted.path, "/live/key");
        assert_eq!(accepted.query.as_deref(), Some("token=abc"));
        assert_eq!(accepted.session.protocol(), Some("moq-lite-06"));
        assert_eq!(client.protocol(), Some("moq-lite-06"));
    });
}

#[test]
fn webtransport_moq_lite_session_round_trips_a_frame() {
    run(async {
        let loopback = Loopback::new().await;
        let (client, server) = pair(&loopback).await;
        moq_lite_round_trip(client, server).await;
    });
}

#[test]
fn webtransport_streams_carry_bytes_behind_their_headers() {
    run(async {
        let loopback = Loopback::new().await;
        let (mut client, mut server) = pair(&loopback).await;
        exchange_streams(&mut client, &mut server).await;
        exchange_streams(&mut server, &mut client).await;
        dropped_stream_is_reset(&mut client, &mut server).await;
    });
}

#[test]
fn webtransport_peer_close_arrives_as_the_close_capsule() {
    run(async {
        let loopback = Loopback::new().await;
        let (client, server) = pair(&loopback).await;
        // The QUIC close follows a second later with H3_NO_ERROR (code 0),
        // so code 7 can only come from the capsule.
        peer_close_is_observed(client, server).await;
    });
}

#[test]
fn webtransport_session_closes_when_its_last_clone_drops() {
    run(async {
        let loopback = Loopback::new().await;
        let (client, server) = pair(&loopback).await;
        last_drop_closes(client, server).await;
    });
}

#[test]
fn a_connect_without_a_supported_subprotocol_is_rejected() {
    run(async {
        let loopback = Loopback::new().await;
        let (client, accepted) = loopback.webtransport(URL, &["moq-transport-17"]).await;
        assert!(
            matches!(accepted, Err(TransportError::Handshake(reason)) if reason.contains("subprotocol"))
        );
        assert!(client.is_err());
    });
}

#[test]
fn a_stream_for_another_session_is_stopped() {
    run(async {
        let loopback = Loopback::new().await;
        let (client, server) = pair(&loopback).await;
        let _accepting = keep_accepting(&server);
        // Uni stream type 0x54 (two-byte varint), session id 4: ours is 0.
        let stopped = send_raw_uni(client.connection(), &[0x40, 0x54, 0x04]).await;
        assert_eq!(stopped, Some(VarInt::from_u32(WT_BUFFERED_STREAM_REJECTED)));
    });
}

#[test]
fn an_unknown_uni_stream_is_stopped() {
    run(async {
        let loopback = Loopback::new().await;
        let (client, server) = pair(&loopback).await;
        let _accepting = keep_accepting(&server);
        // 0x21: a reserved (grease) stream type.
        let stopped = send_raw_uni(client.connection(), &[0x21, 0x00]).await;
        assert_eq!(stopped, Some(VarInt::from_u32(H3_STREAM_CREATION_ERROR)));
    });
}

#[test]
fn a_second_http3_control_stream_is_stopped() {
    run(async {
        let loopback = Loopback::new().await;
        let (client, server) = pair(&loopback).await;
        let _accepting = keep_accepting(&server);
        // The client's own control stream is already held.
        let stopped = send_raw_uni(client.connection(), &[0x00]).await;
        assert_eq!(stopped, Some(VarInt::from_u32(H3_STREAM_CREATION_ERROR)));
    });
}

#[test]
fn a_bidi_stream_that_is_not_webtransport_is_rejected() {
    run(async {
        let loopback = Loopback::new().await;
        let (client, server) = pair(&loopback).await;
        let _accepting = keep_accepting(&server);
        let (mut send, mut recv) = client.connection().open_bi_wait().await.unwrap();
        // A HEADERS frame (a second request), not a WebTransport stream.
        send.write_all_chunks(&mut [Bytes::from_static(&[0x01, 0x00])])
            .await
            .unwrap();
        assert_eq!(
            send.stopped().await.unwrap(),
            Some(VarInt::from_u32(H3_REQUEST_REJECTED))
        );
        assert_eq!(
            recv.read_chunk(16, true).await.unwrap_err(),
            compio::quic::ReadError::Reset(VarInt::from_u32(H3_REQUEST_REJECTED))
        );
    });
}

/// A raw `h3` connection that sends `connect` on its first bi stream and
/// waits for the connection to close; with the listener's `accept` result.
async fn raw_connect(
    loopback: &Loopback,
    connect: Bytes,
) -> (ConnectionError, Result<Accepted, TransportError>) {
    let client = async {
        let conn = loopback
            .client
            .connect(
                loopback.addr,
                LOOPBACK_SERVER_NAME,
                Some(loopback_client_tls(&["h3"])),
            )
            .unwrap()
            .await
            .unwrap();
        let (mut send, _recv) = conn.open_bi_wait().await.unwrap();
        send.write_all_chunks(&mut [connect]).await.unwrap();
        let _ = send.finish();
        conn.closed().await
    };
    join(client, loopback.accept()).await
}

#[test]
fn malformed_connect_headers_are_rejected_without_a_panic() {
    // The overflow panics inside the contained decode; keep its expected
    // panic message out of the passing log.
    crate::test_support::with_expected_panic_suppressed(|| {
        run(async {
            let loopback = Loopback::new().await;
            // HEADERS whose QPACK prefix integer overflows: web-transport-proto's
            // decoder panics on it under debug assertions.
            let mut overflow = vec![0xff_u8; 13];
            overflow[..2].copy_from_slice(&[0x01, 11]);
            let missing_pseudo_headers = Bytes::from_static(&[0x01, 0x02, 0x00, 0x00]);
            for headers in [Bytes::from(overflow), missing_pseudo_headers] {
                let (closed, accepted) = raw_connect(&loopback, headers).await;
                assert!(
                    matches!(accepted, Err(TransportError::Handshake(reason)) if reason.contains("CONNECT"))
                );
                assert!(
                    matches!(&closed, ConnectionError::ApplicationClosed(close) if close.error_code == VarInt::from_u32(H3_MESSAGE_ERROR)),
                    "{closed}"
                );
            }
        })
    });
}

#[test]
fn a_truncated_connect_frame_fails_the_handshake() {
    run(async {
        let loopback = Loopback::new().await;
        let (_closed, accepted) =
            raw_connect(&loopback, Bytes::from_static(&[0x01, 0x20, 0x00])).await;
        assert!(
            matches!(accepted, Err(TransportError::Handshake(reason)) if reason.contains("ended"))
        );
    });
}
