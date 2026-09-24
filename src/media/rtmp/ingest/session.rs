use super::*;
pub(super) async fn process_video_data(
    engine: &MediaEngine,
    active: &mut RtmpIngestHandle,
    probe: &mut ProbeState,
    data: Bytes,
    timestamp: u32,
) {
    let pipeline_id = &active.pipeline_id;
    active
        .bytes_received
        .fetch_add(data.len() as u64, Ordering::Relaxed);
    active.ingest_metrics.record_in(data.len() as u64);
    active
        .last_progress_ms
        .store(MediaEngine::now_epoch_ms(), Ordering::Relaxed);

    let packet_kind = classify_flv_video_packet(&data);
    let is_keyframe = matches!(packet_kind, Some(FlvVideoPacketKind::Keyframe));
    let dts = timestamp as i64;
    let pts = dts + flv_video_composition_time_ms(&data) as i64;
    let parameter_sets = flv_avcc_config_annexb_parameter_sets(&data);
    if matches!(packet_kind, Some(FlvVideoPacketKind::SequenceHeader)) && (data[0] & 0x0F) == 7 {
        engine
            .cache_ingest_session_sequence_header(&active.registration, true, data.clone())
            .await;
    }
    if !probe.video_done
        && let Some(meta) = parse_flv_video_meta(&data)
    {
        if meta.width > 0 {
            probe.video_done = true;
        }
        info!(
            "[rtmp] Probed video: {} {}x{} profile={:?} level={:?}",
            meta.codec, meta.width, meta.height, meta.profile, meta.level
        );
        engine
            .update_ingest_session_meta(pipeline_id, &active.registration, Some(meta), None, None)
            .await;
    }

    let mut packet = MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts,
        dts,
        is_keyframe,
        format: PayloadFormat::Flv,
        payload: data,
    };
    let boundary = if is_keyframe {
        InputPacketBoundary::VideoKeyframe
    } else {
        InputPacketBoundary::Other
    };
    if let Some(preview_ring) = active.registration.preview_ring.load_full() {
        if let Some(parameter_sets) = parameter_sets.clone() {
            preview_ring.set_video_parameter_sets(parameter_sets);
        }
        preview_ring.push(packet.clone());
    }
    if active.registration.gate.state() == InputForwardState::Active {
        let Some(lease) = active.registration.gate.try_enter(boundary) else {
            return;
        };
        active.timestamp_mapper.map_packet(
            &mut packet,
            false,
            &active.registration.last_forwarded_dts,
        );
        if let Some(parameter_sets) = parameter_sets {
            active.ring.set_video_parameter_sets(parameter_sets);
        }
        let keyframe_pts = is_keyframe.then_some(packet.pts);
        InputTimestampMapper::record_forwarded(&packet, &active.registration.last_forwarded_dts);
        active.ring.push(packet);
        drop(lease);
        if let Some(pts) = keyframe_pts {
            engine.record_keyframe(pipeline_id, pts).await;
        }
    } else {
        active.standby_gop.push(packet);
        try_promote_cached_rtmp(engine, active).await;
    }
}
pub(super) async fn process_audio_data(
    engine: &MediaEngine,
    active: &mut RtmpIngestHandle,
    probe: &mut ProbeState,
    data: Bytes,
    timestamp: u32,
) {
    let pipeline_id = &active.pipeline_id;
    active
        .bytes_received
        .fetch_add(data.len() as u64, Ordering::Relaxed);
    active.ingest_metrics.record_in(data.len() as u64);
    active
        .last_progress_ms
        .store(MediaEngine::now_epoch_ms(), Ordering::Relaxed);

    if data.len() >= 2 && (data[0] >> 4) == 10 && data[1] == 0 {
        engine
            .cache_ingest_session_sequence_header(&active.registration, false, data.clone())
            .await;
    }
    if !probe.audio_done {
        let format_id = data.first().map(|byte| (byte >> 4) & 0x0f).unwrap_or(0xff);
        let has_complete_config = format_id != 10 || (data.len() >= 3 && data[1] == 0);
        if has_complete_config && let Some(meta) = parse_flv_audio_meta(&data) {
            probe.audio_done = true;
            info!(
                "[rtmp] Probed audio: {} {}Hz {}ch",
                meta.codec, meta.sample_rate, meta.channels
            );
            engine
                .update_ingest_session_meta(
                    pipeline_id,
                    &active.registration,
                    None,
                    Some(meta.clone()),
                    None,
                )
                .await;
            engine
                .update_ingest_session_audio_tracks(pipeline_id, &active.registration, vec![meta])
                .await;
        }
    }

    let mut packet = MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts: timestamp as i64,
        dts: timestamp as i64,
        is_keyframe: false,
        format: PayloadFormat::Flv,
        payload: data,
    };
    if let Some(preview_ring) = active.registration.preview_ring.load_full() {
        preview_ring.push(packet.clone());
    }
    if active.registration.gate.state() == InputForwardState::Active {
        let Some(lease) = active
            .registration
            .gate
            .try_enter(InputPacketBoundary::Other)
        else {
            return;
        };
        active.timestamp_mapper.map_packet(
            &mut packet,
            false,
            &active.registration.last_forwarded_dts,
        );
        InputTimestampMapper::record_forwarded(&packet, &active.registration.last_forwarded_dts);
        active.ring.push(packet);
        drop(lease);
    } else {
        active.standby_gop.push(packet);
        try_promote_cached_rtmp(engine, active).await;
    }
}
pub(super) struct ProbeState {
    pub(super) video_done: bool,
    pub(super) audio_done: bool,
}
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
    media_handoff: &Arc<tokio::sync::Semaphore>,
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
                        PublishAuthorization::Accepted => {}
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
                ServerSessionEvent::VideoDataReceived {
                    data, timestamp, ..
                } => {
                    handoff_media_data(
                        commands,
                        media_handoff,
                        shutdown,
                        MediaType::Video,
                        data,
                        timestamp.value,
                    )
                    .await?;
                }
                ServerSessionEvent::AudioDataReceived {
                    data, timestamp, ..
                } => {
                    handoff_media_data(
                        commands,
                        media_handoff,
                        shutdown,
                        MediaType::Audio,
                        data,
                        timestamp.value,
                    )
                    .await?;
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
