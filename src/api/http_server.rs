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
//! line, peeked without consuming it: an encoder opens a connection per
//! upload, and one that keeps a connection open uses it only for uploads.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tower::{Service, ServiceExt};
use tracing::{debug, error};

/// Serve `app` on `listener` until the task is dropped. Accept errors that
/// concern one connection are skipped; others (descriptor exhaustion) are
/// logged and retried after a second, as `axum::serve` does.
pub async fn serve(listener: TcpListener, app: Router) {
    let mut make_service = app.into_make_service_with_connect_info::<SocketAddr>();
    loop {
        let (stream, remote) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(accept_error) => {
                if !matches!(
                    accept_error.kind(),
                    std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::ConnectionReset
                ) {
                    error!(err = %accept_error, "HTTP accept error");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                continue;
            }
        };
        let service = match make_service.call(remote).await {
            Ok(service) => service,
            Err(never) => match never {},
        };
        let service = TowerToHyperService::new(
            service.map_request(|request: Request<Incoming>| request.map(Body::new)),
        );
        tokio::spawn(async move {
            let half_close = first_request_is_hls_upload(&stream).await;
            let connection = hyper::server::conn::http1::Builder::new()
                .half_close(half_close)
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades();
            if let Err(serve_error) = connection.await {
                debug!(err = %serve_error, "HTTP connection ended with an error");
            }
        });
    }
}

/// The route prefix whose connections get half-close.
const HLS_UPLOAD_PREFIX: &[u8] = b"/ingest/hls/";
/// Enough of a request line for a method and the prefix.
const SNIFF_BYTES: usize = 64;
/// How long a connection may take to send that much; past it, the connection
/// is served with the default policy and hyper's own timeouts apply.
const SNIFF_TIMEOUT: Duration = Duration::from_secs(5);
/// Between peeks while the request line is still arriving: `peek` returns at
/// once while any byte is buffered, so it cannot wait for more on its own.
const SNIFF_RETRY: Duration = Duration::from_millis(5);

/// Whether the connection's first request targets HLS push ingest, read from
/// its request line without consuming it.
async fn first_request_is_hls_upload(stream: &TcpStream) -> bool {
    let mut buffer = [0u8; SNIFF_BYTES];
    let sniff = async {
        loop {
            let read = stream.peek(&mut buffer).await.ok()?;
            if read == 0 {
                return None;
            }
            let seen = buffer.get(..read).unwrap_or_default();
            if let Some(decided) = targets_hls_upload(seen) {
                return Some(decided);
            }
            if read == buffer.len() {
                return Some(false);
            }
            tokio::time::sleep(SNIFF_RETRY).await;
        }
    };
    matches!(
        tokio::time::timeout(SNIFF_TIMEOUT, sniff).await,
        Ok(Some(true))
    )
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
#[path = "http_server_tests.rs"]
mod tests;
