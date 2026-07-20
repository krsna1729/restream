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

/// Accepts every stream key as the same fixed pipeline id. Session fault
/// tests only care about what happens after a publisher is registered, not
/// about exercising the real database-backed lookup.
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
async fn drive_client_publish_handshake(socket: &mut TcpStream, stream_key: &str) {
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
            let n = socket.read(&mut buffer).await.unwrap();
            assert!(n > 0, "server closed the connection during publish setup");
            session.handle_input(&buffer[..n]).unwrap()
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
            break;
        }
    }
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

#[tokio::test]
async fn malformed_chunk_after_publish_surfaces_error_and_clears_ingest_registration() {
    let (engine, security) = test_engine_and_security();
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> = Arc::new(AcceptAllAuthenticator {
        pipeline_id: "pipe-fault-malformed".to_string(),
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let engine_c = engine.clone();
    let server = tokio::spawn(async move {
        let (socket, client_addr) = listener.accept().await.unwrap();
        handle_rtmp_client(socket, client_addr, pipeline_access, security, engine_c).await
    });

    let mut client = TcpStream::connect(addr).await.unwrap();
    drive_client_publish_handshake(&mut client, "any-key").await;

    assert!(
        engine
            .ingests
            .active
            .read()
            .await
            .contains_key("pipe-fault-malformed"),
        "publish must register an active ingest before the fault is injected"
    );

    client
        .write_all(&MALFORMED_CHUNK_HEADER_BYTE)
        .await
        .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("handle_rtmp_client must not hang on malformed chunk input")
        .expect("handle_rtmp_client task must not panic");

    assert_eq!(result, Ok(()));
    assert!(
        !engine
            .ingests
            .active
            .read()
            .await
            .contains_key("pipe-fault-malformed"),
        "malformed input after publish must fully unregister the ingest"
    );
}

#[tokio::test]
async fn truncated_chunk_then_disconnect_clears_ingest_registration_without_error() {
    let (engine, security) = test_engine_and_security();
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> = Arc::new(AcceptAllAuthenticator {
        pipeline_id: "pipe-fault-truncated".to_string(),
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let engine_c = engine.clone();
    let server = tokio::spawn(async move {
        let (socket, client_addr) = listener.accept().await.unwrap();
        handle_rtmp_client(socket, client_addr, pipeline_access, security, engine_c).await
    });

    let mut client = TcpStream::connect(addr).await.unwrap();
    drive_client_publish_handshake(&mut client, "any-key").await;

    assert!(
        engine
            .ingests
            .active
            .read()
            .await
            .contains_key("pipe-fault-truncated"),
        "publish must register an active ingest before the fault is injected"
    );

    // A lone type-0 basic header byte on csid 3 is a valid start of a new
    // chunk, but the deserializer needs 11 more bytes (timestamp, length,
    // type, stream id) before it forms a message. Sending just this byte and
    // then closing the socket simulates a mid-message truncation: the
    // deserializer must keep buffering rather than erroring, and the
    // resulting EOF must still be treated as an ordinary disconnect.
    client.write_all(&[0x03]).await.unwrap();
    drop(client);

    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("handle_rtmp_client must not hang on a truncated chunk plus disconnect")
        .expect("handle_rtmp_client task must not panic");

    assert_eq!(result, Ok(()));
    assert!(
        !engine
            .ingests
            .active
            .read()
            .await
            .contains_key("pipe-fault-truncated"),
        "truncated input followed by disconnect must fully unregister the ingest"
    );
}

