//! The listener's per-connection half-close policy, admission, header
//! deadline and shutdown drain.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::routing::{get, put};
use futures_util::StreamExt as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use super::{ServeOptions, serve, targets_hls_upload};

#[test]
fn the_first_request_line_decides_the_policy() {
    assert_eq!(
        targets_hls_upload(b"PUT /ingest/hls/key/index0.ts HTTP/1.1\r\n"),
        Some(true)
    );
    assert_eq!(targets_hls_upload(b"POST /ingest/hls/"), Some(true));
    assert_eq!(
        targets_hls_upload(b"GET /api/v1/state/stream HTTP/1.1"),
        Some(false)
    );
    assert_eq!(
        targets_hls_upload(b"GET /ingest/x"),
        Some(false),
        "settled at the first differing byte"
    );
    assert_eq!(
        targets_hls_upload(b"PUT /ingest/h"),
        None,
        "a prefix so far needs more bytes"
    );
    assert_eq!(targets_hls_upload(b"PU"), None);
    assert_eq!(
        targets_hls_upload(b"XXXXXXXXXXXXXXXXXXXX"),
        Some(false),
        "no request line"
    );
}

/// Drops its sender when the response body is dropped: when the server has
/// let go of the stream.
struct Released(Option<oneshot::Sender<()>>);

impl Drop for Released {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

async fn start(app: Router) -> SocketAddr {
    start_with(app, ServeOptions::new(1_024)).await
}

async fn start_with(app: Router, options: ServeOptions) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, app, options));
    address
}

/// A dashboard client that leaves a streaming response is noticed when it
/// goes, not at a later write: the stream here never writes again, so only
/// the read side's EOF can release it.
#[tokio::test]
async fn a_departed_streaming_client_is_released_at_once() {
    let (released, on_release) = oneshot::channel();
    let guard = std::sync::Arc::new(std::sync::Mutex::new(Some(Released(Some(released)))));
    let app = Router::new().route(
        "/stream",
        get(move || {
            let guard = restream_sync::lock::lock(&guard).take();
            async move {
                let first = futures_util::stream::once(async {
                    Ok::<_, std::io::Error>(Bytes::from_static(b"data: hello\n\n"))
                });
                let body = first
                    .chain(futures_util::stream::pending())
                    .map(move |item| {
                        let _ = &guard;
                        item
                    });
                Body::from_stream(body)
            }
        }),
    );
    let address = start(app).await;
    let mut client = TcpStream::connect(address).await.unwrap();
    client
        .write_all(b"GET /stream HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .unwrap();
    let mut first = [0u8; 256];
    let read = tokio::time::timeout(Duration::from_secs(2), client.read(&mut first))
        .await
        .expect("the stream answers")
        .unwrap();
    assert!(String::from_utf8_lossy(&first[..read]).contains("hello"));
    drop(client);

    tokio::time::timeout(Duration::from_secs(2), on_release)
        .await
        .expect("the stream is released when its client goes, not at a later write")
        .unwrap();
}

/// An HLS upload whose client half-closes right after the body, as FFmpeg
/// does without `-http_persistent 1`, is still read whole.
#[tokio::test]
async fn an_upload_half_closed_after_its_body_is_read() {
    let (received, on_receive) = oneshot::channel();
    let received = std::sync::Arc::new(std::sync::Mutex::new(Some(received)));
    let app = Router::new().route(
        "/ingest/hls/{key}/{*path}",
        put(move |body: Bytes| {
            if let Some(sender) = restream_sync::lock::lock(&received).take() {
                let _ = sender.send(body);
            }
            async { "ok" }
        }),
    );
    let address = start(app).await;
    let mut client = TcpStream::connect(address).await.unwrap();
    client
        .write_all(b"PUT /ingest/hls/key/index0.ts HTTP/1.1\r\nHost: test\r\nContent-Length: 5\r\n\r\nhello")
        .await
        .unwrap();
    client.shutdown().await.unwrap();

    let body = tokio::time::timeout(Duration::from_secs(2), on_receive)
        .await
        .expect("the half-closed upload reaches its handler")
        .unwrap();
    assert_eq!(&body[..], b"hello");
}

/// A request line that arrives in pieces, with a stall between them, is
/// served whole: the bytes read to classify the connection reach hyper ahead
/// of the rest. Both policies replay them.
#[tokio::test]
async fn a_request_line_split_by_a_stall_is_served_whole() {
    let app = Router::new()
        .route("/api/ping", get(|| async { "pong" }))
        .route(
            "/ingest/hls/{key}/{*path}",
            put(|body: Bytes| async move { body }),
        );
    let address = start(app).await;

    for (first, rest, expected) in [
        (
            &b"GE"[..],
            &b"T /api/ping HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n"[..],
            "pong",
        ),
        (
            &b"PUT /ingest/h"[..],
            &b"ls/key/index0.ts HTTP/1.1\r\nHost: test\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"[..],
            "hello",
        ),
    ] {
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(first).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        client.write_all(rest).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut response))
            .await
            .expect("the request is answered")
            .unwrap();
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with(expected), "{response}");
    }
}

/// Short deadlines so a test observes them; production uses the defaults.
fn options(max_connections: usize) -> ServeOptions {
    ServeOptions {
        max_connections,
        header_read_timeout: Duration::from_millis(300),
    }
}

fn ping_app() -> Router {
    Router::new().route("/api/ping", get(|| async { "pong" }))
}

/// A client that stalls mid-head, or goes quiet on a keep-alive connection,
/// is closed at the header deadline: a silent socket cannot hold its
/// admission permit forever.
#[tokio::test]
async fn a_silent_client_is_closed_at_the_header_deadline() {
    let address = start_with(ping_app(), options(8)).await;

    let mut partial = TcpStream::connect(address).await.unwrap();
    partial
        .write_all(b"GET /api/ping HTTP/1.1\r\nHost:")
        .await
        .unwrap();
    let mut idle = TcpStream::connect(address).await.unwrap();
    idle.write_all(b"GET /api/ping HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .unwrap();
    let mut first = [0u8; 512];
    let read = tokio::time::timeout(Duration::from_secs(2), idle.read(&mut first))
        .await
        .expect("the keep-alive request is answered")
        .unwrap();
    assert!(first[..read].starts_with(b"HTTP/1.1 200"));

    for (name, client) in [
        ("stalled head", &mut partial),
        ("idle keep-alive", &mut idle),
    ] {
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), client.read_to_end(&mut rest))
            .await
            .unwrap_or_else(|_| panic!("the {name} connection is closed at the header deadline"))
            .unwrap();
    }
}

/// Past `max_connections` a connection waits for a permit instead of being
/// served, and is served as soon as one is released.
#[tokio::test]
async fn a_connection_past_the_limit_waits_for_a_free_permit() {
    let address = start_with(
        ping_app(),
        ServeOptions {
            header_read_timeout: Duration::from_secs(10),
            ..options(1)
        },
    )
    .await;
    let mut holder = TcpStream::connect(address).await.unwrap();
    holder
        .write_all(b"GET /api/ping HTTP/1.1\r\nHost:")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut waiting = TcpStream::connect(address).await.unwrap();
    waiting
        .write_all(b"GET /api/ping HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut early = [0u8; 64];
    assert!(
        tokio::time::timeout(Duration::from_millis(300), waiting.read(&mut early))
            .await
            .is_err(),
        "a connection past the limit must not be served while the permit is held"
    );

    drop(holder);
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), waiting.read_to_end(&mut response))
        .await
        .expect("the waiting connection is served once the permit is released")
        .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200"));
}
