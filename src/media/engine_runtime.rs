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

    pub fn bonding_available(&self) -> bool {
        self.runtime
            .listener_stats
            .bonding_available
            .load(Ordering::Relaxed)
    }

    /// Register an OS thread JoinHandle so it can be joined at shutdown.
    /// Already-finished handles are pruned opportunistically.
    pub fn register_os_thread(&self, handle: std::thread::JoinHandle<()>) {
        let mut guards = restream_sync::lock::lock(&self.runtime.os_threads);
        guards.retain(|thread| !thread.is_finished());
        guards.push(handle);
    }

    pub fn register_listener_shutdown(&self, shutdown: impl Fn() + Send + Sync + 'static) {
        restream_sync::lock::lock(&self.runtime.listener_shutdowns).push(Box::new(shutdown));
    }

    pub fn shutdown_listeners(&self) {
        let shutdowns: Vec<_> = restream_sync::lock::lock(&self.runtime.listener_shutdowns)
            .drain(..)
            .collect();
        for shutdown in shutdowns {
            shutdown();
        }
    }

    /// Drain all registered OS thread handles for joining at shutdown.
    pub fn drain_os_thread_handles(&self) -> Vec<std::thread::JoinHandle<()>> {
        restream_sync::lock::lock(&self.runtime.os_threads)
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
        self.file_ingests.positions.write().await.remove(id);
    }

    /// Records which item of its playlist a file ingest is playing.
    pub async fn set_file_ingest_position(&self, id: &str, index: usize) {
        self.file_ingests
            .positions
            .write()
            .await
            .insert(id.to_string(), index);
    }

    /// The item a running file ingest is playing.
    pub async fn file_ingest_position(&self, id: &str) -> Option<usize> {
        self.file_ingests.positions.read().await.get(id).copied()
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

    /// Registers a pipeline's stream checks, or returns `None` if they run
    /// already; check-and-insert under one lock, as for recordings.
    pub fn register_stream_qa(
        &self,
        pipeline_id: &str,
    ) -> Option<(
        CancellationToken,
        Arc<std::sync::Mutex<crate::media::stream_qa::StreamQa>>,
    )> {
        let mut entries = restream_sync::lock::lock(&self.stream_qa.entries);
        if entries
            .get(pipeline_id)
            .is_some_and(|entry| !entry.cancel.is_cancelled())
        {
            return None;
        }
        let entry = crate::media::engine_registries::StreamQaEntry {
            cancel: CancellationToken::new(),
            findings: Arc::default(),
        };
        let handles = (entry.cancel.clone(), entry.findings.clone());
        entries.insert(pipeline_id.to_string(), entry);
        Some(handles)
    }

    /// Stops a pipeline's stream checks.
    pub fn unregister_stream_qa(&self, pipeline_id: &str) {
        if let Some(entry) = restream_sync::lock::lock(&self.stream_qa.entries).remove(pipeline_id)
        {
            entry.cancel.cancel();
        }
    }

    /// Drops the entry a finished sampler left, unless a newer one replaced
    /// it (a stop and a start raced the thread's exit).
    pub fn release_stream_qa(
        &self,
        pipeline_id: &str,
        findings: &Arc<std::sync::Mutex<crate::media::stream_qa::StreamQa>>,
    ) {
        let mut entries = restream_sync::lock::lock(&self.stream_qa.entries);
        if entries
            .get(pipeline_id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.findings, findings))
        {
            entries.remove(pipeline_id);
        }
    }

    pub fn is_stream_qa_active(&self, pipeline_id: &str) -> bool {
        restream_sync::lock::lock(&self.stream_qa.entries)
            .get(pipeline_id)
            .is_some_and(|entry| !entry.cancel.is_cancelled())
    }

    /// The latest findings of a pipeline's running checks.
    pub fn stream_qa(&self, pipeline_id: &str) -> Option<crate::media::stream_qa::StreamQa> {
        let entries = restream_sync::lock::lock(&self.stream_qa.entries);
        let entry = entries.get(pipeline_id)?;
        Some(restream_sync::lock::lock(&entry.findings).clone())
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
        for entry in restream_sync::lock::lock(&self.stream_qa.entries).values() {
            entry.cancel.cancel();
        }
        self.shutdown_all_srt_fabric_runtimes().await;
        self.shutdown_all_rtmp_fabric_runtimes().await;
        self.shutdown_all_sink_fabric_runtimes().await;
        self.shutdown_all_pipeline_fabric_runtimes().await;
        self.shutdown_all_hls_put_fabric_runtimes().await;
        self.shutdown_all_moq_fabric_runtimes().await;
    }
}
