//! HLS push ingest end to end on a real HTTP server: FFmpeg pushes the
//! checked-in TS fixture over PUT and over POST, as MPEG-TS and as fMP4
//! segments, and a raw client carries a whole stream over one persistent
//! connection. Each run must publish the whole fixture, in order, and end
//! its session on `EXT-X-ENDLIST`.

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

/// Which checked-in fixture a push carries, and the Annex B NAL headers of
/// the parameter sets its first keyframe must lead with.
#[derive(Clone, Copy)]
enum Codec {
    H264,
    Hevc,
}

impl Codec {
    fn fixture(self) -> std::path::PathBuf {
        match self {
            Self::H264 => fixture_path(),
            Self::Hevc => restream::test_fixtures::canonical_h265_ts_fixture().unwrap(),
        }
    }

    /// SPS and PPS for H.264; VPS, SPS and PPS for HEVC.
    fn parameter_set_headers(self) -> &'static [u8] {
        match self {
            Self::H264 => &[0x67, 0x68],
            Self::Hevc => &[0x40, 0x42, 0x44],
        }
    }
}

/// The session ended on its ENDLIST and the ring holds the whole fixture:
/// 8 s of 30 fps video with a keyframe every 2 s, all of it in order.
/// `annexb`: an fMP4 push, whose audio and Annex B conversion are checked too.
async fn assert_whole_fixture_published(
    server: &Server,
    ring: &RingBuffer,
    start: usize,
    annexb: Option<Codec>,
) {
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
    if let Some(codec) = annexb {
        let audio = packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Audio)
            .count();
        assert!(audio > 0, "the fMP4 push carried no audio");
        // The first keyframe is Annex B and carries the parameter sets the
        // TS path carries in band: FFmpeg's sample keeps the parameter sets
        // it muxed into `avcC`/`hvcC` (after the access unit delimiter), or
        // the conversion injects them when a keyframe lacks them.
        let first_keyframe = packets
            .iter()
            .find(|packet| packet.media_type == MediaType::Video && packet.is_keyframe)
            .expect("a keyframe reached the ring");
        let head = &first_keyframe.payload[..first_keyframe.payload.len().min(256)];
        let has = |nalu: u8| head.windows(5).any(|window| window == [0, 0, 0, 1, nalu]);
        assert!(
            head.starts_with(&[0, 0, 0, 1]),
            "the first keyframe is not Annex B: {:02x?}",
            head
        );
        assert!(
            codec.parameter_set_headers().iter().all(|&nalu| has(nalu)),
            "the first keyframe lacks a parameter set: {:02x?}",
            head
        );
    }
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
///
/// `segment_args` selects the segment container; the binary is the one
/// production resolves.
async fn ffmpeg_push_reaches_the_ring(
    method: &str,
    segment_args: &[&str],
    codec: Codec,
    annexb: bool,
) {
    let server = serve().await;
    let ring = server.engine.get_or_create_pipeline(PIPELINE_ID).await;
    let start = ring.get_write_idx();
    let url = format!(
        "http://{}/ingest/hls/{STREAM_KEY}/index.m3u8",
        server.address
    );
    let status = tokio::process::Command::new(restream::ffmpeg_binary::ffmpeg_bin_path())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(codec.fixture())
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
        .args(segment_args)
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
    assert_whole_fixture_published(&server, &ring, start, annexb.then_some(codec)).await;
}

#[tokio::test]
async fn ffmpeg_hls_put_with_a_connection_per_object_reaches_the_ring() {
    ffmpeg_push_reaches_the_ring("PUT", &[], Codec::H264, false).await;
}

#[tokio::test]
async fn ffmpeg_hls_post_with_a_connection_per_object_reaches_the_ring() {
    ffmpeg_push_reaches_the_ring("POST", &[], Codec::H264, false).await;
}

/// FFmpeg's fMP4 HLS: `init.mp4` (named by `EXT-X-MAP`) ahead of
/// `styp sidx moof mdat` segments, AAC as raw access units in the MP4.
#[tokio::test]
async fn ffmpeg_hls_fmp4_segments_reach_the_ring() {
    ffmpeg_push_reaches_the_ring(
        "PUT",
        &["-hls_segment_type", "fmp4", "-bsf:a", "aac_adtstoasc"],
        Codec::H264,
        true,
    )
    .await;
}

/// The same with HEVC: FFmpeg writes an `hvcC` init, and its VPS/SPS/PPS
/// lead the first keyframe as the TS path's in-band ones do.
#[tokio::test]
async fn ffmpeg_hls_fmp4_hevc_segments_reach_the_ring() {
    ffmpeg_push_reaches_the_ring(
        "PUT",
        &["-hls_segment_type", "fmp4", "-bsf:a", "aac_adtstoasc"],
        Codec::Hevc,
        true,
    )
    .await;
}

/// FFmpeg's separate-rendition fMP4 HLS (`-var_stream_map`), at media pace
/// as a live encoder pushes: a master whose variant is video only and whose
/// audio is an `EXT-X-MEDIA` rendition with its own playlist and `EXT-X-MAP`
/// init. FFmpeg writes the master only after every rendition's first
/// playlist, so what a rendition uploads before the master names it may be
/// lost (the first video segment, at most); everything after reaches the
/// ring, both renditions merged in DTS order.
#[tokio::test]
async fn ffmpeg_hls_with_a_separate_audio_rendition_publishes_both() {
    let server = serve().await;
    let ring = server.engine.get_or_create_pipeline(PIPELINE_ID).await;
    let start = ring.get_write_idx();
    let url = format!(
        "http://{}/ingest/hls/{STREAM_KEY}/stream_%v/index.m3u8",
        server.address
    );
    let status = tokio::process::Command::new(restream::ffmpeg_binary::ffmpeg_bin_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-re",
            "-i",
        ])
        .arg(Codec::H264.fixture())
        .args(["-map", "0:v:0", "-map", "0:a:0", "-c", "copy"])
        .args(["-bsf:a", "aac_adtstoasc", "-f", "hls", "-hls_time", "1"])
        .args(["-hls_list_size", "3", "-hls_segment_type", "fmp4"])
        .args([
            "-var_stream_map",
            "v:0,agroup:aud a:0,agroup:aud,default:yes",
        ])
        .args(["-master_pl_name", "master.m3u8"])
        .args(["-hls_flags", "delete_segments", "-method", "PUT"])
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
        "the session ends on both ENDLISTs"
    );

    let packets: Vec<_> = (start..ring.get_write_idx())
        .filter_map(|index| ring.read_at(index))
        .collect();
    let dts = |media_type: MediaType| -> Vec<i64> {
        packets
            .iter()
            .filter(|packet| packet.media_type == media_type)
            .map(|packet| packet.dts)
            .collect()
    };
    let (video, audio) = (dts(MediaType::Video), dts(MediaType::Audio));
    // 8 s of 30 fps video (its first 2 s segment precedes the master and is
    // lost) and of 48 kHz AAC (375 frames, all of them).
    assert!(video.len() >= 150, "only {} video packets", video.len());
    assert!(audio.len() >= 340, "only {} audio packets", audio.len());
    for track in [&video, &audio] {
        assert!(
            track.windows(2).all(|pair| pair[1] > pair[0]),
            "a track goes backwards"
        );
    }
    let keyframes = packets
        .iter()
        .filter(|packet| packet.media_type == MediaType::Video && packet.is_keyframe)
        .count();
    assert!(keyframes >= 3, "only {keyframes} keyframes");
    // Merged by DTS: no packet trails the newest one before it by more than
    // a frame or two.
    let mut newest = i64::MIN;
    for packet in &packets {
        assert!(
            packet.dts >= newest.saturating_sub(100),
            "{:?} at {} trails {newest}",
            packet.media_type,
            packet.dts
        );
        newest = newest.max(packet.dts);
    }
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

    assert_whole_fixture_published(&server, &ring, start, None).await;
}
