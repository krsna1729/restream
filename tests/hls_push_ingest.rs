//! HLS push ingest end to end on a real HTTP server: FFmpeg pushes the
//! checked-in TS fixture over PUT and over POST, and a raw client carries a
//! whole stream over one persistent connection. Each run must publish the
//! whole fixture, in order, and end its session on `EXT-X-ENDLIST`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use restream::domain::ingest_security::IngestSecurityConfig;
use restream::media::engine::MediaEngine;
use restream::media::hls_ingest::{HlsIngest, HlsIngestLimits};
use restream::media::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessFuture,
    PipelineAccessMode,
};
use restream::media::packet::MediaType;
use restream::media::ring_buffer::RingBuffer;
use restream::media::security::IngestSecurityService;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const STREAM_KEY: &str = "hls-key";
const PIPELINE_ID: &str = "pipe-hls";
const INPUT_ID: &str = "input-hls";

/// Accepts only [`STREAM_KEY`], for HLS publish only.
struct OneKey;

impl PipelineAccessAuthenticator for OneKey {
    fn authenticate<'a>(
        &'a self,
        mode: PipelineAccessMode,
        stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            if mode == PipelineAccessMode::HlsPublish && stream_key == STREAM_KEY {
                Ok(AuthenticatedPipeline {
                    id: PIPELINE_ID.to_string(),
                    input_id: INPUT_ID.to_string(),
                    selected: true,
                })
            } else {
                Err(PipelineAccessError::InvalidStreamKey)
            }
        })
    }
}

struct Server {
    engine: Arc<MediaEngine>,
    address: SocketAddr,
    _ingest: Arc<HlsIngest>,
}

async fn serve() -> Server {
    let engine = Arc::new(MediaEngine::new());
    let ingest = HlsIngest::new(
        engine.clone(),
        Arc::new(OneKey),
        Arc::new(IngestSecurityService::new(IngestSecurityConfig::default())),
        HlsIngestLimits::default(),
    );
    let app = restream::api::create_hls_ingest_router(ingest.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(restream::api::http_server::serve(listener, app));
    Server {
        engine,
        address,
        _ingest: ingest,
    }
}

fn fixture_path() -> std::path::PathBuf {
    restream::test_fixtures::canonical_h264_ts_fixture().unwrap()
}

/// The session ended on its ENDLIST and the ring holds the whole fixture:
/// 8 s of 30 fps video with a keyframe every 2 s, all of it in order.
async fn assert_whole_fixture_published(server: &Server, ring: &RingBuffer, start: usize) {
    // Long enough for an 8 s fixture published at media pace.
    tokio::time::timeout(Duration::from_secs(30), async {
        while server.engine.input_has_session(PIPELINE_ID, INPUT_ID).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("HLS session unregistered");
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

/// FFmpeg runs with its default of one connection per object. With
/// `-http_persistent 1` it sends requests ahead of the replies and, on
/// exit, closes without reading them: the next reply that reaches the closed
/// socket draws an RST, hyper drops the connection on the failed reply
/// write, and the requests still queued behind it are never parsed
/// (captured: the final `index3.ts` and ENDLIST playlist lost). This server
/// cannot recover them, so an encoder that pipelines and exits without
/// reading the replies can lose its final objects; this test keeps that
/// encoder limit out of the server's bar, and the persistent path is
/// covered by `a_persistent_connection_carries_a_whole_stream`.
async fn ffmpeg_push_reaches_the_ring(method: &str) {
    let server = serve().await;
    let ring = server.engine.get_or_create_pipeline(PIPELINE_ID).await;
    let start = ring.get_write_idx();
    let url = format!(
        "http://{}/ingest/hls/{STREAM_KEY}/index.m3u8",
        server.address
    );
    let status = tokio::process::Command::new("/usr/bin/ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(fixture_path())
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
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    let status = tokio::time::timeout(Duration::from_secs(60), status)
        .await
        .expect("ffmpeg finished")
        .expect("ffmpeg ran");
    assert!(status.success(), "ffmpeg exited with {status}");
    assert_whole_fixture_published(&server, &ring, start).await;
}

#[tokio::test]
async fn ffmpeg_hls_put_with_a_connection_per_object_reaches_the_ring() {
    ffmpeg_push_reaches_the_ring("PUT").await;
}

#[tokio::test]
async fn ffmpeg_hls_post_with_a_connection_per_object_reaches_the_ring() {
    ffmpeg_push_reaches_the_ring("POST").await;
}

/// Send one PUT on `stream` and read its whole reply; returns the status.
async fn request(stream: &mut tokio::net::TcpStream, path: &str, body: &[u8]) -> u16 {
    let head = format!(
        "PUT /ingest/hls/{STREAM_KEY}/{path} HTTP/1.1\r\nHost: ingest\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut reply = Vec::new();
    while !reply.ends_with(b"\r\n\r\n") {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await.unwrap();
        reply.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&reply).to_ascii_lowercase();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map_or(0, |value| value.trim().parse().unwrap());
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await.unwrap();
    head[9..12].parse().unwrap()
}

/// One keep-alive connection that reads every reply, as a persistent
/// encoder does mid-stream, in FFmpeg's order: each segment first (held,
/// `202`), then the playlist that lists and releases it (`200`).
#[tokio::test]
async fn a_persistent_connection_carries_a_whole_stream() {
    let server = serve().await;
    let ring = server.engine.get_or_create_pipeline(PIPELINE_ID).await;
    let start = ring.get_write_idx();
    let fixture = std::fs::read(fixture_path()).unwrap();
    let half = fixture.len() / 188 / 2 * 188;
    let first = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nindex0.ts\n";
    let ended = format!("{first}#EXTINF:4,\nindex1.ts\n#EXT-X-ENDLIST\n");

    let mut stream = tokio::net::TcpStream::connect(server.address)
        .await
        .unwrap();
    assert_eq!(
        request(&mut stream, "index0.ts", &fixture[..half]).await,
        202
    );
    assert_eq!(
        request(&mut stream, "index.m3u8", first.as_bytes()).await,
        200
    );
    assert_eq!(
        request(&mut stream, "index1.ts", &fixture[half..]).await,
        202
    );
    assert_eq!(
        request(&mut stream, "index.m3u8", ended.as_bytes()).await,
        200
    );

    assert_whole_fixture_published(&server, &ring, start).await;
}
