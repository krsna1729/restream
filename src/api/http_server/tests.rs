//! The listener's per-connection half-close policy, admission, header
//! deadline, progress budgets and shutdown drain.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::routing::{get, post, put};
use futures_util::StreamExt as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::{Pace, ServeOptions, serve, targets_hls_upload};

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
    start_with(app, ServeOptions::new(1_024, CancellationToken::new())).await
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
fn options(max_connections: usize, shutdown: &CancellationToken) -> ServeOptions {
    ServeOptions {
        max_connections,
        header_read_timeout: Duration::from_millis(300),
        progress: Pace {
            grace: Duration::from_millis(300),
            min_bytes_per_sec: 1_000,
        },
        drain_timeout: Duration::from_secs(5),
        shutdown: shutdown.clone(),
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
    let address = start_with(ping_app(), options(8, &CancellationToken::new())).await;

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
    let shutdown = CancellationToken::new();
    let address = start_with(
        ping_app(),
        ServeOptions {
            header_read_timeout: Duration::from_secs(10),
            ..options(1, &shutdown)
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

/// Shutdown lets an in-flight request finish, closes its keep-alive
/// connection after the response, refuses new connections, and returns
/// without waiting out the drain deadline.
#[tokio::test]
async fn shutdown_finishes_in_flight_responses_then_returns() {
    let (entered, on_enter) = oneshot::channel::<()>();
    let entered = std::sync::Arc::new(std::sync::Mutex::new(Some(entered)));
    let app = Router::new().route(
        "/slow",
        get(move || {
            if let Some(sender) = restream_sync::lock::lock(&entered).take() {
                let _ = sender.send(());
            }
            async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                "done"
            }
        }),
    );
    let shutdown = CancellationToken::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(serve(listener, app, options(8, &shutdown)));

    let mut client = TcpStream::connect(address).await.unwrap();
    client
        .write_all(b"GET /slow HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .unwrap();
    on_enter.await.unwrap();
    let cancelled_at = Instant::now();
    shutdown.cancel();

    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), client.read_to_end(&mut response))
        .await
        .expect("the keep-alive connection closes after its in-flight response")
        .unwrap();
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("done"), "{response}");

    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("serve returns once in-flight work is done")
        .unwrap();
    assert!(cancelled_at.elapsed() < Duration::from_secs(2));
    assert!(
        TcpStream::connect(address).await.is_err(),
        "the listener is closed"
    );
}

/// A response that never ends cannot hold shutdown past the drain deadline.
#[tokio::test]
async fn shutdown_returns_at_the_drain_deadline() {
    let app = Router::new().route(
        "/forever",
        get(|| async {
            let first = futures_util::stream::once(async {
                Ok::<_, std::io::Error>(Bytes::from_static(b"data: hello\n\n"))
            });
            Body::from_stream(first.chain(futures_util::stream::pending()))
        }),
    );
    let shutdown = CancellationToken::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(serve(
        listener,
        app,
        ServeOptions {
            drain_timeout: Duration::from_millis(300),
            ..options(8, &shutdown)
        },
    ));
    let mut client = TcpStream::connect(address).await.unwrap();
    client
        .write_all(b"GET /forever HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .unwrap();
    let mut first = [0u8; 256];
    tokio::time::timeout(Duration::from_secs(2), client.read(&mut first))
        .await
        .expect("the stream starts")
        .unwrap();

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("serve returns at the drain deadline")
        .unwrap();
}

/// A fresh `GET /api/ping` on its own connection: whether it got a 200.
async fn ping(address: SocketAddr) -> bool {
    ping_on(&mut TcpStream::connect(address).await.unwrap()).await
}

/// A last `GET /api/ping` on `client`: whether it got a 200.
async fn ping_on(client: &mut TcpStream) -> bool {
    client
        .write_all(b"GET /api/ping HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    let _ = client.read_to_end(&mut response).await;
    response.starts_with(b"HTTP/1.1 200")
}

/// A request body trickled below the minimum rate ends at its budget, so the
/// client cannot hold its permit by sending a byte before every idle period.
#[tokio::test]
async fn a_body_trickled_below_the_minimum_rate_frees_its_permit() {
    let app = ping_app().route(
        "/echo",
        post(|body: Bytes| async move { body.len().to_string() }),
    );
    let address = start_with(
        app,
        ServeOptions {
            header_read_timeout: Duration::from_secs(10),
            ..options(1, &CancellationToken::new())
        },
    )
    .await;
    let mut trickler = TcpStream::connect(address).await.unwrap();
    trickler
        .write_all(b"POST /echo HTTP/1.1\r\nHost: test\r\nContent-Length: 1000\r\n\r\n")
        .await
        .unwrap();
    // 20 B/s, 50 ms apart: shorter than any idle timer, far below the rate.
    let trickle = tokio::spawn(async move {
        for _ in 0..60 {
            if trickler.write_all(b"x").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let served = tokio::time::timeout(Duration::from_secs(2), ping(address))
        .await
        .expect("the trickled body's permit is freed at its budget");
    assert!(served);
    trickle.abort();
}

/// A client that takes part of a response and then stops reading is closed
/// once its writes stay blocked past the grace: credit earned by the bytes it
/// did take is capped, so a long healthy transfer banks no hours of it.
#[tokio::test]
async fn a_client_that_stops_reading_frees_its_permit() {
    let big = Bytes::from(vec![b'x'; 16 << 20]);
    let app = ping_app().route("/big", get(move || async move { big.clone() }));
    let address = start_with(
        app,
        ServeOptions {
            header_read_timeout: Duration::from_secs(10),
            ..options(1, &CancellationToken::new())
        },
    )
    .await;
    let socket = TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4_096).unwrap();
    let mut reader = socket.connect(address).await.unwrap();
    reader
        .write_all(b"GET /big HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .unwrap();
    let mut taken = vec![0u8; 2 << 20];
    reader.read_exact(&mut taken).await.unwrap();

    let served = tokio::time::timeout(Duration::from_secs(3), ping(address))
        .await
        .expect("the stalled reader's permit is freed once its writes stay blocked");
    assert!(served);
    drop(reader);
}

/// The budgets drain only while a transfer waits on the peer: a stream idle
/// between events, a handler slower than the grace (with or without a body)
/// and a steady client on a transfer longer than the grace are all served.
#[tokio::test]
async fn idle_streams_slow_handlers_and_steady_clients_outlive_the_grace() {
    let big = Bytes::from(vec![b'x'; 8 << 20]);
    let app = ping_app()
        .route(
            "/events",
            get(|| async {
                let events = futures_util::stream::iter(0..4).then(|index| async move {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    Ok::<_, std::io::Error>(Bytes::from(format!("data: {index}\n\n")))
                });
                Body::from_stream(events)
            }),
        )
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(600)).await;
                "slow"
            })
            .post(|body: Bytes| async move {
                tokio::time::sleep(Duration::from_millis(600)).await;
                body.len().to_string()
            }),
        )
        .route("/big", get(move || async move { big.clone() }));
    let address = start_with(
        app,
        ServeOptions {
            header_read_timeout: Duration::from_secs(10),
            ..options(8, &CancellationToken::new())
        },
    )
    .await;

    // Keep-alive, as browsers use it: hyper also reads the socket between
    // and during exchanges, where a socket-level read timer would fire.
    let exchange = |request: &'static [u8], end: &'static str| async move {
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(request).await.unwrap();
        let mut response = Vec::new();
        let mut chunk = [0u8; 4_096];
        tokio::time::timeout(Duration::from_secs(5), async {
            while !response.ends_with(end.as_bytes()) {
                let read = client.read(&mut chunk).await.unwrap();
                assert!(
                    read > 0,
                    "closed early: {}",
                    String::from_utf8_lossy(&response)
                );
                response.extend_from_slice(&chunk[..read]);
            }
        })
        .await
        .expect("the exchange completes");
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            ping_on(&mut client).await,
            "the connection is reused after idling past the grace"
        );
        String::from_utf8_lossy(&response).into_owned()
    };
    let events = exchange(b"GET /events HTTP/1.1\r\nHost: test\r\n\r\n", "0\r\n\r\n").await;
    assert!(events.contains("data: 3"), "{events}");
    exchange(b"GET /slow HTTP/1.1\r\nHost: test\r\n\r\n", "slow").await;
    exchange(
        b"POST /slow HTTP/1.1\r\nHost: test\r\nContent-Length: 10\r\n\r\n0123456789",
        "10",
    )
    .await;

    // About 8 MB/s, well above the rate, over a body larger than the socket
    // buffers, so the server's writes block for longer than the grace in total.
    let socket = TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(16_384).unwrap();
    let mut steady = socket.connect(address).await.unwrap();
    steady
        .write_all(b"GET /big HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut received = 0usize;
    let mut chunk = vec![0u8; 16_384];
    loop {
        let read = steady.read(&mut chunk).await.unwrap();
        if read == 0 {
            break;
        }
        received += read;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(
        received > 8 << 20,
        "the whole body arrives: {received} bytes"
    );
}
