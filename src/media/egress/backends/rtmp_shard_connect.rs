use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use super::super::tcp::{TcpConnectAttempt, TcpReadyLeaf, connect_error};
use super::*;

pub(super) struct PendingRtmpConnect {
    pub(super) common: LeafCommon,
    pub(super) parts: crate::media::rtmp::RtmpUrlParts,
    pub(super) connect_timeout: Duration,
}

pub(super) struct ConnectingRtmpConnect {
    pub(super) common: LeafCommon,
    pub(super) parts: crate::media::rtmp::RtmpUrlParts,
    pub(super) stream: super::super::compio_tcp::CompioTcpStream,
    pub(super) deadline: Instant,
    /// Bound on handshake + negotiation once connected (the policy's
    /// connect timeout).
    pub(super) startup_timeout: Duration,
}

impl<P, S> RtmpShardBackend<P, S>
where
    P: RtmpReadinessPoller,
    S: RtmpPublishStartupSource,
{
    pub(super) fn queue_pending_rtmp_connect(&mut self, spec: OutputSpec, target_url: &str) {
        let Some(parts) = parse_rtmp_url(target_url) else {
            tracing::warn!(output_id = %spec.id, "rtmp fabric leaf rejected: invalid url");
            return;
        };
        let output_id = spec.id.clone();
        self.remove_connecting_output(&output_id);
        let already_admitted = self.output_sockets.contains_key(&output_id)
            || self.pending_connects.contains_key(&output_id)
            || self.connecting_by_output.contains_key(&output_id);
        if !already_admitted
            && self
                .output_sockets
                .len()
                .saturating_add(self.pending_connects.len())
                .saturating_add(self.connecting_by_output.len())
                >= self.leaves.len()
        {
            tracing::warn!(
                output_id = %output_id,
                "rtmp fabric leaf rejected: shard leaf capacity exhausted"
            );
            spec.progress.mark_terminated_unexpectedly();
            return;
        }
        let common = LeafCommon::new(
            spec.id,
            spec.generation,
            spec.feed,
            LeafLimits::from_policy(&spec.policy),
        )
        .with_progress_sink(spec.progress.clone());
        self.pending_connects.insert(
            output_id,
            PendingRtmpConnect {
                common,
                parts,
                connect_timeout: spec.policy.connect_timeout,
            },
        );
    }

    #[cfg(test)]
    pub(super) fn pending_connect(&self, output_id: &OutputId) -> Option<&PendingRtmpConnect> {
        self.pending_connects.get(output_id)
    }

    #[cfg(test)]
    pub(crate) fn has_pending_connect(&self, output_id: &OutputId) -> bool {
        self.pending_connects.contains_key(output_id)
    }

    pub(super) fn fail_pending_connect(&mut self, output_id: &OutputId, generation: u64) {
        let Some(pending) = self.pending_connects.remove(output_id) else {
            return;
        };
        if pending.common.generation != generation {
            self.pending_connects.insert(output_id.clone(), pending);
            return;
        }
        tracing::warn!(output_id = %output_id, "rtmp fabric leaf rejected: peer resolution failed");
        pending.common.progress_sink.mark_terminated_unexpectedly();
    }

    pub(super) fn complete_pending_connect(
        &mut self,
        output_id: &OutputId,
        generation: u64,
        peer_addr: SocketAddr,
    ) -> bool {
        let Some(pending) = self.pending_connects.remove(output_id) else {
            return false;
        };
        if pending.common.generation != generation {
            self.pending_connects.insert(output_id.clone(), pending);
            return false;
        }

        let Some(key) = self.allocate_leaf_key() else {
            pending.common.progress_sink.mark_terminated_unexpectedly();
            tracing::warn!(
                output_id = %output_id,
                "rtmp fabric leaf rejected: shard leaf capacity exhausted"
            );
            return false;
        };
        match self.poller.start_connect(
            peer_addr,
            key,
            pending.common.generation,
            pending.connect_timeout,
        ) {
            Ok(TcpConnectAttempt::Connected(stream)) => self.activate_connected(
                output_id,
                ConnectingRtmpConnect {
                    common: pending.common,
                    parts: pending.parts,
                    stream,
                    deadline: Instant::now(),
                    startup_timeout: pending.connect_timeout,
                },
                key,
            ),
            Ok(TcpConnectAttempt::InProgress(stream)) => {
                self.connecting_by_output.insert(output_id.clone(), key);
                self.connecting.insert(
                    key,
                    ConnectingRtmpConnect {
                        common: pending.common,
                        parts: pending.parts,
                        stream,
                        deadline: Instant::now() + pending.connect_timeout,
                        startup_timeout: pending.connect_timeout,
                    },
                );
                true
            }
            Err(error) => {
                tracing::warn!(
                    output_id = %output_id,
                    error = %error.message,
                    "rtmp fabric leaf connect failed"
                );
                pending.common.progress_sink.mark_terminated_unexpectedly();
                self.free_leaf_keys.push(key);
                false
            }
        }
    }

    fn activate_connected(
        &mut self,
        output_id: &OutputId,
        connecting: ConnectingRtmpConnect,
        key: LeafKey,
    ) -> bool {
        let progress_sink = connecting.common.progress_sink.clone();
        let startup_timeout = connecting.startup_timeout;
        let generation = connecting.common.generation;
        let fd = connecting.stream.as_raw_fd();
        let stream = if connecting.parts.tls {
            match RtmpConnection::tls_with_config(
                connecting.stream,
                &connecting.parts.host,
                self.rtmps_client_config.clone(),
            ) {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::warn!(output_id = %output_id, error = %error, "rtmp fabric leaf tls init failed");
                    let _ = self.poller.remove(fd);
                    progress_sink.mark_terminated_unexpectedly();
                    self.free_leaf_keys.push(key);
                    return false;
                }
            }
        } else {
            RtmpConnection::plain(connecting.stream)
        };
        let Some(publish_startup) = self.startup_source.take_startup(output_id) else {
            tracing::warn!(output_id = %output_id, "rtmp fabric leaf rejected: no publish startup available");
            let _ = self.poller.remove(fd);
            progress_sink.mark_terminated_unexpectedly();
            self.free_leaf_keys.push(key);
            return false;
        };
        let engine = match RtmpFabricEngine::new_client(
            connecting.parts,
            self.chunk_size,
            false,
            publish_startup,
        ) {
            Ok(mut engine) => {
                engine.share_payload_cache(self.payload_cache.clone());
                engine
            }
            Err(error) => {
                tracing::warn!(output_id = %output_id, error = %error, "rtmp fabric leaf init failed");
                let _ = self.poller.remove(fd);
                progress_sink.mark_terminated_unexpectedly();
                self.free_leaf_keys.push(key);
                return false;
            }
        };
        if self
            .poller
            .register_connection(fd, key, generation, stream.completion_stream())
            .is_err()
        {
            tracing::warn!(output_id = %output_id, "rtmp fabric leaf poller registration failed");
            let _ = self.poller.remove(fd);
            progress_sink.mark_terminated_unexpectedly();
            self.free_leaf_keys.push(key);
            return false;
        }
        self.leaves[key.0] = Some(RtmpFabricLeaf {
            common: connecting.common,
            engine,
            transport: stream,
            pending_readiness: super::Readiness::default(),
            observed_since: Instant::now(),
            startup_deadline: Instant::now() + startup_timeout,
            draining_since: None,
            draining_reason: None,
            previous_tcp_bytes: None,
            delivery: Default::default(),
        });
        self.enqueue_stall_candidate(key);
        if let Some(previous) = self
            .output_sockets
            .insert(output_id.clone(), RtmpLeafSocket { key, fd })
        {
            self.remove_leaf_socket(previous, CloseReason::Removed);
        }
        // An established TCP socket is writable, but the TX worker only emits
        // completions after a write. Queue that first write opportunity here.
        if !self.enqueue_ready(TcpReadyLeaf {
            fd,
            key,
            generation,
            readable: false,
            writable: true,
        }) {
            tracing::warn!(output_id = %output_id, "rtmp fabric leaf could not enter the ready queue");
            progress_sink.mark_terminated_unexpectedly();
            self.remove_leaf_by_output(output_id);
            return false;
        }
        tracing::info!(output_id = %output_id, leaf_key = key.0, "rtmp fabric leaf connected");
        true
    }

    pub(super) fn remove_leaf_by_output(&mut self, output_id: &OutputId) -> bool {
        self.pending_connects.remove(output_id);
        self.remove_connecting_output(output_id);
        self.output_sockets
            .remove(output_id)
            .is_some_and(|socket_ref| self.remove_leaf_socket(socket_ref, CloseReason::Removed))
    }

    pub(super) fn remove_connecting_output(&mut self, output_id: &OutputId) {
        let Some(key) = self.connecting_by_output.remove(output_id) else {
            return;
        };
        if let Some(connecting) = self.connecting.remove(&key) {
            let _ = self.poller.remove(connecting.stream.as_raw_fd());
            self.free_leaf_keys.push(key);
        }
    }

    pub(super) fn finish_connecting(&mut self, event: TcpReadyLeaf) -> bool {
        let Some(connecting) = self.connecting.remove(&event.key) else {
            return false;
        };
        let output_id = connecting.common.output_id.clone();
        if self.connecting_by_output.get(&output_id) != Some(&event.key)
            || connecting.common.generation != event.generation
        {
            let _ = self.poller.remove(connecting.stream.as_raw_fd());
            self.free_leaf_keys.push(event.key);
            return false;
        }
        self.connecting_by_output.remove(&output_id);
        if let Err(error) = connect_error(connecting.stream.as_raw_fd()) {
            tracing::warn!(output_id = %output_id, error = %error, "rtmp fabric leaf connect failed");
            connecting
                .common
                .progress_sink
                .mark_terminated_unexpectedly();
            let _ = self.poller.remove(connecting.stream.as_raw_fd());
            self.free_leaf_keys.push(event.key);
            return false;
        }
        self.activate_connected(&output_id, connecting, event.key)
    }
}
