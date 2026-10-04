// RTMP admission isolation (docs/isolation-audit.md F1, F2): one client
// cannot take every connection slot, and a client that never publishes
// cannot hold a slot past the admission deadline. Included into the RTMP
// test module.

fn engine_with_admission_limits(per_client: usize, preauth_ms: u64) -> Arc<MediaEngine> {
    let config = crate::AppConfig {
        rtmp_max_connections_per_ip: per_client,
        rtmp_preauth_timeout_ms: preauth_ms,
        ..crate::AppConfig::default()
    };
    Arc::new(MediaEngine::new_with_config(Arc::new(config)))
}

/// A TCP connection to `server` from the loopback address `from`
/// (127.0.0.0/8 is all local on Linux, so each is a distinct client).
async fn connect_from(from: &str, server: std::net::SocketAddr) -> TcpStream {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket
        .bind(format!("{from}:0").parse().unwrap())
        .unwrap();
    socket
        .connect(std::net::SocketAddr::from(([127, 0, 0, 1], server.port())))
        .await
        .unwrap()
}

async fn handshake_succeeds(stream: &mut TcpStream) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(5),
            perform_client_handshake(stream, &CancellationToken::new()),
        )
        .await,
        Ok(Ok(_))
    )
}

/// Resolves when the peer closes `stream` (EOF or error), or times out.
async fn closed_within(stream: &mut TcpStream, limit: Duration) -> bool {
    let mut buf = [0u8; 4096];
    tokio::time::timeout(limit, async {
        while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn one_client_cannot_take_every_connection_slot() {
    let engine = engine_with_admission_limits(2, 60_000);
    let (engine, addr, server) =
        start_ingress_test_server_with_engine(Arc::new(PipelinePerKeyAuthenticator), engine)
            .await;

    let mut first = connect_from("127.0.0.2", addr).await;
    let mut second = connect_from("127.0.0.2", addr).await;
    assert!(handshake_succeeds(&mut first).await);
    assert!(handshake_succeeds(&mut second).await);
    let mut over = connect_from("127.0.0.2", addr).await;
    assert!(
        !handshake_succeeds(&mut over).await,
        "a client over its share is refused"
    );
    let mut other = connect_from("127.0.0.3", addr).await;
    assert!(
        handshake_succeeds(&mut other).await,
        "another client is still admitted"
    );

    // Closing a connection gives the slot back.
    drop(first);
    let reused = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut again = connect_from("127.0.0.2", addr).await;
            if handshake_succeeds(&mut again).await {
                return again;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(reused.is_ok(), "the released slot is usable again");
    drop((second, over, other, reused));
    stop_ingress_test_server(&engine, server).await;
}

#[tokio::test]
async fn a_client_that_never_publishes_is_closed_at_the_admission_deadline() {
    let engine = engine_with_admission_limits(64, 300);
    let (engine, addr, server) =
        start_ingress_test_server_with_engine(Arc::new(PipelinePerKeyAuthenticator), engine)
            .await;

    let mut idle = TcpStream::connect(addr).await.unwrap();
    assert!(handshake_succeeds(&mut idle).await);
    assert!(
        closed_within(&mut idle, Duration::from_secs(3)).await,
        "an idle client is closed at the deadline"
    );

    let mut publisher = TcpStream::connect(addr).await.unwrap();
    assert!(drive_client_publish_handshake(&mut publisher, "deadline-key").await);
    assert!(
        !closed_within(&mut publisher, Duration::from_millis(900)).await,
        "a publishing client is not closed by the deadline"
    );
    drop(publisher);
    stop_ingress_test_server(&engine, server).await;
}
