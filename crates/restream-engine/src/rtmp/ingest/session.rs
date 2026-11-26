use super::client::RtmpClient;
use super::*;

impl RtmpClient<'_> {
    /// Handles one batch of session results; `Ok(true)` when it accepted a
    /// publish.
    pub(super) async fn handle_session_results(
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
