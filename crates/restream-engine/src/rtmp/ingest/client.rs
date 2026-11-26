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

struct RtmpClient<'a> {
    session: ServerSession,
    socket: RtmpClientSocket,
    parser_charge: parser_budget::ParserCharge,
    /// Set when the publish is authorized: this connection's media, run here.
    publisher: Option<Box<RtmpPublisherMedia>>,
    publishing: bool,
    commands: &'a mpsc::Sender<RtmpControlCommand>,
    client_ip: String,
    client_addr: String,
    engine: &'a MediaEngine,
    shutdown: &'a CancellationToken,
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

impl RtmpClient<'_> {
    /// Handles one batch of session results; `Ok(true)` when it accepted a
    /// publish.
    async fn handle_session_results(
        &mut self,
        results: Vec<ServerSessionResult>,
    ) -> Result<bool, &'static str> {
        let mut accepted_publish = false;
        for result in results {
            match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    self.socket
                        .write_all(packet.bytes)
                        .await
                        .map_err(|_| "Failed to write outbound response")?;
                }
                ServerSessionResult::RaisedEvent(event) => match event {
                    ServerSessionEvent::PublishStreamRequested {
                        request_id,
                        stream_key,
                        ..
                    } => {
                        self.accept_publish(request_id, stream_key).await?;
                        accepted_publish = true;
                    }
                    // Media runs to completion here; only a one-time stream
                    // probe goes to the control session.
                    ServerSessionEvent::VideoDataReceived {
                        data, timestamp, ..
                    } => {
                        if let Some(probe) = self
                            .publisher
                            .as_mut()
                            .and_then(|media| media.on_video(data, timestamp.value))
                        {
                            send_control_command(
                                self.commands,
                                self.shutdown,
                                RtmpControlCommand::MediaProbe(probe),
                            )
                            .await?;
                        }
                    }
                    ServerSessionEvent::AudioDataReceived {
                        data, timestamp, ..
                    } => {
                        if let Some(probe) = self
                            .publisher
                            .as_mut()
                            .and_then(|media| media.on_audio(data, timestamp.value))
                        {
                            send_control_command(
                                self.commands,
                                self.shutdown,
                                RtmpControlCommand::MediaProbe(probe),
                            )
                            .await?;
                        }
                    }
                    ServerSessionEvent::PlayStreamRequested {
                        request_id,
                        stream_key,
                        stream_id,
                        ..
                    } => {
                        return handle_play_request(RtmpPlayRequest {
                            session: &mut self.session,
                            socket: &mut self.socket,
                            commands: self.commands,
                            shutdown: self.shutdown,
                            client_ip: &self.client_ip,
                            request_id,
                            stream_key,
                            stream_id,
                        })
                        .await
                        .map(|()| accepted_publish);
                    }
                    ServerSessionEvent::PublishStreamFinished { .. } => {
                        return Err("Publish finished by client");
                    }
                    event => {
                        handle_control_event(
                            event,
                            &mut self.session,
                            &mut self.socket,
                            self.commands,
                            self.shutdown,
                            self.publisher.is_some(),
                        )
                        .await?;
                    }
                },
                ServerSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
        Ok(accepted_publish)
    }

    async fn accept_publish(
        &mut self,
        request_id: u32,
        stream_key: String,
    ) -> Result<(), &'static str> {
        let (reply, response) = oneshot::channel();
        send_control_command(
            self.commands,
            self.shutdown,
            RtmpControlCommand::PublishRequested {
                stream_key,
                client_ip: self.client_ip.clone(),
                client_addr: self.client_addr.clone(),
                reply,
            },
        )
        .await?;
        let decision = tokio::select! {
            _ = self.shutdown.cancelled() => {
                return Err("RTMP listener shutting down");
            }
            response = response => {
                response.map_err(|_| "RTMP control session closed")?
            }
        };
        match decision {
            PublishAuthorization::Rejected {
                code,
                description,
                error,
            } => {
                let _ = self.session.reject_request(request_id, code, description);
                return Err(error);
            }
            PublishAuthorization::Accepted(media) => self.publisher = Some(media),
            PublishAuthorization::Cancelled => {
                return Err("RTMP listener shutting down");
            }
        }
        set_tcp_socket_buffers(
            self.socket.as_fd(),
            self.engine.config.rtmp_stream_buffer_bytes,
        );
        let responses = self
            .session
            .accept_request(request_id)
            .map_err(|_| "Failed to accept publish request")?;
        write_responses(&mut self.socket, responses).await?;
        send_control_command(
            self.commands,
            self.shutdown,
            RtmpControlCommand::PublishAccepted {
                client_ip: self.client_ip.clone(),
            },
        )
        .await?;
        Ok(())
    }
}

async fn handle_control_event(
    event: ServerSessionEvent,
    session: &mut ServerSession,
    socket: &mut RtmpClientSocket,
    commands: &mpsc::Sender<RtmpControlCommand>,
    shutdown: &CancellationToken,
    publishing: bool,
) -> Result<(), &'static str> {
    match event {
        ServerSessionEvent::ConnectionRequested { request_id, .. } => {
            if let Ok(responses) = session.accept_request(request_id) {
                write_responses(socket, responses).await?;
            }
            Ok(())
        }
        // The encoder's own description: kept for the dashboard,
        // never forwarded (each egress writes its own).
        ServerSessionEvent::StreamMetadataChanged { metadata, .. } => {
            report_encoder(commands, shutdown, publishing, metadata).await
        }
        _ => Ok(()),
    }
}

async fn write_responses(
    socket: &mut RtmpClientSocket,
    responses: Vec<ServerSessionResult>,
) -> Result<(), &'static str> {
    for response in responses {
        if let ServerSessionResult::OutboundResponse(packet) = response {
            socket
                .write_all(packet.bytes)
                .await
                .map_err(|_| "Write error")?;
        }
    }
    Ok(())
}

async fn report_encoder(
    commands: &mpsc::Sender<RtmpControlCommand>,
    shutdown: &CancellationToken,
    publishing: bool,
    metadata: rml_rtmp::sessions::StreamMetadata,
) -> Result<(), &'static str> {
    if publishing
        && let Some(declared) = restream_core::metadata::DeclaredEncoder::from_metadata(
            metadata.encoder.as_deref(),
            metadata.video_width,
            metadata.video_height,
            metadata.video_frame_rate,
            metadata.video_bitrate_kbps,
            metadata.audio_bitrate_kbps,
        )
    {
        send_control_command(
            commands,
            shutdown,
            RtmpControlCommand::EncoderDeclared(declared),
        )
        .await?;
    }
    Ok(())
}
