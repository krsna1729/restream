use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use restream_engine::engine::MediaEngine;

use super::runtime::{HttpStop, ListenerHandles};

pub(super) fn spawn_signal_watcher() -> CancellationToken {
    let shutdown = CancellationToken::new();
    let shutdown_task = shutdown.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut sigterm) => {
                    tokio::select! {
                        res = tokio::signal::ctrl_c() => {
                            if let Err(error) = res {
                                warn!(err = %error, "Ctrl+C error");
                            }
                        }
                        _ = sigterm.recv() => {}
                    }
                }
                // Without a SIGTERM handler, still stop on Ctrl+C rather
                // than leaving the process stoppable only by SIGKILL.
                Err(error) => {
                    warn!(err = %error, "failed to install the SIGTERM handler; stopping on Ctrl+C only");
                    if let Err(error) = tokio::signal::ctrl_c().await {
                        warn!(err = %error, "Ctrl+C error");
                    }
                }
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
    listeners: ListenerHandles,
    http_stop: HttpStop,
) {
    let ListenerHandles {
        http: http_handle,
        rtmp: rtmp_handle,
        srt: srt_handle,
    } = listeners;
    info!(
        event_class = "lifecycle",
        event_type = "restream.shutdown.started",
        "shutdown: cancelling all active tasks",
    );
    // The dashboard first, while the engine and database its handlers use
    // are still up: stop accepting, finish in-flight responses. `serve`
    // bounds its own drain; the extra second only guards that bound.
    http_stop.begin();
    let http_deadline = crate::api::http_server::DRAIN_TIMEOUT + Duration::from_secs(1);
    if let Some(mut http_handle) = http_handle
        && tokio::time::timeout(http_deadline, &mut http_handle)
            .await
            .is_err()
    {
        http_handle.abort();
    }

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

    if !rtmp_handle.is_finished() {
        rtmp_handle.abort();
    }
    let _ = rtmp_handle.await;
    if !srt_handle.is_finished() {
        let _ = srt_handle.await;
    }

    // The listener threads (RTMP, SRT, MoQ) are joined and their tasks have
    // ended, so no session is left to drop a lease: stop the release task
    // and end any attempt still queued.
    engine.stop_ingest_release_drain().await;

    info!(
        event_class = "lifecycle",
        event_type = "restream.shutdown.completed",
        "shutdown complete",
    );
}
