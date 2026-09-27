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
                        for response in responses {
                            if let ServerSessionResult::OutboundResponse(packet) = response {
                                socket
                                    .write_all(packet.bytes)
                                    .await
                                    .map_err(|_| "Write error")?;
                            }
                        }
                    }
                }
                ServerSessionEvent::PublishStreamRequested {
                    request_id,
                    stream_key,
                    ..
                } => {
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
                    set_tcp_socket_buffers(socket.raw_fd(), engine.config.rtmp_stream_buffer_bytes);
                    let responses = session
                        .accept_request(request_id)
                        .map_err(|_| "Failed to accept publish request")?;
                    for response in responses {
                        if let ServerSessionResult::OutboundResponse(packet) = response {
                            socket
                                .write_all(packet.bytes)
                                .await
                                .map_err(|_| "Write error")?;
                        }
                    }
                    send_control_command(
                        commands,
                        shutdown,
                        RtmpControlCommand::PublishAccepted {
                            client_ip: client_ip.to_string(),
                        },
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
