//! The HLS push ingest route's request contract on a real HTTP server.
//! FFmpeg end-to-end pushes live in `tests/hls_push_ingest.rs`: API modules
//! never start FFmpeg.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt as _;
use reqwest::StatusCode;

use crate::media::engine::MediaEngine;
use crate::media::hls_ingest::test_support::{
    INPUT_ID, PIPELINE_ID, STREAM_KEY, fmp4_video_objects, test_ingest,
};
use crate::media::hls_ingest::{HlsIngest, HlsIngestLimits};

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
    let matroska = put(&server, STREAM_KEY, "index0.mkv", vec![0u8; 8]).await;
    assert_eq!(matroska.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let backslash = put(&server, STREAM_KEY, "a%5Cb.ts", vec![0u8; 8]).await;
    assert_eq!(backslash.status(), StatusCode::BAD_REQUEST);
    let malformed = put(&server, STREAM_KEY, "index.m3u8", "not a playlist").await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    server.engine.cancel_pipeline_input(INPUT_ID).await;
}

/// One top-level box: a 32-bit size, a type, then `payload_len` zeros.
fn mp4_box(kind: &[u8; 4], payload_len: usize) -> Vec<u8> {
    let size = u32::try_from(payload_len + 8).unwrap();
    let mut bytes = size.to_be_bytes().to_vec();
    bytes.extend_from_slice(kind);
    bytes.resize(payload_len + 8, 0);
    bytes
}

#[tokio::test]
async fn fmp4_objects_are_checked_on_arrival_and_cannot_join_a_ts_stream() {
    let server = serve(HlsIngestLimits {
        max_init_bytes: 64,
        ..HlsIngestLimits::default()
    })
    .await;
    // A box walk that finds no fragment is malformed.
    let not_fmp4 = put(&server, STREAM_KEY, "index0.m4s", mp4_box(b"free", 8)).await;
    assert_eq!(not_fmp4.status(), StatusCode::BAD_REQUEST);
    // An init (a top-level `moov`) over its own bound.
    let oversized = put(&server, STREAM_KEY, "init.mp4", mp4_box(b"moov", 100)).await;
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    // The first accepted media object fixes MPEG-TS; fMP4 cannot join.
    let ts = put(&server, STREAM_KEY, "index0.ts", vec![0x47u8; 188]).await;
    assert_eq!(ts.status(), StatusCode::ACCEPTED);
    let mixed = put(&server, STREAM_KEY, "index1.m4s", mp4_box(b"free", 8)).await;
    assert_eq!(mixed.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    server.engine.cancel_pipeline_input(INPUT_ID).await;
}

/// fMP4 media needs the playlist's `EXT-X-MAP`: under a playlist that names
/// none it is refused on arrival, not held until it ages out.
#[tokio::test]
async fn fmp4_media_under_a_playlist_without_a_map_is_refused() {
    let server = serve(HlsIngestLimits::default()).await;
    let (_, segments) = fmp4_video_objects(1, 2);
    let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:0.1,\nindex0.m4s\n";
    let applied = put(&server, STREAM_KEY, "index.m3u8", playlist).await;
    assert_eq!(applied.status(), StatusCode::OK);

    let refused = put(&server, STREAM_KEY, "index0.m4s", segments[0].clone()).await;
    assert_eq!(refused.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
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

/// A body that keeps every frame inside the idle floor but crawls still
/// ends at the whole-body deadline (four idle floors before a playlist).
#[tokio::test]
async fn a_trickling_body_hits_the_whole_body_deadline() {
    let server = serve(HlsIngestLimits {
        idle_floor: Duration::from_millis(300),
        ..HlsIngestLimits::default()
    })
    .await;
    let trickle = futures_util::stream::unfold((), |()| async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Some((Ok::<_, std::io::Error>(Bytes::from_static(&[0x47])), ()))
    });
    let started = std::time::Instant::now();
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        put(
            &server,
            STREAM_KEY,
            "index0.ts",
            reqwest::Body::wrap_stream(trickle),
        ),
    )
    .await
    .expect("a trickling body is answered");
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert!(
        started.elapsed() >= Duration::from_millis(1_100),
        "answered after {:?}, before the 1.2 s body deadline",
        started.elapsed()
    );
}
