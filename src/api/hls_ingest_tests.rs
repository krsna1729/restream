//! The HLS push ingest route on a real HTTP server: the request contract,
//! then FFmpeg pushing a checked-in TS fixture over PUT and over POST.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt as _;
use reqwest::StatusCode;

use crate::media::engine::MediaEngine;
use crate::media::hls_ingest::test_support::{INPUT_ID, PIPELINE_ID, STREAM_KEY, test_ingest};
use crate::media::hls_ingest::{HlsIngest, HlsIngestLimits};
use crate::media::packet::MediaType;

struct Server {
    engine: Arc<MediaEngine>,
    base: String,
    _ingest: Arc<HlsIngest>,
}

async fn serve(limits: HlsIngestLimits) -> Server {
    let (engine, ingest) = test_ingest(limits);
    let app = crate::api::create_hls_ingest_router(ingest.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(crate::api::http_server::serve(listener, app));
    Server {
        engine,
        base: format!("http://{address}/ingest/hls"),
        _ingest: ingest,
    }
}

fn small_limits() -> HlsIngestLimits {
    HlsIngestLimits {
        max_playlist_bytes: 256,
        max_segment_bytes: 1_024,
        total_buffer_bytes: 1_500,
        ..HlsIngestLimits::default()
    }
}

fn url(server: &Server, key: &str, path: &str) -> String {
    format!("{}/{key}/{path}", server.base)
}

async fn put(
    server: &Server,
    key: &str,
    path: &str,
    body: impl Into<reqwest::Body>,
) -> reqwest::Response {
    reqwest::Client::new()
        .put(url(server, key, path))
        .body(body)
        .send()
        .await
        .unwrap()
}

async fn wait_until_unregistered(engine: &MediaEngine) {
    // Long enough for an 8 s fixture published at media pace.
    tokio::time::timeout(Duration::from_secs(30), async {
        while engine.input_has_session(PIPELINE_ID, INPUT_ID).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("HLS session unregistered");
}

#[tokio::test]
async fn a_wrong_key_is_refused_before_any_body_byte_is_read() {
    let server = serve(HlsIngestLimits::default()).await;
    // A body that never yields: answering proves it was never awaited.
    let endless = reqwest::Body::wrap_stream(futures_util::stream::pending::<
        Result<Bytes, std::io::Error>,
    >());
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        reqwest::Client::new()
            .put(url(&server, "wrong-key", "index0.ts"))
            .body(endless)
            .send(),
    )
    .await
    .expect("answered without reading the body")
    .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let delete = reqwest::Client::new()
        .delete(url(&server, "wrong-key", "index0.ts"))
        .send()
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::FORBIDDEN);
    assert!(!server.engine.input_has_session(PIPELINE_ID, INPUT_ID).await);
}

#[tokio::test]
async fn methods_other_than_put_post_and_delete_are_not_allowed() {
    let server = serve(HlsIngestLimits::default()).await;
    let response = reqwest::get(url(&server, STREAM_KEY, "index.m3u8"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "PUT, POST, DELETE");

    let delete = reqwest::Client::new()
        .delete(url(&server, STREAM_KEY, "index0.ts"))
        .send()
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::OK);
    assert!(!server.engine.input_has_session(PIPELINE_ID, INPUT_ID).await);
}

#[tokio::test]
async fn bodies_over_their_bound_are_refused_by_length_or_while_streaming() {
    let server = serve(small_limits()).await;
    let declared = put(&server, STREAM_KEY, "index.m3u8", vec![b'#'; 257]).await;
    assert_eq!(declared.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let chunks = futures_util::stream::iter(
        (0..4).map(|_| Ok::<_, std::io::Error>(Bytes::from(vec![0x47u8; 400]))),
    );
    let chunked = put(
        &server,
        STREAM_KEY,
        "index0.ts",
        reqwest::Body::wrap_stream(chunks),
    )
    .await;
    assert_eq!(chunked.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(!server.engine.input_has_session(PIPELINE_ID, INPUT_ID).await);
}

#[tokio::test]
async fn unsupported_objects_and_paths_are_refused() {
    let server = serve(HlsIngestLimits::default()).await;
    let fmp4 = put(&server, STREAM_KEY, "init.mp4", vec![0u8; 8]).await;
    assert_eq!(fmp4.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let backslash = put(&server, STREAM_KEY, "a%5Cb.ts", vec![0u8; 8]).await;
    assert_eq!(backslash.status(), StatusCode::BAD_REQUEST);
    let malformed = put(&server, STREAM_KEY, "index.m3u8", "not a playlist").await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    server.engine.cancel_pipeline_input(INPUT_ID).await;
}

#[tokio::test]
async fn an_early_segment_is_answered_202_at_once_and_idle_ends_the_session() {
    let server = serve(HlsIngestLimits {
        idle_floor: Duration::from_millis(300),
        ..HlsIngestLimits::default()
    })
    .await;
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        put(&server, STREAM_KEY, "index0.ts", vec![0x47u8; 188]),
    )
    .await
    .expect("a held segment never waits for its playlist");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(server.engine.input_has_session(PIPELINE_ID, INPUT_ID).await);

    wait_until_unregistered(&server.engine).await;
    let restarted = put(&server, STREAM_KEY, "index1.ts", vec![0x47u8; 188]).await;
    assert_eq!(
        restarted.status(),
        StatusCode::ACCEPTED,
        "a later object starts a new session"
    );
    server.engine.cancel_pipeline_input(INPUT_ID).await;
}

#[tokio::test]
async fn an_input_held_by_another_publisher_is_a_conflict() {
    let server = serve(HlsIngestLimits::default()).await;
    let rtmp = server
        .engine
        .try_register_pipeline_input_attempt(PIPELINE_ID, INPUT_ID, STREAM_KEY, "rtmp", true)
        .await
        .expect("RTMP publisher registers");
    let response = put(&server, STREAM_KEY, "index0.ts", vec![0x47u8; 188]).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    server
        .engine
        .unregister_ingest_if_current(PIPELINE_ID, &rtmp)
        .await;
}

#[tokio::test]
async fn the_global_buffer_budget_refuses_with_retry_after_then_recovers() {
    let server = serve(small_limits()).await;
    let held = put(&server, STREAM_KEY, "index0.ts", vec![0x47u8; 1_000]).await;
    assert_eq!(held.status(), StatusCode::ACCEPTED);

    let refused = put(&server, STREAM_KEY, "index1.ts", vec![0x47u8; 1_000]).await;
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(refused.headers()["retry-after"], "1");

    // Listing index0 publishes it (right after the reply, between objects),
    // which returns its bytes to the budget.
    let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nindex0.ts\n";
    assert_eq!(
        put(&server, STREAM_KEY, "index.m3u8", playlist)
            .await
            .status(),
        StatusCode::OK
    );
    let mut recovered = put(&server, STREAM_KEY, "index1.ts", vec![0x47u8; 1_000]).await;
    for _ in 0..50 {
        if recovered.status() != StatusCode::SERVICE_UNAVAILABLE {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        recovered = put(&server, STREAM_KEY, "index1.ts", vec![0x47u8; 1_000]).await;
    }
    assert_eq!(recovered.status(), StatusCode::ACCEPTED);
    server.engine.cancel_pipeline_input(INPUT_ID).await;
}

#[tokio::test]
async fn a_stalled_body_times_out_with_408_and_returns_its_budget() {
    let server = serve(HlsIngestLimits {
        idle_floor: Duration::from_millis(300),
        total_buffer_bytes: 1_000,
        ..HlsIngestLimits::default()
    })
    .await;
    let stalled = futures_util::stream::once(async {
        Ok::<_, std::io::Error>(Bytes::from(vec![0x47u8; 800]))
    })
    .chain(futures_util::stream::pending());
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        put(
            &server,
            STREAM_KEY,
            "index0.ts",
            reqwest::Body::wrap_stream(stalled),
        ),
    )
    .await
    .expect("a stalled body is answered");
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);

    // The 800 bytes read before the stall are back in the budget.
    let next = put(&server, STREAM_KEY, "index0.ts", vec![0x47u8; 900]).await;
    assert_eq!(next.status(), StatusCode::ACCEPTED);
    server.engine.cancel_pipeline_input(INPUT_ID).await;
}

/// FFmpeg pushes the checked-in TS fixture as HLS; media must reach the
/// pipeline ring, and the session must end on its ENDLIST.
///
/// FFmpeg runs with its default of one connection per object. With
/// `-http_persistent 1` it sends requests ahead of the replies and, on
/// exit, closes without reading them: the next reply that reaches the closed
/// socket draws an RST, and the server's kernel discards the requests still
/// unread in its receive queue (captured: the final `index3.ts` and ENDLIST
/// playlist lost). No server can recover those bytes, so an encoder that
/// pipelines and exits without reading the replies can lose its final
/// objects; this test keeps that encoder limit out of the server's bar.
async fn ffmpeg_push_reaches_the_ring(method: &str) {
    let server = serve(HlsIngestLimits::default()).await;
    let ring = server.engine.get_or_create_pipeline(PIPELINE_ID).await;
    let start = ring.get_write_idx();
    let fixture = crate::test_fixtures::canonical_h264_ts_fixture().unwrap();
    let status = tokio::process::Command::new("/usr/bin/ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(&fixture)
        .args([
            "-c",
            "copy",
            "-f",
            "hls",
            "-hls_time",
            "1",
            "-hls_list_size",
            "3",
        ])
        .args(["-hls_flags", "delete_segments", "-method", method])
        .args(["-http_persistent", "0"])
        .arg(url(&server, STREAM_KEY, "index.m3u8"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    let status = tokio::time::timeout(Duration::from_secs(60), status)
        .await
        .expect("ffmpeg finished")
        .unwrap();
    assert!(status.success(), "ffmpeg {method} push failed: {status}");
    wait_until_unregistered(&server.engine).await;
    let phase = server
        .engine
        .recent_ingest_outcome(PIPELINE_ID)
        .await
        .and_then(|outcome| outcome.failure_phase);
    assert_eq!(
        phase.as_deref(),
        Some("eof"),
        "the session ends on EXT-X-ENDLIST"
    );

    let packets: Vec<_> = (start..ring.get_write_idx())
        .filter_map(|index| ring.read_at(index))
        .collect();
    let video_dts: Vec<i64> = packets
        .iter()
        .filter(|packet| packet.media_type == MediaType::Video)
        .map(|packet| packet.dts)
        .collect();
    let keyframes = packets
        .iter()
        .filter(|packet| packet.media_type == MediaType::Video && packet.is_keyframe)
        .count();
    // 8 s of 30 fps video with a keyframe every 2 s, all of it in order.
    assert!(
        video_dts.len() > 200,
        "only {} video packets reached the ring",
        video_dts.len()
    );
    assert!(
        keyframes >= 3,
        "only {keyframes} keyframes reached the ring"
    );
    assert!(
        video_dts.windows(2).all(|pair| pair[1] > pair[0]),
        "video DTS leaves the ring in order"
    );
}

#[tokio::test]
async fn ffmpeg_hls_put_with_a_connection_per_object_reaches_the_ring() {
    ffmpeg_push_reaches_the_ring("PUT").await;
}

#[tokio::test]
async fn ffmpeg_hls_post_with_a_connection_per_object_reaches_the_ring() {
    ffmpeg_push_reaches_the_ring("POST").await;
}
