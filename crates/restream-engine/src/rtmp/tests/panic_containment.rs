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
