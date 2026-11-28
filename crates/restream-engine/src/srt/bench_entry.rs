//! The SRT direct-play reader drive for `benches/srt_reader_drive.rs`: the
//! ingress Owner's per-visit reader pass (`OwnerLoop::drive_readers`) over
//! attached [`SrtReaderMedia`] readers, from the shared muxer's TS chunk ring
//! to the SRT-sized fragments handed to the send path. The socket send is
//! not modelled: each fragment's length is counted where `OwnerLoop::send`
//! would take it, and no reader is ever deferred.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use restream_core::MediaType;
use tokio_util::sync::CancellationToken;

use super::ingress_media::{IngressMedia, SrtReaderMedia};
use super::ingress_owner::READER_SENDS_PER_VISIT;
use crate::engine::MediaEngine;
use crate::ingest_lease::IngestLease;
use crate::ts_chunk_ring::{TsChunkReader, TsChunkRing};

const PIPELINE: &str = "bench-srt-reader-drive";
/// One burst of the shared muxer's output: a keyframe and 23 inter frames
/// (64 TS packets each), each followed by one audio frame (3 TS packets).
const VIDEO_CHUNKS: usize = 24;
const VIDEO_CHUNK_BYTES: usize = 64 * 188;
const AUDIO_CHUNK_BYTES: usize = 3 * 188;

/// Attached readers on one TS chunk ring, driven as the Owner drives them.
pub struct ReaderDrive {
    media: IngressMedia<u64>,
    ring: TsChunkRing,
    burst: Vec<(Bytes, MediaType, bool)>,
    scratch: Vec<Bytes>,
    readers: usize,
    _engine: Arc<MediaEngine>,
    _ingest: IngestLease,
}

impl ReaderDrive {
    /// `readers` direct-play readers at the ring's live edge, each holding
    /// a lease on the pipeline's (idle) shared muxer as production readers
    /// do.
    pub async fn start(readers: usize) -> Self {
        let engine = Arc::new(MediaEngine::new());
        let ingest = engine
            .try_register_ingest(PIPELINE, "key", "srt")
            .await
            .expect("fresh engine registers the bench ingest");
        let source = engine.get_or_create_pipeline(PIPELINE).await;
        let ring = TsChunkRing::new(4096, CancellationToken::new());
        let mut media = IngressMedia::<u64>::default();
        for peer in 0..readers {
            let (_muxer, lease) = engine
                .get_or_create_ts_muxer_stage(PIPELINE, "play", source.clone())
                .await;
            let reader = TsChunkReader::new_live(format!("bench_srt_reader_{peer}"), &ring);
            media.attach_reader(peer as u64, SrtReaderMedia::new(reader, lease));
        }
        let video = Bytes::from(vec![0x47; VIDEO_CHUNK_BYTES]);
        let audio = Bytes::from(vec![0x47; AUDIO_CHUNK_BYTES]);
        let burst = (0..VIDEO_CHUNKS)
            .flat_map(|index| {
                [
                    (video.clone(), MediaType::Video, index == 0),
                    (audio.clone(), MediaType::Audio, false),
                ]
            })
            .collect();
        Self {
            media,
            ring,
            burst,
            scratch: Vec::with_capacity(READER_SENDS_PER_VISIT),
            readers,
            _engine: engine,
            _ingest: ingest.lease,
        }
    }

    /// Bytes every reader receives per burst, together.
    pub fn delivered_bytes(&self) -> usize {
        self.readers * VIDEO_CHUNKS * (VIDEO_CHUNK_BYTES + AUDIO_CHUNK_BYTES)
    }

    /// Publish `iterations` bursts; each is timed from the first visit to
    /// the visit that finds every reader drained.
    pub fn run(&mut self, iterations: u64) -> Duration {
        let mut elapsed = Duration::ZERO;
        for _ in 0..iterations {
            self.ring.push_batch(self.burst.iter().cloned());
            let start = Instant::now();
            let mut delivered = 0;
            loop {
                let sent = self.visit();
                if sent == 0 {
                    break;
                }
                delivered += sent;
            }
            elapsed += start.elapsed();
            assert_eq!(delivered, self.delivered_bytes(), "a burst went missing");
        }
        elapsed
    }

    /// One Owner visit over the readers: `OwnerLoop::drive_readers` with
    /// the send replaced by counting the fragment's bytes.
    fn visit(&mut self) -> usize {
        let mut sent = 0;
        for peer in self.media.reader_sessions() {
            let mut fragments = std::mem::take(&mut self.scratch);
            self.media
                .take_reader_fragments(peer, &mut fragments, READER_SENDS_PER_VISIT);
            for payload in fragments.drain(..) {
                sent += std::hint::black_box(payload).len();
            }
            self.scratch = fragments;
        }
        sent
    }
}
