//! Generalized RTMP sink and probe helpers for the test harness.

use super::*;

// ── Generalized harness sink (Phase 1) ──────────────────────────────────────
//
// Extends the existing SinkMetrics from byte-counting to packet-level tracking
// with timestamps, format, keyframe flags, and counts — the single source of
// truth for egress correctness in live tests.

/// Packet-level observation captured by the generalized RTMP sink.
pub(crate) struct SinkPacket {
    pub(crate) media_type: &'static str,
    pub(crate) timestamp_ms: u64,
    pub(crate) audio_packet_type: Option<u8>,
    pub(crate) audio_has_adts_sync: bool,
    pub(crate) video_is_sequence_header: bool,
}

/// Shared counters and packet history for generalized sink assertions.
pub(crate) struct GeneralizedSinkMetrics {
    pub(crate) connections: AtomicUsize,
    pub(crate) publishing: AtomicUsize,
    pub(crate) messages: AtomicU64,
    pub(crate) bytes: AtomicU64,
    pub(crate) video_count: AtomicU64,
    pub(crate) audio_count: AtomicU64,
    pub(crate) keyframe_count: AtomicU64,
    pub(crate) packets: Mutex<Vec<SinkPacket>>,
    pub(crate) video_codec: Mutex<Option<String>>,
    pub(crate) audio_codec: Mutex<Option<String>>,
}

impl Default for GeneralizedSinkMetrics {
    fn default() -> Self {
        Self {
            connections: AtomicUsize::new(0),
            publishing: AtomicUsize::new(0),
            messages: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            video_count: AtomicU64::new(0),
            audio_count: AtomicU64::new(0),
            keyframe_count: AtomicU64::new(0),
            packets: Mutex::new(Vec::new()),
            video_codec: Mutex::new(None),
            audio_codec: Mutex::new(None),
        }
    }
}

impl GeneralizedSinkMetrics {
    fn audio_packet_stats(&self) -> (Option<u8>, u64, u64, u64) {
        let packets = self.packets.lock().unwrap();
        let mut first_audio_packet_type = None;
        let mut audio_sequence_headers = 0;
        let mut audio_raw_packets = 0;
        let mut audio_raw_with_adts = 0;

        for pkt in packets.iter().filter(|pkt| pkt.media_type == "audio") {
            if first_audio_packet_type.is_none() {
                first_audio_packet_type = pkt.audio_packet_type;
            }
            match pkt.audio_packet_type {
                Some(0) => audio_sequence_headers += 1,
                Some(1) => {
                    audio_raw_packets += 1;
                    if pkt.audio_has_adts_sync {
                        audio_raw_with_adts += 1;
                    }
                }
                _ => {}
            }
        }

        (
            first_audio_packet_type,
            audio_sequence_headers,
            audio_raw_packets,
            audio_raw_with_adts,
        )
    }

    pub(crate) fn dts_monotone(&self) -> bool {
        let packets = self.packets.lock().unwrap();
        let mut last_video_ts: Option<u64> = None;
        for pkt in packets.iter() {
            if pkt.media_type == "video" {
                if pkt.video_is_sequence_header {
                    continue;
                }
                if let Some(prev) = last_video_ts
                    && pkt.timestamp_ms <= prev
                {
                    return false;
                }
                last_video_ts = Some(pkt.timestamp_ms);
            }
        }
        true
    }

    pub(crate) fn summary(&self) -> Value {
        let (
            first_audio_packet_type,
            audio_sequence_headers,
            audio_raw_packets,
            audio_raw_with_adts,
        ) = self.audio_packet_stats();
        json!({
            "connections": self.connections.load(Ordering::Relaxed),
            "publishing": self.publishing.load(Ordering::Relaxed),
            "messages": self.messages.load(Ordering::Relaxed),
            "bytes": self.bytes.load(Ordering::Relaxed),
            "videoCount": self.video_count.load(Ordering::Relaxed),
            "audioCount": self.audio_count.load(Ordering::Relaxed),
            "keyframeCount": self.keyframe_count.load(Ordering::Relaxed),
            "dtsMonotone": self.dts_monotone(),
            "firstAudioPacketType": first_audio_packet_type,
            "audioSequenceHeaders": audio_sequence_headers,
            "audioRawPackets": audio_raw_packets,
            "audioRawPacketsWithAdts": audio_raw_with_adts,
        })
    }
}

async fn handle_generalized_sink_client(
    mut socket: TcpStream,
    metrics: Arc<GeneralizedSinkMetrics>,
) -> Result<(), String> {
    metrics.connections.fetch_add(1, Ordering::Relaxed);
    let mut handshake = Handshake::new(PeerType::Server);
    let mut buffer = vec![0u8; 8_192];
    let remaining = loop {
        let n = socket.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("socket closed during handshake".to_string());
        }
        match handshake
            .process_bytes(&buffer[..n])
            .map_err(|e| format!("handshake: {e:?}"))?
        {
            HandshakeProcessResult::InProgress { response_bytes } => {
                socket
                    .write_all(&response_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                socket
                    .write_all(&response_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                break remaining_bytes;
            }
        }
    };

    let (mut session, initial) =
        ServerSession::new(ServerSessionConfig::new()).map_err(|e| format!("{e:?}"))?;
    write_generalized_sink_results(&mut socket, &mut session, initial, &metrics).await?;
    if !remaining.is_empty() {
        let results = session
            .handle_input(&remaining)
            .map_err(|e| format!("{e:?}"))?;
        write_generalized_sink_results(&mut socket, &mut session, results, &metrics).await?;
    }

    loop {
        let n = socket.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        let results = session
            .handle_input(&buffer[..n])
            .map_err(|e| format!("{e:?}"))?;
        write_generalized_sink_results(&mut socket, &mut session, results, &metrics).await?;
    }
}

async fn write_generalized_sink_results(
    socket: &mut TcpStream,
    session: &mut ServerSession,
    results: Vec<ServerSessionResult>,
    metrics: &GeneralizedSinkMetrics,
) -> Result<(), String> {
    let mut pending: VecDeque<_> = results.into();
    while let Some(result) = pending.pop_front() {
        match result {
            ServerSessionResult::OutboundResponse(packet) => {
                socket
                    .write_all(&packet.bytes)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            ServerSessionResult::RaisedEvent(event) => match event {
                ServerSessionEvent::ConnectionRequested { request_id, .. } => {
                    let mut accepted = session
                        .accept_request(request_id)
                        .map_err(|e| format!("{e:?}"))?;
                    pending.extend(accepted.drain(..));
                }
                ServerSessionEvent::PublishStreamRequested { request_id, .. } => {
                    let mut accepted = session
                        .accept_request(request_id)
                        .map_err(|e| format!("{e:?}"))?;
                    metrics.publishing.fetch_add(1, Ordering::Relaxed);
                    pending.extend(accepted.drain(..));
                }
                ServerSessionEvent::VideoDataReceived {
                    data, timestamp, ..
                } => {
                    metrics.messages.fetch_add(1, Ordering::Relaxed);
                    metrics
                        .bytes
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                    metrics.video_count.fetch_add(1, Ordering::Relaxed);
                    let tag = data.first().copied().unwrap_or(0);
                    let is_keyframe = (tag & 0xF0) == 0x10 || tag == 0x90;
                    if is_keyframe {
                        metrics.keyframe_count.fetch_add(1, Ordering::Relaxed);
                    }
                    if metrics.video_codec.lock().unwrap().is_none() {
                        let codec = if tag & 0x80 != 0 {
                            if data.len() >= 5 {
                                match &data[1..5] {
                                    b"hvc1" => Some("hevc"),
                                    b"av01" => Some("av1"),
                                    b"vp09" => Some("vp9"),
                                    _ => Some("h264"),
                                }
                            } else {
                                None
                            }
                        } else {
                            match tag & 0x0F {
                                7 => Some("h264"),
                                12 => Some("hevc"),
                                _ => None,
                            }
                        };
                        if let Some(c) = codec {
                            *metrics.video_codec.lock().unwrap() = Some(c.to_string());
                        }
                    }
                    if let Ok(mut pkts) = metrics.packets.lock() {
                        pkts.push(SinkPacket {
                            media_type: "video",
                            timestamp_ms: timestamp.value as u64,
                            audio_packet_type: None,
                            audio_has_adts_sync: false,
                            video_is_sequence_header: (tag & 0x80) == 0
                                && data.get(1).copied() == Some(0),
                        });
                    }
                }
                ServerSessionEvent::AudioDataReceived {
                    data, timestamp, ..
                } => {
                    metrics.messages.fetch_add(1, Ordering::Relaxed);
                    metrics
                        .bytes
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                    metrics.audio_count.fetch_add(1, Ordering::Relaxed);
                    if metrics.audio_codec.lock().unwrap().is_none()
                        && let Some(&tag) = data.first()
                    {
                        let codec = match (tag >> 4) & 0x0F {
                            10 => Some("aac"),
                            2 => Some("mp3"),
                            _ => None,
                        };
                        if let Some(c) = codec {
                            *metrics.audio_codec.lock().unwrap() = Some(c.to_string());
                        }
                    }
                    let audio_packet_type = data.get(1).copied();
                    let audio_has_adts_sync =
                        data.len() >= 4 && data[2] == 0xFF && (data[3] & 0xF0) == 0xF0;
                    if let Ok(mut pkts) = metrics.packets.lock() {
                        pkts.push(SinkPacket {
                            media_type: "audio",
                            timestamp_ms: timestamp.value as u64,
                            audio_packet_type,
                            audio_has_adts_sync,
                            video_is_sequence_header: false,
                        });
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(())
}

// ── Harness sink probe (Phase 4) ──────────────────────────────────────────
//
// Spins up a generalized sink, creates an output pointed at it, waits for
// packets, asserts DTS monotonicity / video+audio presence / keyframes,
// then tears down. Returns the sink summary for embedding in test results.

/// Result bundle returned by the live egress sink-probe helper.
pub(crate) struct SinkProbeResult {
    pub(crate) passed: bool,
    pub(crate) summary: Value,
    pub(crate) output_id: String,
}

/// Running generalized RTMP sink and its spawned connection tasks.
pub(crate) struct GeneralizedSinkServer {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    reader_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

/// RTMP sink that intentionally stops reading to exercise output-stall handling.
pub(crate) struct StalledRtmpSinkServer {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    pub(crate) publish_accepted: Arc<std::sync::atomic::AtomicBool>,
}

fn set_socket_recv_buffer(socket: &TcpStream, size: libc::c_int) -> Result<(), String> {
    // SAFETY: `socket.as_raw_fd()` is a live socket descriptor for the duration
    // of this call, and `size` points to initialized stack memory of the
    // expected type for `SO_RCVBUF`.
    let rc = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &size as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

pub(crate) async fn start_generalized_sink_server(
    sink_port: u16,
    metrics: Arc<GeneralizedSinkMetrics>,
) -> Result<GeneralizedSinkServer, String> {
    let listener = TcpListener::bind(format!("127.0.0.1:{sink_port}"))
        .await
        .map_err(|e| format!("sink bind {sink_port}: {e}"))?;
    let cancel = CancellationToken::new();
    let reader_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(Mutex::new(Vec::new()));
    let reader_handles_inner = reader_handles.clone();
    let metrics_inner = metrics.clone();
    let cancel_inner = cancel.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                result = listener.accept() => {
                    if let Ok((socket, _)) = result {
                        let metrics = metrics_inner.clone();
                        let handle = tokio::spawn(async move {
                            let _ = handle_generalized_sink_client(socket, metrics).await;
                        });
                        reader_handles_inner.lock().unwrap().push(handle);
                    }
                }
                _ = cancel_inner.cancelled() => break,
            }
        }
    });

    Ok(GeneralizedSinkServer {
        cancel,
        task,
        reader_handles,
    })
}

async fn handle_stalled_rtmp_sink_client(
    mut socket: TcpStream,
    publish_accepted: Arc<std::sync::atomic::AtomicBool>,
    cancel: CancellationToken,
) -> Result<(), String> {
    let _ = set_socket_recv_buffer(&socket, 4 * 1024);
    let mut handshake = Handshake::new(PeerType::Server);
    let mut buffer = vec![0u8; 8_192];
    let remaining = loop {
        let n = socket.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("socket closed during handshake".to_string());
        }
        match handshake
            .process_bytes(&buffer[..n])
            .map_err(|e| format!("handshake: {e:?}"))?
        {
            HandshakeProcessResult::InProgress { response_bytes } => {
                socket
                    .write_all(&response_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                socket
                    .write_all(&response_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                break remaining_bytes;
            }
        }
    };

    let (mut session, initial) =
        ServerSession::new(ServerSessionConfig::new()).map_err(|e| format!("{e:?}"))?;
    let mut pending: VecDeque<_> = initial.into();
    if !remaining.is_empty() {
        pending.extend(
            session
                .handle_input(&remaining)
                .map_err(|e| format!("{e:?}"))?,
        );
    }

    loop {
        while let Some(result) = pending.pop_front() {
            match result {
                ServerSessionResult::OutboundResponse(packet) => {
                    socket
                        .write_all(&packet.bytes)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                ServerSessionResult::RaisedEvent(event) => match event {
                    ServerSessionEvent::ConnectionRequested { request_id, .. } => {
                        let mut accepted = session
                            .accept_request(request_id)
                            .map_err(|e| format!("{e:?}"))?;
                        pending.extend(accepted.drain(..));
                    }
                    ServerSessionEvent::PublishStreamRequested { request_id, .. } => {
                        let mut accepted = session
                            .accept_request(request_id)
                            .map_err(|e| format!("{e:?}"))?;
                        publish_accepted.store(true, Ordering::Relaxed);
                        pending.extend(accepted.drain(..));
                        while let Some(response) = pending.pop_front() {
                            if let ServerSessionResult::OutboundResponse(packet) = response {
                                socket
                                    .write_all(&packet.bytes)
                                    .await
                                    .map_err(|e| e.to_string())?;
                            }
                        }
                        loop {
                            tokio::select! {
                                _ = cancel.cancelled() => return Ok(()),
                                _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        let n = socket.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        pending = session
            .handle_input(&buffer[..n])
            .map(|results| results.into())
            .map_err(|e| format!("{e:?}"))?;
    }
}

pub(crate) async fn start_stalled_rtmp_sink_server(
    sink_port: u16,
) -> Result<StalledRtmpSinkServer, String> {
    let listener = TcpListener::bind(format!("127.0.0.1:{sink_port}"))
        .await
        .map_err(|e| format!("stall sink bind {sink_port}: {e}"))?;
    let cancel = CancellationToken::new();
    let cancel_inner = cancel.clone();
    let publish_accepted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let publish_accepted_inner = publish_accepted.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                result = listener.accept() => {
                    if let Ok((socket, _)) = result {
                        let accepted = publish_accepted_inner.clone();
                        let cancel_client = cancel_inner.clone();
                        tokio::spawn(async move {
                            let _ = handle_stalled_rtmp_sink_client(socket, accepted, cancel_client).await;
                        });
                    }
                }
                _ = cancel_inner.cancelled() => break,
            }
        }
    });

    Ok(StalledRtmpSinkServer {
        cancel,
        task,
        publish_accepted,
    })
}

pub(crate) fn stop_stalled_rtmp_sink_server(server: StalledRtmpSinkServer) {
    server.cancel.cancel();
    server.task.abort();
}

pub(crate) fn stop_generalized_sink_server(server: GeneralizedSinkServer) {
    server.cancel.cancel();
    server.task.abort();
    let handles = server.reader_handles.lock().unwrap();
    for handle in handles.iter() {
        handle.abort();
    }
}

pub(crate) fn output_config_from_harness_label(label: &str) -> OutputConfig {
    let mut parts = label.trim().splitn(2, '+');
    let first = parts.next().unwrap_or("source");
    let second = parts.next().filter(|value| !value.is_empty());
    let (video, audio_operation) = if is_audio_operation(first) {
        (
            OutputVideoConfig::Source {
                codec: OutputVideoCodec::Auto,
            },
            Some(first),
        )
    } else {
        let video = match first {
            "" | "source" => OutputVideoConfig::Source {
                codec: OutputVideoCodec::Auto,
            },
            "custom" => OutputVideoConfig::Custom,
            preset => OutputVideoConfig::Preset {
                preset: preset.to_string(),
                codec: OutputVideoCodec::Auto,
            },
        };
        (video, second)
    };

    OutputConfig {
        video,
        audio: audio_operation
            .map(parse_audio_operation)
            .unwrap_or(AudioRouting::Passthrough),
        ..OutputConfig::default()
    }
}

pub(crate) fn output_create_payload(name: &str, url: &str, encoding: &str) -> Value {
    output_create_payload_with_rtmp_mode(name, url, encoding, RtmpOutputMode::Legacy)
}

pub(crate) fn output_create_payload_with_rtmp_mode(
    name: &str,
    url: &str,
    encoding: &str,
    rtmp_mode: RtmpOutputMode,
) -> Value {
    let mut config = output_config_from_harness_label(encoding);
    if OutputUrlScheme::from_url(url).is_rtmp_family() {
        config = config.with_rtmp_mode(rtmp_mode);
    }
    json!({
        "name": name,
        "url": url,
        "config": config,
    })
}

pub(crate) async fn run_sink_probe(
    api: &RampApi,
    pipeline_id: &str,
    label: &str,
    encoding: &str,
    sink_port: u16,
    min_video: u64,
) -> Result<SinkProbeResult, String> {
    let metrics = Arc::new(GeneralizedSinkMetrics::default());
    let server = start_generalized_sink_server(sink_port, metrics.clone()).await?;
    let sink_url = format!("rtmp://127.0.0.1:{sink_port}/live/sink-probe-{label}");
    let output_id = match create_output(
        api,
        pipeline_id,
        &format!("sink-{label}"),
        &sink_url,
        encoding,
    )
    .await
    {
        Ok(output_id) => output_id,
        Err(error) => {
            stop_generalized_sink_server(server);
            return Err(error);
        }
    };
    if let Err(error) = start_output(api, pipeline_id, &output_id).await {
        stop_generalized_sink_server(server);
        return Err(error);
    }

    let deadline = Instant::now() + Duration::from_secs(20);
    while metrics.video_count.load(Ordering::Relaxed) < min_video {
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    let dts_ok = metrics.dts_monotone();
    let video = metrics.video_count.load(Ordering::Relaxed);
    let audio = metrics.audio_count.load(Ordering::Relaxed);
    let keyframes = metrics.keyframe_count.load(Ordering::Relaxed);
    let summary = metrics.summary();

    // Stop the output
    let _ = api
        .post_empty(&format!(
            "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop"
        ))
        .await;
    wait_for_sink_probe_stopped(api, pipeline_id, &output_id).await;
    stop_generalized_sink_server(server);

    let passed = video >= min_video && audio > 0 && keyframes > 0 && dts_ok;
    if !passed {
        eprintln!(
            "[sink-probe:{label}] FAIL: video={video} audio={audio} keyframes={keyframes} dts_monotone={dts_ok}"
        );
    } else {
        println!(
            "[sink-probe:{label}] ok: video={video} audio={audio} keyframes={keyframes} dts_monotone={dts_ok}"
        );
    }

    Ok(SinkProbeResult {
        passed,
        summary,
        output_id,
    })
}

async fn wait_for_sink_probe_stopped(api: &RampApi, pipeline_id: &str, output_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match api
            .get_output_status_or_not_found(pipeline_id, output_id)
            .await
        {
            Ok(None) => return,
            Ok(Some((status, _))) if status.status != "running" => return,
            Ok(Some(_)) | Err(_) => {}
        }
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
