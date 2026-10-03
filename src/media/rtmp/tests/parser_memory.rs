// WI5B.1 ingest parser memory: a publisher may not make the RTMP owner buffer
// an oversized declared message, and all parsers together stay within the
// aggregate parser budget. Included into the RTMP test module.

/// Accepts every stream key as its own pipeline, so several publishers can be
/// live at once.
struct PipelinePerKeyAuthenticator;

impl PipelineAccessAuthenticator for PipelinePerKeyAuthenticator {
    fn authenticate<'a>(
        &'a self,
        _mode: PipelineAccessMode,
        stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            Ok(AuthenticatedPipeline {
                id: stream_key.to_string(),
                input_id: stream_key.to_string(),
                selected: true,
            })
        })
    }
}

fn engine_with_ingest_limits(max_message: usize, budget: usize) -> Arc<MediaEngine> {
    let config = crate::AppConfig {
        rtmp_max_message_bytes: max_message,
        rtmp_ingest_parser_budget_bytes: budget,
        ..crate::AppConfig::default()
    };
    Arc::new(MediaEngine::new_with_config(Arc::new(config)))
}

/// A video message on csid 6 declaring `declared` bytes, of which `sent`
/// follow: a type 0 chunk, then type 3 continuation chunks at the client's
/// announced chunk size.
fn declared_video_message(declared: u32, sent: usize) -> Vec<u8> {
    let chunk_size = ClientSessionConfig::new().chunk_size as usize;
    let mut bytes = vec![0x06, 0, 0, 0];
    bytes.extend_from_slice(&declared.to_be_bytes()[1..]);
    bytes.push(9);
    bytes.extend_from_slice(&1u32.to_le_bytes());
    let mut remaining = sent;
    let mut first = true;
    while remaining > 0 {
        if !first {
            bytes.push(0xC6);
        }
        let take = remaining.min(chunk_size);
        bytes.extend(std::iter::repeat_n(0x17, take));
        remaining -= take;
        first = false;
    }
    bytes
}

async fn ingest_registered(engine: &MediaEngine, pipeline_id: &str) -> bool {
    engine.ingests.active.read().await.contains_key(pipeline_id)
}

#[tokio::test]
async fn oversized_declared_message_rejects_the_publisher() {
    let engine = engine_with_ingest_limits(64 * 1024, 1024 * 1024);
    let (engine, addr, server) =
        start_ingress_test_server_with_engine(Arc::new(PipelinePerKeyAuthenticator), engine)
            .await;

    let mut client = TcpStream::connect(addr).await.unwrap();
    assert!(drive_client_publish_handshake(&mut client, "oversized").await);
    assert!(ingest_registered(&engine, "oversized").await);
    client
        .write_all(&declared_video_message(1024 * 1024, 128))
        .await
        .unwrap();
    wait_for_ingest_cleanup(&engine, "oversized").await;

    let mut probe = [0u8; 64];
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match client.read(&mut probe).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the owner must close the rejected connection");
    stop_ingress_test_server(&engine, server).await;
}

#[tokio::test]
async fn aggregate_parser_budget_rejects_the_connection_that_exceeds_it() {
    const PARTIAL: usize = 20 * 1024;
    let engine = engine_with_ingest_limits(64 * 1024, 64 * 1024);
    let (engine, addr, server) =
        start_ingress_test_server_with_engine(Arc::new(PipelinePerKeyAuthenticator), engine)
            .await;

    let mut clients = Vec::new();
    for index in 0..4 {
        let key = format!("budget-{index}");
        let mut client = TcpStream::connect(addr).await.unwrap();
        assert!(drive_client_publish_handshake(&mut client, &key).await);
        // Each message is below the per-message maximum and left incomplete,
        // so only the aggregate budget can refuse it.
        client
            .write_all(&declared_video_message(60 * 1024, PARTIAL))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        clients.push((key, client));
    }

    // Three partial messages (60 KiB held) fit the 64 KiB budget; the fourth
    // pushes the aggregate over it and only that publisher is refused.
    wait_for_ingest_cleanup(&engine, "budget-3").await;
    for (key, _) in &clients[..3] {
        assert!(
            ingest_registered(&engine, key).await,
            "{key} stayed within the budget and must remain connected"
        );
    }

    drop(clients);
    stop_ingress_test_server(&engine, server).await;
}
