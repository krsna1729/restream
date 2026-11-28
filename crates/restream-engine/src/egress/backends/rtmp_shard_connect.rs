//! An RTMP output's connect path: its destination lookup on the process
//! resolver (`crate::resolver`, deadline `LeafPolicy::resolve_timeout`),
//! the TCP connect, then a live leaf; and the production backend
//! constructor that wires the resolver in. Every lookup is answered
//! (addresses, failure, timeout or a full resolver queue) into the backend's
//! completion queue, drained on its media tick; a failure fails that output
//! alone and its egress task retries it. The shard owns no resolver thread,
//! so its teardown never waits on DNS.

use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use super::super::compio_tcp::TcpConnectAttempt;
use super::super::tcp::{TcpReadyLeaf, connect_error};
use super::*;

pub(super) struct PendingRtmpConnect {
    pub(super) common: LeafCommon,
    pub(super) parts: restream_rtmp::RtmpUrlParts,
    pub(super) connect_timeout: Duration,
}

pub(super) struct ConnectingRtmpConnect {
    pub(super) common: LeafCommon,
    pub(super) parts: restream_rtmp::RtmpUrlParts,
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
            spec.progress.mark_terminated_unexpectedly();
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
                >= self.leaves.capacity()
        {
            tracing::warn!(
                output_id = %output_id,
                "rtmp fabric leaf rejected: shard leaf capacity exhausted"
            );
            spec.progress.mark_terminated_unexpectedly();
            return;
        }
        let connect_timeout = spec.policy.connect_timeout;
        let resolve_deadline = Instant::now() + spec.policy.resolve_timeout;
        let common = LeafCommon::new(spec.id, spec.generation, spec.feed, spec.policy)
            .with_progress_sink(spec.progress.clone());
        self.resolve_destination(&output_id, common.generation, &parts, resolve_deadline);
        self.pending_connects.insert(
            output_id,
            PendingRtmpConnect {
                common,
                parts,
                connect_timeout,
            },
        );
    }

    /// The answer is drained on a later media tick, after the pending
    /// connect it completes is in place. Never blocks (`Resolver::submit`).
    fn resolve_destination(
        &self,
        output_id: &OutputId,
        generation: u64,
        parts: &restream_rtmp::RtmpUrlParts,
        deadline: Instant,
    ) {
        let Some(resolver) = &self.resolver else {
            return;
        };
        let output_id = output_id.clone();
        let completions = self.resolved_sender.clone();
        resolver.submit(&parts.host, parts.port, deadline, move |resolution| {
            // The receiver goes only with the shard, when no answer matters.
            let _ = completions.send(RtmpResolvedConnect {
                output_id,
                generation,
                peer: resolution.and_then(|addresses| {
                    addresses.first().copied().ok_or(ResolveError::NoAddress)
                }),
            });
        });
    }

    #[cfg(test)]
    pub(super) fn pending_connect(&self, output_id: &OutputId) -> Option<&PendingRtmpConnect> {
        self.pending_connects.get(output_id)
    }

    #[cfg(test)]
    pub(crate) fn has_pending_connect(&self, output_id: &OutputId) -> bool {
        self.pending_connects.contains_key(output_id)
    }

    pub(super) fn fail_pending_connect(
        &mut self,
        output_id: &OutputId,
        generation: u64,
        error: &crate::resolver::ResolveError,
    ) {
        let Some(pending) = self.pending_connects.remove(output_id) else {
            return;
        };
        if pending.common.generation != generation {
            self.pending_connects.insert(output_id.clone(), pending);
            return;
        }
        tracing::warn!(output_id = %output_id, %error, "rtmp fabric leaf rejected: peer resolution failed");
        pending.common.mark_terminated_unexpectedly();
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

        let Some(key) = self.leaves.reserve() else {
            pending.common.mark_terminated_unexpectedly();
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
                pending.common.mark_terminated_unexpectedly();
                self.leaves.release_reserved(key);
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
        self.try_activate(output_id, connecting, key).is_ok()
    }

    fn try_activate(
        &mut self,
        output_id: &OutputId,
        connecting: ConnectingRtmpConnect,
        key: LeafKey,
    ) -> Result<(), ()> {
        let startup_timeout = connecting.startup_timeout;
        let generation = connecting.common.generation;
        let fd = connecting.stream.as_raw_fd();
        let stream = self.secure_stream(
            output_id,
            connecting.stream,
            &connecting.parts,
            fd,
            key,
            &connecting.common,
        )?;
        let publish_startup = self.take_startup(output_id, fd, key, &connecting.common)?;
        let engine = self.build_engine(
            connecting.parts,
            publish_startup,
            output_id,
            fd,
            key,
            &connecting.common,
        )?;
        let leaf = RtmpFabricLeaf {
            common: connecting.common,
            engine,
            transport: stream,
            pending_readiness: super::Readiness::default(),
            pending_application_bytes: 0,
            observed_since: Instant::now(),
            startup_deadline: Instant::now() + startup_timeout,
            draining_since: None,
            draining_reason: None,
            previous_tcp_bytes: None,
            delivery: Default::default(),
        };
        self.install_leaf(output_id, key, fd, generation, leaf)
    }

    fn secure_stream(
        &mut self,
        output_id: &OutputId,
        stream: super::super::compio_tcp::CompioTcpStream,
        parts: &restream_rtmp::RtmpUrlParts,
        fd: std::os::fd::RawFd,
        key: LeafKey,
        common: &LeafCommon,
    ) -> Result<TlsTcpConnection, ()> {
        if !parts.tls {
            return Ok(TlsTcpConnection::plain(stream));
        }
        match TlsTcpConnection::tls_with_config(
            stream,
            &parts.host,
            self.rtmps_client_config.clone(),
            &super::super::rtmp::RTMPS_TLS_COUNTERS,
        ) {
            Ok(stream) => Ok(stream),
            Err(error) => {
                tracing::warn!(output_id = %output_id, error = %error, "rtmp fabric leaf tls init failed");
                self.abandon_connect(fd, key, common)
            }
        }
    }

    fn take_startup(
        &mut self,
        output_id: &OutputId,
        fd: std::os::fd::RawFd,
        key: LeafKey,
        common: &LeafCommon,
    ) -> Result<RtmpPublishStartup, ()> {
        let Some(publish_startup) = self.startup_source.take_startup(output_id) else {
            tracing::warn!(output_id = %output_id, "rtmp fabric leaf rejected: no publish startup available");
            return self.abandon_connect(fd, key, common);
        };
        Ok(publish_startup)
    }

    fn build_engine(
        &mut self,
        parts: restream_rtmp::RtmpUrlParts,
        publish_startup: RtmpPublishStartup,
        output_id: &OutputId,
        fd: std::os::fd::RawFd,
        key: LeafKey,
        common: &LeafCommon,
    ) -> Result<RtmpFabricEngine, ()> {
        match RtmpFabricEngine::new_client(
            parts,
            self.chunk_size,
            publish_startup,
            self.payload_cache.clone(),
        ) {
            Ok(engine) => Ok(engine),
            Err(error) => {
                tracing::warn!(output_id = %output_id, error = %error, "rtmp fabric leaf init failed");
                self.abandon_connect(fd, key, common)
            }
        }
    }

    fn install_leaf(
        &mut self,
        output_id: &OutputId,
        key: LeafKey,
        fd: std::os::fd::RawFd,
        generation: u64,
        leaf: RtmpFabricLeaf,
    ) -> Result<(), ()> {
        self.register_leaf_connection(output_id, key, fd, generation, &leaf)?;
        self.fill_leaf(key, leaf, output_id, fd)?;
        self.record_leaf_socket(output_id, key, fd);
        self.enqueue_first_write(output_id, key, fd, generation)?;
        tracing::info!(output_id = %output_id, leaf_key = ?key, "rtmp fabric leaf connected");
        Ok(())
    }

    fn register_leaf_connection(
        &mut self,
        output_id: &OutputId,
        key: LeafKey,
        fd: std::os::fd::RawFd,
        generation: u64,
        leaf: &RtmpFabricLeaf,
    ) -> Result<(), ()> {
        if self
            .poller
            .register_connection(fd, key, generation, leaf.transport.completion_stream())
            .is_err()
        {
            tracing::warn!(output_id = %output_id, "rtmp fabric leaf poller registration failed");
            return self.abandon_connect(fd, key, &leaf.common);
        }
        Ok(())
    }

    fn fill_leaf(
        &mut self,
        key: LeafKey,
        leaf: RtmpFabricLeaf,
        output_id: &OutputId,
        fd: std::os::fd::RawFd,
    ) -> Result<(), ()> {
        if let Err(mut leaf) = self.leaves.fill(key, leaf) {
            // Unreachable while `key` is this connect's reservation; never
            // drop a live connection without closing it.
            tracing::warn!(output_id = %output_id, "rtmp fabric leaf slot was not reserved");
            let _ = self.poller.remove(fd);
            leaf.engine.close(&mut leaf.transport, CloseReason::Removed);
            leaf.common.mark_terminated_unexpectedly();
            return Err(());
        }
        Ok(())
    }

    fn record_leaf_socket(&mut self, output_id: &OutputId, key: LeafKey, fd: std::os::fd::RawFd) {
        self.enqueue_stall_candidate(key);
        self.sweep_service.invalidate();
        self.service = Default::default();
        if let Some(previous) = self
            .output_sockets
            .insert(output_id.clone(), RtmpLeafSocket { key, fd })
        {
            self.remove_leaf_socket(previous, CloseReason::Removed);
        }
    }

    fn enqueue_first_write(
        &mut self,
        output_id: &OutputId,
        key: LeafKey,
        fd: std::os::fd::RawFd,
        generation: u64,
    ) -> Result<(), ()> {
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
            self.leaves
                .get(key)
                .expect("the leaf was filled into this key before its first write")
                .common
                .mark_terminated_unexpectedly();
            self.remove_leaf_by_output(output_id);
            return Err(());
        }
        Ok(())
    }

    fn abandon_connect<T>(
        &mut self,
        fd: std::os::fd::RawFd,
        key: LeafKey,
        common: &LeafCommon,
    ) -> Result<T, ()> {
        let _ = self.poller.remove(fd);
        common.mark_terminated_unexpectedly();
        self.leaves.release_reserved(key);
        Err(())
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
            self.leaves.release_reserved(key);
        }
    }

    pub(super) fn finish_connecting(&mut self, event: TcpReadyLeaf) -> bool {
        let Some(connecting) = self.connecting.remove(&event.key) else {
            return false;
        };
        let output_id = connecting.common.output_id.clone();
        if self.connecting_by_output.get(&output_id) != Some(&event.key) {
            let _ = self.poller.remove(connecting.stream.as_raw_fd());
            self.leaves.release_reserved(event.key);
            return false;
        }
        self.connecting_by_output.remove(&output_id);
        if let Err(error) = connect_error(connecting.stream.as_raw_fd()) {
            tracing::warn!(output_id = %output_id, error = %error, "rtmp fabric leaf connect failed");
            connecting.common.mark_terminated_unexpectedly();
            let _ = self.poller.remove(connecting.stream.as_raw_fd());
            self.leaves.release_reserved(event.key);
            return false;
        }
        self.activate_connected(&output_id, connecting, event.key)
    }
}
