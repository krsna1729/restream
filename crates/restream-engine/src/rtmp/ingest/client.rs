//! One RTMP client connection on its ingress owner: handshake, server
//! session, the read loop, and the finish report to the control session.

use super::*;

/// How a connection ended: (phase, reason, had_error).
type Disconnect = (&'static str, &'static str, bool);

const SHUTTING_DOWN: Disconnect = ("shutdown", "RTMP listener shutting down", false);
const DISCONNECTED: Disconnect = ("disconnect", "publisher disconnected", false);

pub(in crate::rtmp) async fn handle_rtmp_client(
    stream: compio::net::TcpStream,
    client_addr: SocketAddr,
    commands: mpsc::Sender<RtmpControlCommand>,
    shutdown: CancellationToken,
    engine: Arc<MediaEngine>,
    parser_budget: parser_budget::ParserBudget,
) -> Result<(), &'static str> {
    let mut socket = RtmpClientSocket::new(stream, shutdown.clone());
    set_tcp_socket_buffers(socket.as_fd(), engine.config.rtmp_preauth_buffer_bytes);
    let Some(remaining) = server_handshake(&mut socket, &engine, &shutdown).await? else {
        return Ok(());
    };
    #[cfg(test)]
    injected_panics::unwind_if_marked(client_addr);

    let session = open_session(&mut socket, &engine).await?;
    let mut client = RtmpClient {
        session,
        socket,
        parser_charge: parser_budget.charge(),
        publisher: None,
        publishing: false,
        commands: &commands,
        client_ip: client_addr.ip().to_string(),
        client_addr: client_addr.to_string(),
        engine: &engine,
        shutdown: &shutdown,
    };
    let disconnect = client.run(&remaining).await;
    // The client, and so its publisher and socket, live until the control
    // session has handled the finish.
    finish(&commands, &shutdown, disconnect).await
}

/// The server handshake, bounded by the handshake timeout; `None` when the
/// listener shuts down first. Returns the bytes received past it.
async fn server_handshake(
    socket: &mut RtmpClientSocket,
    engine: &MediaEngine,
    shutdown: &CancellationToken,
) -> Result<Option<Vec<u8>>, &'static str> {
    let handshake = tokio::select! {
        result = compio::time::timeout(
            Duration::from_millis(engine.config.rtmp_handshake_timeout_ms),
            perform_server_handshake(socket, vec![0; 4096]),
        ) => result,
        _ = shutdown.cancelled() => return Ok(None),
    };
    let (remaining, _handshake_buffer) = handshake.map_err(|_| "RTMP handshake timed out")??;
    Ok(Some(remaining))
}

/// A server session bounded by the configured message size, with its
/// initial responses written.
async fn open_session(
    socket: &mut RtmpClientSocket,
    engine: &MediaEngine,
) -> Result<ServerSession, &'static str> {
    let mut session_config = ServerSessionConfig::new();
    session_config.max_message_length =
        u32::try_from(engine.config.rtmp_max_message_bytes).unwrap_or(u32::MAX);
    let (session, initial_results) =
        ServerSession::new(session_config).map_err(|_| "Failed to initialize server session")?;
    for result in initial_results {
        if let ServerSessionResult::OutboundResponse(packet) = result {
            socket
                .write_all(packet.bytes)
                .await
                .map_err(|_| "Failed to write initial response")?;
        }
    }
    Ok(session)
}

/// Reports the disconnect to the control session and waits for it to
/// finish, unless the listener is shutting down.
async fn finish(
    commands: &mpsc::Sender<RtmpControlCommand>,
    shutdown: &CancellationToken,
    (phase, reason, had_error): Disconnect,
) -> Result<(), &'static str> {
    if !shutdown.is_cancelled() {
        let (finish_tx, finish_rx) = oneshot::channel();
        if send_control_command(
            commands,
            shutdown,
            RtmpControlCommand::Finish {
                phase: phase.to_string(),
                reason: reason.to_string(),
                had_error,
                reply: finish_tx,
            },
        )
        .await
        .is_ok()
        {
            tokio::select! {
                _ = shutdown.cancelled() => {}
                _ = finish_rx => {}
            }
        }
    }
    if had_error { Err(reason) } else { Ok(()) }
}

/// Socket read state carried across the read loop: the quality sampling
/// tick and the previous TCP byte count.
struct ReadState {
    previous_tcp_bytes: Option<(u64, Instant)>,
    stats_tick: Pin<Box<dyn Future<Output = ()>>>,
    fd: RawFd,
}

pub(super) struct RtmpClient<'a> {
    pub(super) session: ServerSession,
    pub(super) socket: RtmpClientSocket,
    parser_charge: parser_budget::ParserCharge,
    /// Set when the publish is authorized: this connection's media, run here.
    pub(super) publisher: Option<Box<RtmpPublisherMedia>>,
    publishing: bool,
    pub(super) commands: &'a mpsc::Sender<RtmpControlCommand>,
    pub(super) client_ip: String,
    pub(super) client_addr: String,
    pub(super) engine: &'a MediaEngine,
    pub(super) shutdown: &'a CancellationToken,
}

impl RtmpClient<'_> {
    /// Handles the bytes received with the handshake, then reads until the
    /// connection ends.
    async fn run(&mut self, remaining: &[u8]) -> Disconnect {
        if !remaining.is_empty() {
            let results = match self.session.handle_input(remaining) {
                Ok(results) => results,
                Err(error) => return ("session", session_error_reason(&error), true),
            };
            if let Some(disconnect) = self.apply(results).await {
                return disconnect;
            }
        }

        let mut read_state = ReadState {
            previous_tcp_bytes: None,
            stats_tick: Box::pin(compio::time::sleep(Duration::from_secs(2))),
            fd: self.socket.raw_fd(),
        };
        // Until it publishes, the client holds a slot without having
        // authenticated; it must get there by this deadline.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "u64::MAX ms fits a monotonic Instant"
        )]
        let admission_deadline =
            Instant::now() + Duration::from_millis(self.engine.config.rtmp_preauth_timeout_ms);
        loop {
            let results = match self.read_results(&mut read_state, admission_deadline).await {
                Ok(results) => results,
                Err(disconnect) => return disconnect,
            };
            if let Some(disconnect) = self.apply(results).await {
                return disconnect;
            }
        }
    }

    /// One socket read, parsed by the session. Before the publish is
    /// authorized the read is bounded by the admission deadline.
    async fn read_results(
        &mut self,
        read_state: &mut ReadState,
        admission_deadline: Instant,
    ) -> Result<Vec<ServerSessionResult>, Disconnect> {
        // Read straight into the session's own input buffer: the only copy of
        // a received byte before chunk reassembly is the kernel's.
        let input = self.session.take_input_buffer();
        let read = read_rtmp_input_or_quality(
            &mut self.socket,
            input,
            self.shutdown,
            &mut read_state.stats_tick,
            self.publishing,
            read_state.fd,
            &mut read_state.previous_tcp_bytes,
            self.commands,
        );
        let read = if self.publishing {
            read.await
        } else {
            // Dropping the read on expiry is safe: the connection closes.
            let remaining = admission_deadline.saturating_duration_since(Instant::now());
            compio::time::timeout(remaining, read)
                .await
                .map_err(|_| ("admission", PREAUTH_DEADLINE_PASSED, true))?
        };
        let Some(read_result) = read else {
            return Err(SHUTTING_DOWN);
        };
        let Ok((count, input)) = read_result else {
            warn_read_error(&self.client_addr);
            return Err(("io", "Read error in main loop", true));
        };
        if count == 0 {
            return Err(DISCONNECTED);
        }
        self.session
            .handle_buffered_input(input, count)
            .map_err(|error| {
                warn_parse_error(&error, &self.client_addr);
                ("session", session_error_reason(&error), true)
            })
    }

    /// Charges the parsed results to the parser budget and handles them;
    /// `Some` ends the connection.
    async fn apply(&mut self, results: Vec<ServerSessionResult>) -> Option<Disconnect> {
        // Completed media in this batch counts too, until it is published.
        if self
            .parser_charge
            .update(parser_budget::held_bytes(
                self.session.inbound_buffered_bytes(),
                &results,
            ))
            .is_err()
        {
            warn_parser_budget_exhausted(&self.client_addr);
            return Some(("session", PARSER_BUDGET_EXHAUSTED, true));
        }
        let outcome = self.handle_session_results(results).await;
        // Completed media has been published to the ring.
        self.parser_charge
            .release_to(self.session.inbound_buffered_bytes());
        match outcome {
            Ok(accepted_publish) => {
                self.publishing |= accepted_publish;
                None
            }
            Err(_) if self.shutdown.is_cancelled() => Some(SHUTTING_DOWN),
            Err(error) => Some(("session", error, true)),
        }
    }
}

#[cold]
fn warn_read_error(client_addr: &str) {
    warn!("read error in main loop for {}", client_addr);
}

#[cold]
fn warn_parse_error(error: &rml_rtmp::sessions::ServerSessionError, client_addr: &str) {
    warn!(%error, "session parse error for {}", client_addr);
}

#[cold]
fn warn_parser_budget_exhausted(client_addr: &str) {
    warn!(
        "RTMP ingest parser budget exhausted; rejecting {}",
        client_addr
    );
}
