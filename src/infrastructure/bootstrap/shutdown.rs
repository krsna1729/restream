use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::media::engine::MediaEngine;

pub(super) fn spawn_signal_watcher() -> CancellationToken {
    let shutdown = CancellationToken::new();
    let shutdown_task = shutdown.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("Failed to install SIGTERM handler");
            tokio::select! {
                res = tokio::signal::ctrl_c() => {
                    if let Err(error) = res {
                        warn!(err = %error, "Ctrl+C error");
                    }
                }
                _ = sigterm.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            if let Err(error) = tokio::signal::ctrl_c().await {
                warn!(err = %error, "Ctrl+C error");
            }
        }
        info!(
            event_class = "lifecycle",
            event_type = "restream.shutdown.requested",
            "signal received — stopping reconciler",
        );
        shutdown_task.cancel();
    });
    shutdown
}

pub(super) async fn cleanup(
    engine: &Arc<MediaEngine>,
    pool: &SqlitePool,
    http_handle: JoinHandle<()>,
    rtmp_handle: JoinHandle<()>,
    srt_handle: JoinHandle<()>,
) {
    info!(
        event_class = "lifecycle",
        event_type = "restream.shutdown.started",
        "shutdown: cancelling all active tasks",
    );
    engine.cancel_all_active_tasks().await;
    engine.shutdown_all_hls_segmenters().await;
    engine.shutdown_listeners();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let handles = engine.drain_os_thread_handles();
    if !handles.is_empty() {
        tokio::task::spawn_blocking(move || {
            for handle in handles {
                let _ = handle.join();
            }
        })
        .await
        .ok();
    }

    pool.close().await;

    if !http_handle.is_finished() {
        http_handle.abort();
    }
    if !rtmp_handle.is_finished() {
        rtmp_handle.abort();
    }
    if !srt_handle.is_finished() {
        let _ = srt_handle.await;
    }

    info!(
        event_class = "lifecycle",
        event_type = "restream.shutdown.completed",
        "shutdown complete",
    );
}
