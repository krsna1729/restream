use super::*;
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_session_results(
    session: &mut ServerSession,
    results: Vec<ServerSessionResult>,
    socket: &mut RtmpClientSocket,
    commands: &mpsc::Sender<RtmpControlCommand>,
    client_ip: &str,
    client_addr: &str,
    engine: &MediaEngine,
    shutdown: &CancellationToken,
    publisher: &mut Option<Box<RtmpPublisherMedia>>,
) -> Result<bool, &'static str> {
    let mut accepted_publish = false;
    for result in results {
        match result {
            ServerSessionResult::OutboundResponse(packet) => {
                socket
                    .write_all(packet.bytes)
                    .await
                    .map_err(|_| "Failed to write outbound response")?;
            }
            ServerSessionResult::RaisedEvent(event) => match event {
                ServerSessionEvent::ConnectionRequested { request_id, .. } => {
                    if let Ok(responses) = session.accept_request(request_id) {
                        write_responses(socket, responses).await?;
                    }
                }
                ServerSessionEvent::PublishStreamRequested {
                    request_id,
                    stream_key,
                    ..
                } => {
                    accept_publish(
                        session,
                        socket,
                        commands,
                        client_ip,
                        client_addr,
                        engine,
                        shutdown,
                        publisher,
                        request_id,
                        stream_key,
                    )
                    .await?;
                    accepted_publish = true;
                }
                // Media runs to completion here; only a one-time stream probe
                // goes to the control session.
                ServerSessionEvent::VideoDataReceived {
                    data, timestamp, ..
                } => {
                    if let Some(probe) = publisher
                        .as_mut()
                        .and_then(|media| media.on_video(data, timestamp.value))
                    {
                        send_control_command(
                            commands,
                            shutdown,
                            RtmpControlCommand::MediaProbe(probe),
                        )
                        .await?;
                    }
                }
                ServerSessionEvent::AudioDataReceived {
                    data, timestamp, ..
                } => {
                    if let Some(probe) = publisher
                        .as_mut()
                        .and_then(|media| media.on_audio(data, timestamp.value))
                    {
                        send_control_command(
                            commands,
                            shutdown,
                            RtmpControlCommand::MediaProbe(probe),
                        )
                        .await?;
                    }
                }
                // The encoder's own description: kept for the dashboard,
                // never forwarded (each egress writes its own).
                ServerSessionEvent::StreamMetadataChanged { metadata, .. } => {
                    report_encoder(commands, shutdown, publisher.is_some(), metadata).await?;
                }
                ServerSessionEvent::PlayStreamRequested {
                    request_id,
                    stream_key,
                    stream_id,
                    ..
                } => {
                    return handle_play_request(RtmpPlayRequest {
                        session,
                        socket,
                        commands,
                        shutdown,
                        client_ip,
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
                _ => {}
            },
            ServerSessionResult::UnhandleableMessageReceived(_) => {}
        }
    }
    Ok(accepted_publish)
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

#[allow(clippy::too_many_arguments)]
async fn accept_publish(
    session: &mut ServerSession,
    socket: &mut RtmpClientSocket,
    commands: &mpsc::Sender<RtmpControlCommand>,
    client_ip: &str,
    client_addr: &str,
    engine: &MediaEngine,
    shutdown: &CancellationToken,
    publisher: &mut Option<Box<RtmpPublisherMedia>>,
    request_id: u32,
    stream_key: String,
) -> Result<(), &'static str> {
    let (reply, response) = oneshot::channel();
    send_control_command(
        commands,
        shutdown,
        RtmpControlCommand::PublishRequested {
            stream_key,
            client_ip: client_ip.to_string(),
            client_addr: client_addr.to_string(),
            reply,
        },
    )
    .await?;
    let decision = tokio::select! {
        _ = shutdown.cancelled() => {
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
            let _ = session.reject_request(request_id, code, description);
            return Err(error);
        }
        PublishAuthorization::Accepted(media) => *publisher = Some(media),
        PublishAuthorization::Cancelled => {
            return Err("RTMP listener shutting down");
        }
    }
    set_tcp_socket_buffers(socket.as_fd(), engine.config.rtmp_stream_buffer_bytes);
    let responses = session
        .accept_request(request_id)
        .map_err(|_| "Failed to accept publish request")?;
    write_responses(socket, responses).await?;
    send_control_command(
        commands,
        shutdown,
        RtmpControlCommand::PublishAccepted {
            client_ip: client_ip.to_string(),
        },
    )
    .await?;
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
