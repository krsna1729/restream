//! Process, listener, file-ingest, recording, and shutdown resources.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::media::engine::MediaEngine;
use crate::media::snapshots::ListenerSocketStats;

impl MediaEngine {
    pub fn listener_stats_handle(&self) -> Arc<ListenerSocketStats> {
        self.runtime.listener_stats.clone()
    }

    pub fn sender_semaphore_handle(&self) -> Arc<tokio::sync::Semaphore> {
        self.runtime.sender_semaphore.clone()
    }

    /// Engine-wide per-shard libsrt egress multiplexer port registry. Each
    /// egress-fabric shard resolves its own entry, so shards do not share a
    /// libsrt sender thread — see
    /// `crate::media::egress::backends::srt::muxer_ports`.
    pub(crate) fn srt_egress_muxer_ports_handle(
        &self,
    ) -> crate::media::egress::backends::srt::muxer_ports::SrtEgressMuxerPorts {
        self.runtime.srt_egress_muxer_ports.clone()
    }

    pub fn bonding_available(&self) -> bool {
        self.runtime
            .listener_stats
            .bonding_available
            .load(Ordering::Relaxed)
    }

    /// Register an OS thread JoinHandle so it can be joined at shutdown.
    /// Already-finished handles are pruned opportunistically.
    pub fn register_os_thread(&self, handle: std::thread::JoinHandle<()>) {
        let mut guards = self
            .runtime
            .os_threads
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        guards.retain(|thread| !thread.is_finished());
        guards.push(handle);
    }

    pub fn register_listener_shutdown(&self, shutdown: impl Fn() + Send + Sync + 'static) {
        self.runtime
            .listener_shutdowns
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(Box::new(shutdown));
    }

    pub fn shutdown_listeners(&self) {
        let shutdowns: Vec<_> = self
            .runtime
            .listener_shutdowns
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain(..)
            .collect();
        for shutdown in shutdowns {
            shutdown();
        }
    }

    /// Drain all registered OS thread handles for joining at shutdown.
    pub fn drain_os_thread_handles(&self) -> Vec<std::thread::JoinHandle<()>> {
        self.runtime
            .os_threads
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain(..)
            .collect()
    }

    pub async fn get_or_create_diag_semaphore(
        &self,
        pipeline_id: &str,
    ) -> Arc<tokio::sync::Semaphore> {
        let mut map = self.runtime.diag_semaphores.write().await;
        map.entry(pipeline_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone()
    }

    pub async fn stop_file_ingest_child(&self, ingest_id: &str) -> bool {
        let mut children = self.file_ingests.children.write().await;
        let Some(mut child) = children.remove(ingest_id) else {
            return false;
        };
        drop(children);
        let _ = child.kill().await;
        let _ = child.wait().await;
        true
    }

    pub async fn take_file_ingest_child(&self, ingest_id: &str) -> Option<tokio::process::Child> {
        self.file_ingests.children.write().await.remove(ingest_id)
    }

    pub async fn is_file_ingest_running(&self, id: &str) -> bool {
        let mut children = self.file_ingests.children.write().await;
        if let Some(child) = children.get_mut(id) {
            match child.try_wait() {
                Ok(None) => {
                    self.file_ingests
                        .active
                        .write()
                        .await
                        .insert(id.to_string());
                    true
                }
                _ => {
                    children.remove(id);
                    self.file_ingests.active.write().await.remove(id);
                    false
                }
            }
        } else {
            self.file_ingests.active.read().await.contains(id)
        }
    }

    pub async fn reap_file_ingests(&self) {
        let mut children = self.file_ingests.children.write().await;
        let mut stopped = Vec::new();
        children.retain(|id, child| match child.try_wait() {
            Ok(None) => true,
            _ => {
                info!("File ingest child process {} has exited/stopped", id);
                stopped.push(id.clone());
                false
            }
        });
        drop(children);

        if !stopped.is_empty() {
            let mut active = self.file_ingests.active.write().await;
            for id in stopped {
                active.remove(&id);
            }
        }
    }

    pub async fn mark_file_ingest_running(&self, id: &str) {
        self.file_ingests
            .active
            .write()
            .await
            .insert(id.to_string());
    }

    pub async fn clear_file_ingest_running(&self, id: &str) {
        self.file_ingests.active.write().await.remove(id);
    }

    /// Registers a recording, or returns `None` if already active; check-and-insert under one lock closes the start/start race.
    pub async fn register_recording(&self, pipeline_id: &str) -> Option<CancellationToken> {
        let mut tokens = self.recordings.cancel_tokens.write().await;
        if tokens.get(pipeline_id).is_some_and(|t| !t.is_cancelled()) {
            return None;
        }
        let token = CancellationToken::new();
        tokens.insert(pipeline_id.to_string(), token.clone());
        Some(token)
    }

    /// Unregister and cancel an active recording for a pipeline.
    pub async fn unregister_recording(&self, pipeline_id: &str) {
        let mut tokens = self.recordings.cancel_tokens.write().await;
        if let Some(token) = tokens.remove(pipeline_id) {
            token.cancel();
        }
    }

    pub async fn is_recording_active(&self, pipeline_id: &str) -> bool {
        let tokens = self.recordings.cancel_tokens.read().await;
        tokens
            .get(pipeline_id)
            .is_some_and(|token| !token.is_cancelled())
    }

    pub async fn cancel_all_active_tasks(&self) {
        {
            let egresses = self.egresses.cancel_tokens.read().await;
            for token in egresses.values() {
                token.cancel();
            }
        }
        {
            let ingests = self.ingests.cancel_tokens.read().await;
            for token in ingests.values() {
                token.cancel();
            }
        }
        {
            let recordings = self.recordings.cancel_tokens.read().await;
            for token in recordings.values() {
                token.cancel();
            }
        }
        self.shutdown_all_srt_fabric_runtimes().await;
        self.shutdown_all_rtmp_fabric_runtimes().await;
        self.shutdown_all_sink_fabric_runtimes().await;
        self.shutdown_all_pipeline_fabric_runtimes().await;
    }
}
