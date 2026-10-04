#[tokio::test]
async fn client_handshake_can_be_bounded_when_peer_is_silent() {
    let (mut client, mut server) = tokio::io::duplex(4096);
    let cancel = CancellationToken::new();
    let peer = tokio::spawn(async move {
        let mut buf = [0u8; 1537];
        server.read_exact(&mut buf).await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let result = tokio::time::timeout(
        Duration::from_millis(25),
        perform_client_handshake(&mut client, &cancel),
    )
    .await;

    assert!(result.is_err(), "silent peer should not complete handshake");
    cancel.cancel();
    peer.abort();
}

/// Accepts every stream key as the same fixed pipeline id.
struct AcceptAllAuthenticator {
    pipeline_id: String,
}

impl PipelineAccessAuthenticator for AcceptAllAuthenticator {
    fn authenticate<'a>(
        &'a self,
        _mode: PipelineAccessMode,
        _stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            Ok(AuthenticatedPipeline {
                id: self.pipeline_id.clone(),
                input_id: _stream_key.to_string(),
                selected: true,
            })
        })
    }
}

/// Drives a real `rml_rtmp` `ClientSession` through handshake, connect, and
/// publish against `socket`, blocking until the server has accepted the
/// publish request. This reuses the same client-session machinery
/// `start_rtmp_egress` uses in production, so the resulting wire bytes are a
/// genuine RTMP publish handshake rather than hand-rolled AMF.
async fn drive_client_publish_handshake(socket: &mut TcpStream, stream_key: &str) -> bool {
    let cancel = CancellationToken::new();
    let remaining = perform_client_handshake(socket, &cancel)
        .await
        .expect("client handshake must succeed against handle_rtmp_client");

    let mut config = ClientSessionConfig::new();
    config.tc_url = Some("rtmp://127.0.0.1/live".to_string());
    let (mut session, initial_results) =
        ClientSession::new(config).expect("client session must initialize");
    for res in initial_results {
        if let ClientSessionResult::OutboundResponse(pkt) = res {
            socket.write_all(&pkt.bytes).await.unwrap();
        }
    }

    let conn_pkt = match session.request_connection("live".to_string()) {
        Ok(ClientSessionResult::OutboundResponse(p)) => p,
        other => panic!("expected connect request packet, got {other:?}"),
    };
    socket.write_all(&conn_pkt.bytes).await.unwrap();

    let mut buffer = vec![0u8; 4096];
    let mut pending = remaining;
    loop {
        let results = if !pending.is_empty() {
            let taken = std::mem::take(&mut pending);
            session.handle_input(&taken).unwrap()
        } else {
            let count = socket.read(&mut buffer).await.unwrap();
            if count == 0 {
                return false;
            }
            session.handle_input(&buffer[..count]).unwrap()
        };

        let mut published = false;
        for res in results {
            match res {
                ClientSessionResult::OutboundResponse(pkt) => {
                    socket.write_all(&pkt.bytes).await.unwrap();
                }
                ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestAccepted) => {
                    let pub_pkt = match session
                        .request_publishing(stream_key.to_string(), PublishRequestType::Live)
                    {
                        Ok(ClientSessionResult::OutboundResponse(p)) => p,
                        other => panic!("expected publish request packet, got {other:?}"),
                    };
                    socket.write_all(&pub_pkt.bytes).await.unwrap();
                }
                ClientSessionResult::RaisedEvent(ClientSessionEvent::PublishRequestAccepted) => {
                    published = true;
                }
                _ => {}
            }
        }
        if published {
            return true;
        }
    }
}

struct RejectAuthenticator;

impl PipelineAccessAuthenticator for RejectAuthenticator {
    fn authenticate<'a>(
        &'a self,
        _mode: PipelineAccessMode,
        _stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async { Err(crate::media::ingest_auth::PipelineAccessError::InvalidStreamKey) })
    }
}

#[tokio::test]
async fn rejected_publish_never_registers_ingest() {
    let (engine, addr, server) =
        start_ingress_test_server(Arc::new(RejectAuthenticator)).await;
    let mut client = TcpStream::connect(addr).await.unwrap();
    let published = tokio::time::timeout(
        Duration::from_secs(5),
        drive_client_publish_handshake(&mut client, "invalid-key"),
    )
    .await
    .expect("rejected publish should close without hanging");
    assert!(!published, "an unauthenticated publish must not be accepted");
    assert!(
        engine.ingests.active.read().await.is_empty(),
        "authorization must precede ingest registration"
    );
    drop(client);
    stop_ingress_test_server(&engine, server).await;
}

struct PendingAuthenticator {
    entered: Arc<tokio::sync::Notify>,
}

impl PipelineAccessAuthenticator for PendingAuthenticator {
    fn authenticate<'a>(
        &'a self,
        _mode: PipelineAccessMode,
        _stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        self.entered.notify_one();
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn shutdown_cancels_pending_publish_auth_before_owner_join() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let (engine, addr, server) = start_ingress_test_server(Arc::new(PendingAuthenticator {
        entered: entered.clone(),
    }))
    .await;
    let client = tokio::spawn(async move {
        let mut client = TcpStream::connect(addr).await.unwrap();
        drive_client_publish_handshake(&mut client, "pending-key").await
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("publish auth request should reach the pending authenticator");

    engine.shutdown_listeners();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("listener and control actors should join after cancellation")
        .expect("listener task should not panic");
    assert!(
        !tokio::time::timeout(Duration::from_secs(5), client)
            .await
            .expect("pending-auth client should close")
            .expect("pending-auth client task should not panic"),
        "shutdown should close the publish connection without accepting it"
    );
    assert!(
        engine.ingests.active.read().await.is_empty(),
        "cancelled auth must not register an ingest"
    );
    join_test_owner_threads(&engine).await;
}

async fn drive_client_play_handshake(socket: &mut TcpStream, stream_key: &str) -> ClientSession {
    let cancel = CancellationToken::new();
    let remaining = perform_client_handshake(socket, &cancel)
        .await
        .expect("client handshake must succeed against RTMP ingress owner");

    let mut config = ClientSessionConfig::new();
    config.tc_url = Some("rtmp://127.0.0.1/live".to_string());
    let (mut session, initial_results) =
        ClientSession::new(config).expect("client session must initialize");
    for result in initial_results {
        if let ClientSessionResult::OutboundResponse(packet) = result {
            socket.write_all(&packet.bytes).await.unwrap();
        }
    }
    let connect = match session.request_connection("live".to_string()) {
        Ok(ClientSessionResult::OutboundResponse(packet)) => packet,
        other => panic!("expected connect request packet, got {other:?}"),
    };
    socket.write_all(&connect.bytes).await.unwrap();

    let mut buffer = vec![0u8; 4096];
    let mut pending = remaining;
    loop {
        let results = if pending.is_empty() {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0, "server closed during playback setup");
            session.handle_input(&buffer[..count]).unwrap()
        } else {
            session.handle_input(&std::mem::take(&mut pending)).unwrap()
        };
        let mut playing = false;
        for result in results {
            match result {
                ClientSessionResult::OutboundResponse(packet) => {
                    socket.write_all(&packet.bytes).await.unwrap();
                }
                ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestAccepted) => {
                    let request = match session.request_playback(stream_key.to_string()) {
                        Ok(ClientSessionResult::OutboundResponse(packet)) => packet,
                        other => panic!("expected playback request packet, got {other:?}"),
                    };
                    socket.write_all(&request.bytes).await.unwrap();
                }
                ClientSessionResult::RaisedEvent(ClientSessionEvent::PlaybackRequestAccepted) => {
                    playing = true;
                }
                _ => {}
            }
        }
        if playing {
            return session;
        }
    }
}

#[tokio::test]
async fn idle_playback_is_cancelled_with_listener_shutdown() {
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> =
        Arc::new(AcceptAllAuthenticator {
            pipeline_id: "pipe-play-shutdown".to_string(),
        });
    let (engine, addr, server) = start_ingress_test_server(pipeline_access).await;

    let mut publisher = TcpStream::connect(addr).await.unwrap();
    assert!(drive_client_publish_handshake(&mut publisher, "any-key").await);
    let mut player = TcpStream::connect(addr).await.unwrap();
    drive_client_play_handshake(&mut player, "any-key").await;
    engine.shutdown_listeners();
    wait_for_ingest_cleanup(&engine, "pipe-play-shutdown").await;
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("RTMP listener should stop after cancelling idle playback")
        .expect("RTMP listener should not panic");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), player.read(&mut [0u8; 1]))
            .await
            .expect("play connection should close")
            .expect("play socket read should succeed"),
        0,
        "listener shutdown must close the active play connection"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), publisher.read(&mut [0u8; 1]))
            .await
            .expect("publish connection should close")
            .expect("publish socket read should succeed"),
        0,
        "listener shutdown must close the active publish connection"
    );
    drop(player);
    drop(publisher);
    join_test_owner_threads(&engine).await;
}

#[tokio::test]
async fn shutdown_cancels_blocked_play_write_and_joins_owner() {
    let pipeline_id = "pipe-blocked-play-write";
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> =
        Arc::new(AcceptAllAuthenticator {
            pipeline_id: pipeline_id.to_string(),
        });
    let (engine, addr, server) = start_ingress_test_server(pipeline_access).await;
    let mut publisher = TcpStream::connect(addr).await.unwrap();
    assert!(drive_client_publish_handshake(&mut publisher, "any-key").await);
    let ring = engine.get_or_create_pipeline(pipeline_id).await;
    for index in 0..4 {
        let is_keyframe = index == 0;
        let mut payload = vec![0x55; 8 * 1024 * 1024];
        payload[0] = if is_keyframe { 0x17 } else { 0x27 };
        payload[1] = 1;
        let nalu_len = (payload.len() - 9) as u32;
        payload[5..9].copy_from_slice(&nalu_len.to_be_bytes());
        payload[9] = if is_keyframe { 0x65 } else { 0x41 };
        ring.push(crate::media::packet::MediaPacket {
            media_type: crate::media::packet::MediaType::Video,
            format: crate::media::packet::PayloadFormat::Flv,
            is_keyframe,
            track_index: 0,
            pts: index,
            dts: index,
            payload: bytes::Bytes::from(payload),
        });
    }

    let mut player = TcpStream::connect(addr).await.unwrap();
    drive_client_play_handshake(&mut player, "any-key").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while ring.active_reader_count() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("playback reader should attach before the slow peer stalls writes");
    // The peer deliberately stops reading. Four large RTMP media messages
    // exceed the socket buffers, leaving the Compio write pending at shutdown.
    tokio::time::sleep(Duration::from_millis(100)).await;

    engine.shutdown_listeners();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("shutdown should cancel the pending play write and join owner")
        .expect("RTMP listener task should not panic");
    wait_for_ingest_cleanup(&engine, pipeline_id).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), publisher.read(&mut [0u8; 1]))
            .await
            .expect("publisher should close during owner shutdown")
            .expect("publisher socket read should succeed"),
        0,
        "listener shutdown must close the active publisher"
    );
    drop(player);
    join_test_owner_threads(&engine).await;
}

fn test_engine_and_security() -> (Arc<MediaEngine>, Arc<IngestSecurityService>) {
    (
        Arc::new(MediaEngine::new()),
        Arc::new(IngestSecurityService::new(IngestSecurityConfig::default())),
    )
}

/// A chunk with a non-zero format on a chunk stream id that has never seen a
/// type-0 header is invalid per the RTMP chunk spec (rml_rtmp's
/// `ChunkDeserializationError::NoPreviousChunkOnStream`). It is a single
/// byte, so it deterministically faults on the very next read instead of
/// stalling while the deserializer waits for more bytes.
const MALFORMED_CHUNK_HEADER_BYTE: [u8; 1] = [0x45];

async fn start_ingress_test_server(
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
) -> (
    Arc<MediaEngine>,
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
) {
    let (engine, _) = test_engine_and_security();
    start_ingress_test_server_with_engine(pipeline_access, engine).await
}

async fn start_ingress_test_server_with_engine(
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    engine: Arc<MediaEngine>,
) -> (
    Arc<MediaEngine>,
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
) {
    let security = Arc::new(IngestSecurityService::new(IngestSecurityConfig::default()));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(super::listener::start_rtmp_server_on_with_shutdown(
        pipeline_access,
        security,
        engine.clone(),
        0,
        CancellationToken::new(),
        Some(started_tx),
    ));
    let startup = match tokio::time::timeout(Duration::from_secs(5), started_rx).await {
        Ok(startup) => startup,
        Err(error) => {
            engine.shutdown_listeners();
            let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
            join_test_owner_threads(&engine).await;
            panic!("RTMP owner startup timed out: {error}");
        }
    };
    let address = match startup {
        Ok(Ok(address)) => address,
        Ok(Err(error)) => {
            server.await.expect("failed RTMP startup task should not panic");
            join_test_owner_threads(&engine).await;
            panic!("RTMP io_uring owner startup failed: {error}");
        }
        Err(error) => {
            server.await.expect("failed RTMP startup task should not panic");
            join_test_owner_threads(&engine).await;
            panic!("RTMP owner exited before startup readiness: {error}");
        }
    };
    (engine, address, server)
}

async fn join_test_owner_threads(engine: &Arc<MediaEngine>) {
    let handles = engine.drain_os_thread_handles();
    tokio::task::spawn_blocking(move || {
        for handle in handles {
            handle.join().expect("RTMP owner thread should join");
        }
    })
    .await
    .expect("RTMP owner joins should complete");
}

async fn wait_for_ingest_cleanup(engine: &MediaEngine, pipeline_id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !engine
                .ingests
                .active
                .read()
                .await
                .contains_key(pipeline_id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("RTMP owner must clean up the active ingest");
}

async fn stop_ingress_test_server(
    engine: &Arc<MediaEngine>,
    server: tokio::task::JoinHandle<()>,
) {
    engine.shutdown_listeners();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("RTMP listener should stop")
        .expect("RTMP listener should not panic");
    join_test_owner_threads(engine).await;
}

#[tokio::test]
async fn malformed_chunk_after_publish_clears_ingest_registration() {
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> =
        Arc::new(AcceptAllAuthenticator {
            pipeline_id: "pipe-fault-malformed".to_string(),
        });
    let (engine, addr, server) = start_ingress_test_server(pipeline_access).await;

    let mut client = TcpStream::connect(addr).await.unwrap();
    assert!(drive_client_publish_handshake(&mut client, "any-key").await);
    assert!(
        engine
            .ingests
            .active
            .read()
            .await
            .contains_key("pipe-fault-malformed"),
        "publish must register an active ingest before fault injection"
    );
    client
        .write_all(&MALFORMED_CHUNK_HEADER_BYTE)
        .await
        .unwrap();
    wait_for_ingest_cleanup(&engine, "pipe-fault-malformed").await;
    drop(client);
    stop_ingress_test_server(&engine, server).await;
}

#[tokio::test]
async fn truncated_chunk_then_disconnect_clears_ingest_registration() {
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> =
        Arc::new(AcceptAllAuthenticator {
            pipeline_id: "pipe-fault-truncated".to_string(),
        });
    let (engine, addr, server) = start_ingress_test_server(pipeline_access).await;

    let mut client = TcpStream::connect(addr).await.unwrap();
    assert!(drive_client_publish_handshake(&mut client, "any-key").await);
    assert!(
        engine
            .ingests
            .active
            .read()
            .await
            .contains_key("pipe-fault-truncated"),
        "publish must register an active ingest before disconnect"
    );
    client.write_all(&[0x03]).await.unwrap();
    drop(client);
    wait_for_ingest_cleanup(&engine, "pipe-fault-truncated").await;
    stop_ingress_test_server(&engine, server).await;
}

/// libx264 AVCDecoderConfigurationRecord (1920x1080@50) as an FLV sequence
/// header tag body.
#[rustfmt::skip]
const AVC_SEQUENCE_HEADER: [u8; 44] = [
    0x17, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x42, 0xc0, 0x2a, 0xff, 0xe1, 0x00, 0x18,
    0x67, 0x42, 0xc0, 0x2a, 0xda, 0x01, 0xe0, 0x08,
    0x9f, 0x97, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00,
    0x10, 0x00, 0x00, 0x06, 0x48, 0xf1, 0x83, 0x2a,
    0x01, 0x00, 0x04, 0x68, 0xce, 0x0f, 0xc8,
];

/// Publish media runs on the owner: the control session authorizes and hands
/// the publisher's media state over; the owner (this test) publishes each
/// message to the ring in order, writes the sequence header into the
/// engine's cache synchronously, and reports only the one-time probe, which
/// the control session records as ingest metadata.
#[tokio::test]
async fn owner_publishes_media_in_order_and_reports_only_probes() {
    let engine = Arc::new(MediaEngine::new());
    let security = Arc::new(IngestSecurityService::new(IngestSecurityConfig::default()));
    let (commands, receiver) = tokio::sync::mpsc::channel(1);
    let actor = tokio::spawn(super::ingest::run_rtmp_control_session(
        receiver,
        CancellationToken::new(),
        Arc::new(AcceptAllAuthenticator {
            pipeline_id: "pipe-media-order".to_string(),
        }),
        security,
        engine.clone(),
    ));

    let (reply, response) = tokio::sync::oneshot::channel();
    commands
        .send(super::ingest::RtmpControlCommand::PublishRequested {
            stream_key: "key".to_string(),
            client_ip: "127.0.0.1".to_string(),
            client_addr: "127.0.0.1:1".to_string(),
            reply,
        })
        .await
        .unwrap();
    let super::ingest::PublishAuthorization::Accepted(mut media) = response.await.unwrap() else {
        panic!("publish accepted");
    };

    // The owner side.
    let header = bytes::Bytes::from_static(&AVC_SEQUENCE_HEADER);
    let keyframe = bytes::Bytes::from(vec![0x17, 0x01, 0, 0, 0, 0, 0, 0, 1, 0x65]);
    let audio = bytes::Bytes::from(vec![0x2f, 0xc0]);
    let probe = media.on_video(header.clone(), 0);
    let Some(super::ingest_media::RtmpMediaEvent::Video(meta)) = probe else {
        panic!("the sequence header yields the video probe");
    };
    assert_eq!((meta.width, meta.height), (1920, 1080));
    assert!(media.on_video(keyframe.clone(), 10).is_none(), "probed once");
    let _ = media.on_audio(audio.clone(), 20);

    // Sequence headers are in the engine's cache without a Tokio round trip.
    let (cached_video, _) = engine
        .get_ingest_session_sequence_headers(&media.registration)
        .await;
    assert_eq!(cached_video.as_deref(), Some(header.as_ref()));

    // The probe goes to the control session as ingest metadata.
    commands
        .send(super::ingest::RtmpControlCommand::MediaProbe(
            super::ingest_media::RtmpMediaEvent::Video(meta),
        ))
        .await
        .unwrap();
    let (reply, response) = tokio::sync::oneshot::channel();
    commands
        .send(super::ingest::RtmpControlCommand::Finish {
            phase: "disconnect".to_string(),
            reason: "test complete".to_string(),
            had_error: false,
            reply,
        })
        .await
        .unwrap();
    response.await.unwrap();
    actor.await.unwrap();

    let ring = engine.get_or_create_pipeline("pipe-media-order").await;
    assert_eq!(ring.get_write_idx(), 3);
    let packets: Vec<_> = (0..3).map(|index| ring.read_at(index).unwrap()).collect();
    assert_eq!(packets[0].payload.as_ref(), header.as_ref());
    assert_eq!(packets[1].payload.as_ref(), keyframe.as_ref());
    assert!(packets[1].is_keyframe);
    assert_eq!(packets[2].media_type, crate::media::packet::MediaType::Audio);
    assert_eq!(packets[2].payload.as_ref(), audio.as_ref());
    assert_eq!(
        media.keyframe_times.lock().unwrap().as_slice(),
        &[packets[1].pts],
        "keyframe times recorded on the owner"
    );
}

/// A standby publisher caches its GOP on the owner; once the gate is armed
/// for promotion, the next complete GOP is replayed with the sequence headers
/// first, and its keyframe times are recorded.
#[test]
fn standby_publisher_promotes_with_headers_before_the_cached_gop() {
    use crate::media::input_gate::InputPacketGate;
    use crate::media::packet::MediaType;
    let gate = Arc::new(InputPacketGate::standby());
    let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(64));
    let mut media = super::ingest_media::RtmpPublisherMedia {
        registration: crate::media::engine::IngestRegistration {
            cancel_token: CancellationToken::new(),
            attempt_id: 1,
            input_id: "standby".to_string(),
            gate: gate.clone(),
            last_forwarded_dts: Arc::new(std::sync::atomic::AtomicI64::new(1_000)),
            preview_ring: Arc::new(arc_swap::ArcSwapOption::empty()),
        },
        ring: ring.clone(),
        bytes_received: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        ingest_metrics: Arc::new(crate::media::stage_metrics::StageMetrics::new()),
        last_progress_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        keyframe_times: Arc::new(std::sync::Mutex::new(Vec::new())),
        video_sequence_header: Arc::new(std::sync::Mutex::new(None)),
        audio_sequence_header: Arc::new(std::sync::Mutex::new(None)),
        timestamp_mapper: crate::media::input_gate::InputTimestampMapper::default(),
        standby_gop: crate::media::standby_gop::StandbyGopCache::default(),
        video_probed: false,
        audio_probed: false,
    };
    let keyframe = |ts: u8| bytes::Bytes::from(vec![0x17, 0x01, 0, 0, 0, 0, 0, 0, 1, 0x65, ts]);
    let inter = bytes::Bytes::from(vec![0x27, 0x01, 0, 0, 0, 0, 0, 0, 1, 0x41]);

    let _ = media.on_video(bytes::Bytes::from_static(&AVC_SEQUENCE_HEADER), 0);
    let _ = media.on_video(keyframe(1), 40);
    let _ = media.on_video(inter.clone(), 80);
    assert_eq!(ring.get_write_idx(), 0, "standby publishes nothing");

    gate.arm_for_promotion();
    let _ = media.on_video(inter, 120);

    let published: Vec<_> = (0..ring.get_write_idx())
        .map(|index| ring.read_at(index).unwrap())
        .collect();
    assert!(published.len() >= 3, "{} packets", published.len());
    assert_eq!(
        published[0].payload.as_ref(),
        &AVC_SEQUENCE_HEADER[..],
        "the sequence header precedes the replayed GOP"
    );
    let first_media = published.iter().position(|packet| packet.is_keyframe).unwrap();
    assert!(published[first_media].dts > published[0].dts);
    assert!(
        published
            .windows(2)
            .all(|pair| pair[0].dts <= pair[1].dts),
        "replayed in DTS order"
    );
    assert!(published.iter().all(|packet| packet.media_type == MediaType::Video));
    assert!(!media.keyframe_times.lock().unwrap().is_empty());
}

/// A playing client's commands must be handled while media flows: stopping
/// playback (closeStream) mid-stream detaches the server's ring reader
/// instead of waiting until the play loop ends on its own.
#[tokio::test]
async fn client_stop_during_playback_detaches_the_reader() {
    let pipeline_id = "pipe-play-client-stop";
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> =
        Arc::new(AcceptAllAuthenticator {
            pipeline_id: pipeline_id.to_string(),
        });
    let (engine, addr, server) = start_ingress_test_server(pipeline_access).await;
    let mut publisher = TcpStream::connect(addr).await.unwrap();
    assert!(drive_client_publish_handshake(&mut publisher, "any-key").await);
    let ring = engine.get_or_create_pipeline(pipeline_id).await;

    let feeding = CancellationToken::new();
    let feeder = {
        let ring = ring.clone();
        let feeding = feeding.clone();
        tokio::spawn(async move {
            let mut index = 0i64;
            while !feeding.is_cancelled() {
                let is_keyframe = index % 30 == 0;
                let mut payload = vec![0x55; 512];
                payload[0] = if is_keyframe { 0x17 } else { 0x27 };
                payload[1] = 1;
                let nalu_len = (payload.len() - 9) as u32;
                payload[5..9].copy_from_slice(&nalu_len.to_be_bytes());
                payload[9] = if is_keyframe { 0x65 } else { 0x41 };
                ring.push(crate::media::packet::MediaPacket {
                    media_type: crate::media::packet::MediaType::Video,
                    format: crate::media::packet::PayloadFormat::Flv,
                    is_keyframe,
                    track_index: 0,
                    pts: index * 33,
                    dts: index * 33,
                    payload: bytes::Bytes::from(payload),
                });
                index += 1;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    };

    let mut player = TcpStream::connect(addr).await.unwrap();
    let mut session = drive_client_play_handshake(&mut player, "any-key").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while ring.active_reader_count() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("playback reader attaches");
    // Media is flowing to the player.
    let mut sink = vec![0u8; 64 * 1024];
    let received = tokio::time::timeout(Duration::from_secs(5), player.read(&mut sink))
        .await
        .expect("media arrives")
        .unwrap();
    assert!(received > 0);

    for result in session.stop_playback().expect("stop playback") {
        if let ClientSessionResult::OutboundResponse(packet) = result {
            player.write_all(&packet.bytes).await.unwrap();
        }
    }
    // Keep draining so the server is never blocked on a full socket.
    let detached = tokio::time::timeout(Duration::from_secs(5), async {
        while ring.active_reader_count() > 0 {
            let _ = tokio::time::timeout(Duration::from_millis(20), player.read(&mut sink)).await;
        }
    })
    .await;
    assert!(
        detached.is_ok(),
        "closeStream during playback must detach the ring reader while media flows"
    );

    feeding.cancel();
    let _ = feeder.await;
    drop(player);
    drop(publisher);
    engine.shutdown_listeners();
    wait_for_ingest_cleanup(&engine, pipeline_id).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    join_test_owner_threads(&engine).await;
}

/// One RTMP chunk after the handshake: an AMF0 command message (type 20)
/// whose body is a single AMF0 string, i.e. fewer than the three values
/// (name, transaction id, command object) every command carries.
#[rustfmt::skip]
const SHORT_AMF0_COMMAND: [u8; 16] = [
    0x03,             // fmt 0, chunk stream 3
    0x00, 0x00, 0x00, // timestamp
    0x00, 0x00, 0x04, // message length
    0x14,             // AMF0 command
    0x00, 0x00, 0x00, 0x00, // message stream 0
    0x02, 0x00, 0x01, b'x', // AMF0 string "x"
];

/// Before the vendored rml_rtmp fix, this unauthenticated chunk panicked the
/// RTMP ingress owner thread inside `amf0_command::deserialize`; the owner's
/// drop guard then stopped the listener and Restream shut down. It must close
/// only the offending connection.
#[tokio::test]
async fn short_amf0_command_closes_only_that_connection() {
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> =
        Arc::new(AcceptAllAuthenticator {
            pipeline_id: "pipe-short-command".to_string(),
        });
    let (engine, addr, server) = start_ingress_test_server(pipeline_access).await;

    let mut attacker = TcpStream::connect(addr).await.unwrap();
    perform_client_handshake(&mut attacker, &CancellationToken::new())
        .await
        .expect("handshake");
    attacker.write_all(&SHORT_AMF0_COMMAND).await.unwrap();
    let mut buf = [0u8; 256];
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while matches!(attacker.read(&mut buf).await, Ok(n) if n > 0) {}
    })
    .await;
    assert!(closed.is_ok(), "the offending connection must be closed");

    // Bounded: with a dead owner the socket still accepts in the kernel
    // backlog, so an unbounded handshake would hang instead of failing.
    let published = tokio::time::timeout(Duration::from_secs(5), async {
        let mut publisher = TcpStream::connect(addr).await.unwrap();
        drive_client_publish_handshake(&mut publisher, "any-key").await
    })
    .await;
    assert!(
        matches!(published, Ok(true)),
        "the listener must keep serving publishers"
    );
    stop_ingress_test_server(&engine, server).await;
}

/// A message whose chunk stream later declares a length below the bytes
/// already received (fuzz: `rtmp_server_responses`). Before the vendored fix
/// this underflowed in rml_rtmp's chunk deserializer: a panic with overflow
/// checks, and in release a message that never completes and keeps growing.
#[rustfmt::skip]
fn shrinking_message_length_chunks() -> Vec<u8> {
    let mut bytes = vec![
        0x03, 0, 0, 0, 0, 0, 200, 0x09, 1, 0, 0, 0, // fmt 0: 200-byte video message
    ];
    bytes.extend([0xAB; 128]); // first 128-byte chunk
    bytes.extend([0x43, 0, 0, 0, 0, 0, 10, 0x09]); // fmt 1: length now 10
    bytes.extend([0xCD; 10]);
    bytes
}

#[tokio::test]
async fn shrinking_message_length_closes_only_that_connection() {
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> =
        Arc::new(AcceptAllAuthenticator {
            pipeline_id: "pipe-shrinking-length".to_string(),
        });
    let (engine, addr, server) = start_ingress_test_server(pipeline_access).await;

    let mut attacker = TcpStream::connect(addr).await.unwrap();
    perform_client_handshake(&mut attacker, &CancellationToken::new())
        .await
        .expect("handshake");
    attacker.write_all(&shrinking_message_length_chunks()).await.unwrap();
    let mut buf = [0u8; 256];
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while matches!(attacker.read(&mut buf).await, Ok(n) if n > 0) {}
    })
    .await;
    assert!(closed.is_ok(), "the offending connection must be closed");

    let published = tokio::time::timeout(Duration::from_secs(5), async {
        let mut publisher = TcpStream::connect(addr).await.unwrap();
        drive_client_publish_handshake(&mut publisher, "any-key").await
    })
    .await;
    assert!(
        matches!(published, Ok(true)),
        "the listener must keep serving publishers"
    );
    stop_ingress_test_server(&engine, server).await;
}

/// The same chunk from an RTMP destination server is a protocol error for
/// that output, not a panic on its egress shard.
#[test]
fn short_amf0_command_from_a_destination_is_a_protocol_error() {
    let parts = egress_transport::RtmpUrlParts {
        host: "127.0.0.1".to_string(),
        port: 1935,
        app: "live".to_string(),
        stream_key: "key".to_string(),
        tls: false,
    };
    let mut session = egress_connection::RtmpSessionCore::new(parts, 4096).unwrap();
    let _ = session.take_initial_packets();
    session.request_connection(false).unwrap();

    assert!(matches!(
        session.handle_server_input(&SHORT_AMF0_COMMAND),
        Err(egress_connection::RtmpSessionError::Protocol(_))
    ));
}
