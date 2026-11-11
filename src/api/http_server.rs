//! The dashboard HTTP listener: the axum router on hyper HTTP/1.1, with
//! half-closed requests allowed on HLS push connections only.
//!
//! HLS push encoders (FFmpeg without `-http_persistent 1`) close their side
//! right after the request body and never read the response. hyper's default
//! treats that EOF as an abort and drops the request before its body is read,
//! so such an upload would be lost: those connections need half-close.
//!
//! Every other connection keeps hyper's default, because with half-close a
//! client EOF is not a disconnect: a client that left a streaming response
//! (SSE) would be noticed only when a write to it fails (20-40 s for the
//! state stream's 20 s heartbeat). Half-close is a per-connection setting
//! fixed before routing, so a connection is classified by its first request
//! line: an encoder opens a connection per upload, and one that keeps a
//! connection open uses it only for uploads. The line is read (a stalled
//! client costs nothing while it waits), and the bytes read are given back to
//! hyper ahead of the socket.
//!
//! Admission: at most `max_connections` connections are served at once. The
//! permit is taken before `accept`, so past the limit new connections wait in
//! the kernel backlog instead of costing a task, a buffer and a descriptor
//! from the table RTMP, SRT and FFmpeg pipes share. A client that sends no
//! complete request head within `header_read_timeout` (idle keep-alive
//! included) is closed, so silent sockets cannot hold permits forever.
//!
//! Shutdown: once `shutdown` is cancelled the listener closes, every live
//! connection finishes its in-flight response and then closes, and `serve`
//! returns when all have, or after `drain_timeout`.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tower::{Service, ServiceExt};
use tracing::{debug, error, warn};

/// How long a connection may take to send a complete request head, counted
/// from when the connection starts waiting for one. With the 5 s sniff this
/// bounds how long a silent socket holds a permit to about 15 s.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long shutdown waits for in-flight responses.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Admission, timeout and shutdown settings for [`serve`].
#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub max_connections: usize,
    pub header_read_timeout: Duration,
    pub drain_timeout: Duration,
    pub shutdown: CancellationToken,
}

impl ServeOptions {
    pub fn new(max_connections: usize, shutdown: CancellationToken) -> Self {
        Self {
            max_connections,
            header_read_timeout: HEADER_READ_TIMEOUT,
            drain_timeout: DRAIN_TIMEOUT,
            shutdown,
        }
    }
}

/// Serve `app` on `listener` until `options.shutdown` is cancelled, then
/// drain. Accept errors that concern one connection are skipped; others
/// (descriptor exhaustion) are logged and retried after a second, as
/// `axum::serve` does.
pub async fn serve(listener: TcpListener, app: Router, options: ServeOptions) {
    let max_connections = options.max_connections.clamp(1, Semaphore::MAX_PERMITS);
    let permits = Arc::new(Semaphore::new(max_connections));
    let mut make_service = app.into_make_service_with_connect_info::<SocketAddr>();
    loop {
        let permit = tokio::select! {
            () = options.shutdown.cancelled() => break,
            permit = Arc::clone(&permits).acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_closed) => break,
            },
        };
        let (stream, remote) = tokio::select! {
            () = options.shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(accept_error) => {
                    if !matches!(
                        accept_error.kind(),
                        io::ErrorKind::ConnectionRefused
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                    ) {
                        error!(err = %accept_error, "HTTP accept error");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    continue;
                }
            },
        };
        // Small JSON replies and SSE events are single short writes; without
        // this each waits on the client's delayed ACK of the previous one.
        if let Err(nodelay_error) = stream.set_nodelay(true) {
            debug!(err = %nodelay_error, "TCP_NODELAY not set on HTTP connection");
        }
        let service = match make_service.call(remote).await {
            Ok(service) => service,
            Err(never) => match never {},
        };
        let service = TowerToHyperService::new(
            service.map_request(|request: Request<Incoming>| request.map(Body::new)),
        );
        let shutdown = options.shutdown.clone();
        let header_read_timeout = options.header_read_timeout;
        tokio::spawn(async move {
            let _permit = permit;
            let (half_close, stream) = tokio::select! {
                () = shutdown.cancelled() => return,
                classified = classify(stream) => classified,
            };
            let connection = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(header_read_timeout)
                .half_close(half_close)
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades();
            let mut connection = std::pin::pin!(connection);
            let result = tokio::select! {
                result = connection.as_mut() => result,
                () = shutdown.cancelled() => {
                    connection.as_mut().graceful_shutdown();
                    connection.await
                }
            };
            if let Err(serve_error) = result {
                debug!(err = %serve_error, "HTTP connection ended with an error");
            }
        });
    }
    // Refuse new connections while the live ones finish.
    drop(listener);
    let all = u32::try_from(max_connections).unwrap_or(u32::MAX);
    if tokio::time::timeout(options.drain_timeout, permits.acquire_many(all))
        .await
        .is_err()
    {
        warn!(
            open = max_connections.saturating_sub(permits.available_permits()),
            "HTTP drain timed out; closing the remaining connections"
        );
    }
}

/// The route prefix whose connections get half-close.
const HLS_UPLOAD_PREFIX: &[u8] = b"/ingest/hls/";
/// Enough of a request line for a method and the prefix.
const SNIFF_BYTES: usize = 64;
/// How long a connection may take to send that much; past it, the connection
/// is served with the default policy and hyper's own timeouts apply.
const SNIFF_TIMEOUT: Duration = Duration::from_secs(5);
/// Read the start of the connection's first request line, until it settles
/// whether that request targets HLS push ingest (or `SNIFF_TIMEOUT`, EOF or
/// a read error ends the wait: default policy). The bytes read are not lost:
/// the returned stream yields them first.
async fn classify(mut stream: TcpStream) -> (bool, Prefixed) {
    let mut prefix = [0u8; SNIFF_BYTES];
    let mut filled = 0usize;
    let deadline = tokio::time::Instant::now() + SNIFF_TIMEOUT;
    let mut upload = false;
    while let Some(free) = prefix.get_mut(filled..).filter(|free| !free.is_empty()) {
        let read = match tokio::time::timeout_at(deadline, stream.read(free)).await {
            Ok(Ok(read)) if read > 0 => read,
            _ => break,
        };
        filled = filled.saturating_add(read).min(SNIFF_BYTES);
        if let Some(decided) = targets_hls_upload(prefix.get(..filled).unwrap_or_default()) {
            upload = decided;
            break;
        }
    }
    let stream = Prefixed {
        prefix,
        start: 0,
        end: filled,
        stream,
    };
    (upload, stream)
}

/// A connection whose first bytes were read to classify it: reads yield
/// those bytes, then the socket's; writes go to the socket.
struct Prefixed {
    prefix: [u8; SNIFF_BYTES],
    start: usize,
    end: usize,
    stream: TcpStream,
}

impl AsyncRead for Prefixed {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let pending = this.prefix.get(this.start..this.end).unwrap_or_default();
        if pending.is_empty() {
            return Pin::new(&mut this.stream).poll_read(cx, buf);
        }
        let take = pending.len().min(buf.remaining());
        buf.put_slice(pending.get(..take).unwrap_or_default());
        this.start = this.start.saturating_add(take);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Prefixed {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

/// Decide from the start of a request line (`METHOD SP path …`): `Some` once
/// the bytes settle it, `None` while more are needed.
fn targets_hls_upload(seen: &[u8]) -> Option<bool> {
    let Some(space) = seen.iter().position(|byte| *byte == b' ') else {
        // No method is this long; it is not a request line this needs.
        return (seen.len() >= 16).then_some(false);
    };
    let path = seen.get(space.saturating_add(1)..).unwrap_or_default();
    let compared = path.len().min(HLS_UPLOAD_PREFIX.len());
    if path.get(..compared) != HLS_UPLOAD_PREFIX.get(..compared) {
        return Some(false);
    }
    (compared == HLS_UPLOAD_PREFIX.len()).then_some(true)
}

#[cfg(test)]
mod tests;
