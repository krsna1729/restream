//! The dashboard HTTP listener: the axum router on hyper HTTP/1.1, with
//! half-closed requests allowed.
//!
//! HLS push encoders (FFmpeg without `-http_persistent 1`) close their side
//! right after the request body and never read the response. hyper's default
//! treats that EOF as an abort and drops the request before its body is read,
//! so `axum::serve` would lose every such upload.
//!
//! The cost: with half-close a client EOF is not a disconnect, so a client
//! that leaves a streaming response (SSE) is noticed only when a write to
//! it fails, at the second write after its reset (20-40 s for the state
//! stream's 20 s heartbeat), not when it goes. Q-072 moves HLS ingest to a
//! listener of its own so only uploads pay this.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tower::{Service, ServiceExt};
use tracing::{error, trace};

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
            let connection = hyper::server::conn::http1::Builder::new()
                .half_close(true)
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades();
            if let Err(serve_error) = connection.await {
                trace!(err = %serve_error, "HTTP connection ended with an error");
            }
        });
    }
}
