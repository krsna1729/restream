// Panic containment on the RTMP owner threads (included by `rtmp/tests.rs`).

/// A panic while serving one RTMP connection closes that connection only:
/// the owner thread keeps accepting and serving publishers. Before the
/// per-connection boundary in `run_compio_owner`, the panic unwound the
/// owner, its drop guard stopped the listener, and bootstrap shut down.
#[tokio::test]
async fn a_panicking_connection_does_not_stop_the_listener() {
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> =
        Arc::new(AcceptAllAuthenticator {
            pipeline_id: "pipe-contained-panic".to_string(),
        });
    let (engine, addr, server) = start_ingress_test_server(pipeline_access).await;
    let before = crate::panic_boundary::contained_panics();

    let mut doomed = TcpStream::connect(addr).await.unwrap();
    super::ingest::injected_panics::mark(doomed.local_addr().unwrap());
    let _ = perform_client_handshake(&mut doomed, &CancellationToken::new()).await;
    let mut buf = [0u8; 256];
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while matches!(doomed.read(&mut buf).await, Ok(n) if n > 0) {}
    })
    .await;
    assert!(closed.is_ok(), "the panicking connection must be closed");
    // The owner closes the socket before it records the panic.
    let counted = tokio::time::timeout(Duration::from_secs(5), async {
        while crate::panic_boundary::contained_panics() == before {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(counted.is_ok(), "the contained panic is counted");

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

/// A panic on an owner thread outside the per-connection boundary is caught
/// at the thread entry: counted, the owner thread ends cleanly (no panic
/// escapes its join handle), and the listener task returns so its
/// supervisor restarts it.
#[tokio::test]
async fn an_owner_thread_panic_is_contained_and_stops_the_listener_cleanly() {
    let pipeline_access: Arc<dyn PipelineAccessAuthenticator> =
        Arc::new(AcceptAllAuthenticator {
            pipeline_id: "pipe-owner-panic".to_string(),
        });
    let (engine, addr, server) = start_ingress_test_server(pipeline_access).await;
    let before = crate::panic_boundary::contained_panics();

    let mut doomed = TcpStream::connect(addr).await.unwrap();
    perform_client_handshake(&mut doomed, &CancellationToken::new())
        .await
        .expect("handshake");
    super::ingest::injected_panics::mark_owner(doomed.local_addr().unwrap());
    drop(doomed);

    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the listener task returns once its owners stop")
        .expect("the listener task does not panic");
    // The thread records the panic after its runtime drops the listener's
    // channel end, so the count is settled once the thread has joined.
    join_test_owner_threads(&engine).await;
    assert!(
        crate::panic_boundary::contained_panics() > before,
        "the owner panic is counted"
    );
}

/// Accepts every publish; a play lookup unwinds, as a bug in the control
/// session would after the publisher registered.
struct PanicOnPlayAuthenticator;

impl PipelineAccessAuthenticator for PanicOnPlayAuthenticator {
    fn authenticate<'a>(
        &'a self,
        mode: PipelineAccessMode,
        stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            if mode == PipelineAccessMode::RtmpPlay {
                std::panic::resume_unwind(Box::new("injected RTMP control panic"));
            }
            Ok(AuthenticatedPipeline {
                id: "pipe-control-panic".to_string(),
                input_id: stream_key.to_string(),
                selected: true,
            })
        })
    }
}

/// Spawn a control session and ask it to authorize a publish on `key`.
async fn request_publish(
    engine: &Arc<MediaEngine>,
    key: &str,
) -> (
    tokio::sync::mpsc::Sender<super::ingest::RtmpControlCommand>,
    tokio::task::JoinHandle<()>,
    bool,
) {
    let (commands, receiver) = tokio::sync::mpsc::channel(1);
    let session = tokio::spawn(super::ingest::run_rtmp_control_session(
        receiver,
        CancellationToken::new(),
        Arc::new(PanicOnPlayAuthenticator),
        Arc::new(IngestSecurityService::new(IngestSecurityConfig::default())),
        engine.clone(),
    ));
    let (reply, response) = tokio::sync::oneshot::channel();
    commands
        .send(super::ingest::RtmpControlCommand::PublishRequested {
            stream_key: key.to_string(),
            client_ip: "127.0.0.1".to_string(),
            client_addr: "127.0.0.1:1".to_string(),
            reply,
        })
        .await
        .unwrap();
    let accepted = matches!(
        response.await.unwrap(),
        super::ingest::PublishAuthorization::Accepted(_)
    );
    (commands, session, accepted)
}

/// A publisher's control session that unwinds after registering must not
/// keep the stream key: the next publisher on that key is accepted.
#[tokio::test]
async fn a_panicking_control_session_frees_its_stream_key() {
    let engine = Arc::new(MediaEngine::new());
    let (commands, session, accepted) = request_publish(&engine, "key-control-panic").await;
    assert!(accepted, "the first publish is accepted");
    let (reply, _response) = tokio::sync::oneshot::channel();
    commands
        .send(super::ingest::RtmpControlCommand::PlayRequested {
            stream_key: "key-control-panic".to_string(),
            client_ip: "127.0.0.1".to_string(),
            reply,
        })
        .await
        .unwrap();
    let ended = session.await.expect_err("the control session unwinds");
    assert!(ended.is_panic());

    let republished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_commands, _session, accepted) = request_publish(&engine, "key-control-panic").await;
            if accepted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        republished.is_ok(),
        "a panicked control session left its stream key registered"
    );
}
