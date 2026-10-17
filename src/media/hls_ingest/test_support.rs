//! Test doubles for HLS push ingest: one accepted stream key, a real engine.

use std::sync::Arc;

use super::{HlsIngest, HlsIngestLimits};
use crate::domain::ingest_security::IngestSecurityConfig;
use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessFuture,
    PipelineAccessMode,
};
use crate::media::security::IngestSecurityService;

pub(crate) const STREAM_KEY: &str = "hls-key";
pub(crate) const PIPELINE_ID: &str = "pipe-hls";
pub(crate) const INPUT_ID: &str = "input-hls";

/// Accepts only [`STREAM_KEY`], for HLS publish only.
pub(crate) struct OneKey;

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

/// Accepts [`STREAM_KEY`] until [`RevocableKey::revoke`] is called, so a test
/// can authorize an upload and then change the input's row before the body it
/// was authorized for arrives.
pub(crate) struct RevocableKey {
    revoked: std::sync::atomic::AtomicBool,
    /// Every client address the authenticator was asked about, in order, so a
    /// test can check the key a caller passes: the security layer bans and rate
    /// limits by bare IP.
    seen_client_ips: std::sync::Mutex<Vec<String>>,
}

impl RevocableKey {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            revoked: std::sync::atomic::AtomicBool::new(false),
            seen_client_ips: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// The row changed: the key no longer resolves.
    pub(crate) fn revoke(&self) {
        self.revoked
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn seen_client_ips(&self) -> Vec<String> {
        crate::sync::lock(&self.seen_client_ips).clone()
    }
}

impl PipelineAccessAuthenticator for RevocableKey {
    fn authenticate<'a>(
        &'a self,
        mode: PipelineAccessMode,
        stream_key: &'a str,
        client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            crate::sync::lock(&self.seen_client_ips).push(client_ip.to_string());
            if mode == PipelineAccessMode::HlsPublish
                && stream_key == STREAM_KEY
                && !self.revoked.load(std::sync::atomic::Ordering::Relaxed)
            {
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

/// Accepts [`STREAM_KEY`] for the primary input (as [`OneKey`] does) and
/// `key-<index>` for `pipe-<index>`/`input-<index>`, so a test can publish
/// from several inputs at once. A session is opened only for a key that still
/// resolves to the input the caller authorized, so a test that dispatches for
/// a second input needs a key that resolves to it.
pub(crate) struct IndexedKeys;

impl PipelineAccessAuthenticator for IndexedKeys {
    fn authenticate<'a>(
        &'a self,
        mode: PipelineAccessMode,
        stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            if mode != PipelineAccessMode::HlsPublish {
                return Err(PipelineAccessError::InvalidStreamKey);
            }
            if stream_key == STREAM_KEY {
                return Ok(AuthenticatedPipeline {
                    id: PIPELINE_ID.to_string(),
                    input_id: INPUT_ID.to_string(),
                    selected: true,
                });
            }
            match stream_key
                .strip_prefix("key-")
                .and_then(|index| index.parse::<u64>().ok())
            {
                Some(index) => Ok(AuthenticatedPipeline {
                    id: format!("pipe-{index}"),
                    input_id: format!("input-{index}"),
                    selected: true,
                }),
                None => Err(PipelineAccessError::InvalidStreamKey),
            }
        })
    }
}

pub(crate) fn test_ingest(limits: HlsIngestLimits) -> (Arc<MediaEngine>, Arc<HlsIngest>) {
    test_ingest_with(limits, Arc::new(OneKey))
}

/// The same ingest with the caller's own access double.
pub(crate) fn test_ingest_with(
    limits: HlsIngestLimits,
    access: Arc<dyn PipelineAccessAuthenticator>,
) -> (Arc<MediaEngine>, Arc<HlsIngest>) {
    let engine = Arc::new(MediaEngine::new());
    let ingest = HlsIngest::new(
        engine.clone(),
        access,
        Arc::new(IngestSecurityService::new(IngestSecurityConfig::default())),
        limits,
    );
    (engine, ingest)
}

pub(crate) fn authenticated() -> AuthenticatedPipeline {
    AuthenticatedPipeline {
        id: PIPELINE_ID.to_string(),
        input_id: INPUT_ID.to_string(),
        selected: true,
    }
}

/// A listener that accepts connections and never answers, so a pull task
/// parks in its first request. A closed port would refuse the connection and
/// the puller would log a warning on every backoff, and passing test logs
/// stay quiet (`cargo xtask test-hygiene`).
///
/// The caller aborts the returned task when the test ends.
pub(crate) fn stalled_origin() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a stalled origin");
    let addr = listener
        .local_addr()
        .expect("read the stalled origin's port");
    listener
        .set_nonblocking(true)
        .expect("set the stalled origin non-blocking");
    let handle = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).expect("adopt the listener");
        // Held, never read: the socket stays open so the request hangs.
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    (addr, handle)
}

/// The first keyframe of a canonical fixture, as Annex B: it carries the
/// parameter sets a sample entry is built from.
fn fixture_keyframe(codec: &str) -> Vec<u8> {
    let (_, _, packets) = crate::test_fixtures::primary_av_packets_for_codec(codec)
        .unwrap_or_else(|error| panic!("the canonical {codec} fixture demuxes: {error}"));
    packets
        .into_iter()
        .find(|packet| {
            packet.media_type == crate::media::packet::MediaType::Video && packet.is_keyframe
        })
        .expect("the fixture has a keyframe")
        .payload
        .to_vec()
}

/// One fMP4 media segment, as FFmpeg's fMP4 HLS muxer writes it: `moof` +
/// `mdat` with a `tfdt`, `frames` samples of which the first is a keyframe.
fn fmp4_segment(
    muxer: &mut shiguredo_mp4::mux::Fmp4SegmentMuxer,
    sample_entry: &shiguredo_mp4::boxes::SampleEntry,
    frames: usize,
) -> Vec<u8> {
    use shiguredo_mp4::TrackKind;
    use shiguredo_mp4::mux::Sample;

    const TIMESCALE: u32 = 90_000;
    const FRAME_TICKS: u32 = 3_000;
    const FRAME: [u8; 4] = [0x41, 0x9A, 0x02, 0x33];
    const IDR: [u8; 4] = [0x65, 0x88, 0x84, 0x21];

    // Every sample is one NAL unit behind a 4-byte length, as `avcC` says.
    let payload = |nalu: &[u8]| {
        let mut bytes = u32::try_from(nalu.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend_from_slice(nalu);
        bytes
    };
    let samples: Vec<(Vec<u8>, Sample)> = (0..frames)
        .map(|index| {
            let data = payload(if index == 0 { &IDR } else { &FRAME });
            let sample = Sample {
                track_kind: TrackKind::Video,
                sample_entry: Some(sample_entry.clone()),
                keyframe: index == 0,
                timescale: std::num::NonZeroU32::new(TIMESCALE).unwrap(),
                duration: FRAME_TICKS,
                composition_time_offset: None,
                data_offset: u64::try_from(index * data.len()).unwrap(),
                data_size: data.len(),
            };
            (data, sample)
        })
        .collect();
    let described: Vec<Sample> = samples.iter().map(|(_, sample)| sample.clone()).collect();
    let mut bytes = muxer
        .create_media_segment_metadata(&described)
        .expect("the segment muxes");
    for (data, _) in &samples {
        bytes.extend_from_slice(data);
    }
    bytes
}

/// An fMP4 init segment and `segments` media segments of `frames` samples
/// each, from the canonical H.264 fixture's parameter sets.
pub(crate) fn fmp4_video_objects(segments: usize, frames: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
    use shiguredo_mp4::bitstream::h264::{
        H264SampleEntryConfig, LengthSize, build_avc1_box_from_annexb,
    };
    use shiguredo_mp4::boxes::SampleEntry;

    let config = H264SampleEntryConfig {
        length_size: LengthSize::FourBytes,
    };
    let entry = SampleEntry::Avc1(
        build_avc1_box_from_annexb(&fixture_keyframe("h264"), &config)
            .expect("the fixture's SPS/PPS parse"),
    );
    let mut muxer = shiguredo_mp4::mux::Fmp4SegmentMuxer::new().expect("the muxer builds");
    let media: Vec<Vec<u8>> = (0..segments)
        .map(|_| fmp4_segment(&mut muxer, &entry, frames))
        .collect();
    (muxer.init_segment_bytes().expect("the init builds"), media)
}

/// An `EXT-X-MAP` playlist listing `paths` from `first`, ended (ENDLIST).
pub(crate) fn fmp4_playlist(first: u64, paths: &[&str], ended: bool) -> String {
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{first}\n\
         #EXT-X-MAP:URI=\"init.mp4\"\n"
    );
    for path in paths {
        playlist.push_str(&format!("#EXTINF:0.1,\n{path}\n"));
    }
    if ended {
        playlist.push_str("#EXT-X-ENDLIST\n");
    }
    playlist
}

/// The same media segment with its `mdat` payload removed: its `moof` walks
/// and claims samples the object no longer carries.
pub(crate) fn emptied_mdat(segment: &[u8]) -> Vec<u8> {
    let mut offset = 0usize;
    while offset + 8 <= segment.len() {
        let size = u32::from_be_bytes(segment[offset..offset + 4].try_into().unwrap()) as usize;
        if &segment[offset + 4..offset + 8] == b"mdat" {
            let mut out = segment[..offset].to_vec();
            out.extend_from_slice(&8u32.to_be_bytes());
            out.extend_from_slice(b"mdat");
            return out;
        }
        if size < 8 {
            break;
        }
        offset += size;
    }
    segment.to_vec()
}

/// An HEVC fMP4 init segment (`hvc1`): this ingest refuses it.
pub(crate) fn fmp4_hevc_init() -> Vec<u8> {
    use shiguredo_mp4::bitstream::h265::{
        H265ConstantFrameRate, H265SampleEntryConfig, LengthSize, build_hvc1_box_from_annexb,
    };
    use shiguredo_mp4::boxes::SampleEntry;

    let config = H265SampleEntryConfig {
        length_size: LengthSize::FourBytes,
        avg_frame_rate: H265SampleEntryConfig::AVG_FRAME_RATE_UNSPECIFIED,
        constant_frame_rate: H265ConstantFrameRate::Unknown,
    };
    let entry = SampleEntry::Hvc1(
        build_hvc1_box_from_annexb(&fixture_keyframe("h265"), &config)
            .expect("the fixture's VPS/SPS/PPS parse"),
    );
    let mut muxer = shiguredo_mp4::mux::Fmp4SegmentMuxer::new().expect("the muxer builds");
    let _ = fmp4_segment(&mut muxer, &entry, 1);
    muxer.init_segment_bytes().expect("the init builds")
}
