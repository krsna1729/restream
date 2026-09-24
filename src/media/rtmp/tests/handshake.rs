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

async fn drive_client_play_handshake(socket: &mut TcpStream, stream_key: &str) {
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
            return;
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
    let (engine, security) = test_engine_and_security();
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

#[tokio::test]
async fn bounded_media_handoff_preserves_video_audio_order() {
    let engine = Arc::new(MediaEngine::new());
    let security = Arc::new(IngestSecurityService::new(IngestSecurityConfig::default()));
    let (commands, receiver) = tokio::sync::mpsc::channel(1);
    let media_handoff = Arc::new(tokio::sync::Semaphore::new(1024));
    let actor_shutdown = CancellationToken::new();
    let actor = tokio::spawn(super::ingest::run_rtmp_control_session(
        receiver,
        actor_shutdown,
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
    assert!(matches!(
        response.await.unwrap(),
        super::ingest::PublishAuthorization::Accepted
    ));
    commands
        .send(super::ingest::RtmpControlCommand::PublishAccepted {
            client_ip: "127.0.0.1".to_string(),
        })
        .await
        .unwrap();
    let video = vec![0x17, 0x01, 0, 0, 0, 1, 0x65];
    let audio = vec![0x2f, 0xc0];
    let shutdown = CancellationToken::new();
    super::ingest::handoff_media_data(
        &commands,
        &media_handoff,
        &shutdown,
        crate::media::packet::MediaType::Video,
        bytes::Bytes::from(video.clone()),
        10,
    )
    .await
    .unwrap();
    super::ingest::handoff_media_data(
        &commands,
        &media_handoff,
        &shutdown,
        crate::media::packet::MediaType::Audio,
        bytes::Bytes::from(audio.clone()),
        20,
    )
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
    let mut reader = crate::media::ring_buffer::Reader::new("rtmp-media-order".to_string(), ring);
    let first = reader.pull().unwrap().unwrap();
    let second = reader.pull().unwrap().unwrap();
    assert_eq!(first.media_type, crate::media::packet::MediaType::Video);
    assert_eq!(first.payload.as_ref(), video.as_slice());
    assert_eq!(second.media_type, crate::media::packet::MediaType::Audio);
    assert_eq!(second.payload.as_ref(), audio.as_slice());
}

#[tokio::test]
async fn media_handoff_backpressures_and_releases_weighted_permits() {
    let (commands, mut receiver) = tokio::sync::mpsc::channel(2);
    let budget = Arc::new(tokio::sync::Semaphore::new(4));
    let shutdown = CancellationToken::new();
    super::ingest::handoff_media_data(
        &commands,
        &budget,
        &shutdown,
        crate::media::packet::MediaType::Video,
        bytes::Bytes::from(vec![1, 2, 3]),
        0,
    )
    .await
    .unwrap();
    assert_eq!(budget.available_permits(), 1);

    let mut second = Box::pin(super::ingest::handoff_media_data(
        &commands,
        &budget,
        &shutdown,
        crate::media::packet::MediaType::Audio,
        bytes::Bytes::from(vec![4, 5]),
        1,
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut second)
            .await
            .is_err(),
        "second payload must wait while the first lease is held"
    );
    drop(receiver.recv().await.unwrap());
    second.await.unwrap();
    assert_eq!(budget.available_permits(), 2);
    drop(receiver.recv().await.unwrap());
    assert_eq!(budget.available_permits(), 4);

    let (commands, mut receiver) = tokio::sync::mpsc::channel(1);
    let budget = Arc::new(tokio::sync::Semaphore::new(4));
    let send_shutdown = CancellationToken::new();
    super::ingest::handoff_media_data(
        &commands,
        &budget,
        &send_shutdown,
        crate::media::packet::MediaType::Video,
        bytes::Bytes::from(vec![1, 2, 3]),
        0,
    )
    .await
    .unwrap();
    let mut blocked_send = Box::pin(super::ingest::handoff_media_data(
        &commands,
        &budget,
        &send_shutdown,
        crate::media::packet::MediaType::Audio,
        bytes::Bytes::from(vec![4]),
        1,
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut blocked_send)
            .await
            .is_err(),
        "second command should hold a lease while bounded channel is full"
    );
    assert_eq!(budget.available_permits(), 0);
    send_shutdown.cancel();
    assert!(blocked_send.await.is_err());
    assert_eq!(budget.available_permits(), 1);
    drop(receiver.recv().await.unwrap());
    assert_eq!(budget.available_permits(), 4);

    let (commands, mut receiver) = tokio::sync::mpsc::channel(1);
    let budget = Arc::new(tokio::sync::Semaphore::new(4));
    let held_shutdown = CancellationToken::new();
    super::ingest::handoff_media_data(
        &commands,
        &budget,
        &held_shutdown,
        crate::media::packet::MediaType::Video,
        bytes::Bytes::from(vec![1, 2, 3, 4]),
        0,
    )
    .await
    .unwrap();
    let mut cancelled = Box::pin(super::ingest::handoff_media_data(
        &commands,
        &budget,
        &held_shutdown,
        crate::media::packet::MediaType::Audio,
        bytes::Bytes::from(vec![5]),
        1,
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut cancelled)
            .await
            .is_err(),
        "payload should be waiting on the weighted lease"
    );
    held_shutdown.cancel();
    assert!(cancelled.await.is_err());
    drop(receiver.recv().await.unwrap());
    assert_eq!(budget.available_permits(), 4);

    let (commands, receiver) = tokio::sync::mpsc::channel(1);
    let budget = Arc::new(tokio::sync::Semaphore::new(4));
    drop(receiver);
    assert!(
        super::ingest::handoff_media_data(
            &commands,
            &budget,
            &CancellationToken::new(),
            crate::media::packet::MediaType::Audio,
            bytes::Bytes::from(vec![9, 8]),
            2,
        )
        .await
        .is_err()
    );
    assert_eq!(budget.available_permits(), 4);
}
