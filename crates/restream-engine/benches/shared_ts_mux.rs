//! The production shared MPEG-TS muxer stage, end to end: packets pushed to
//! the source ring until their TS chunks are read back from the stage's TS
//! ring. Covers payload conversion (raw Annex B and FLV/AVCC), DTS
//! enforcement, muxing and the per-burst chunk slicing.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use restream_core::MediaPacket;
use restream_engine::engine::MediaEngine;
use restream_engine::ingest_lease::IngestLease;
use restream_engine::stage_registry::StageLease;
use restream_engine::ts_chunk_ring::TsChunkReader;
use restream_sync::RingBuffer;
use std::sync::Arc;
use std::time::{Duration, Instant};

const BURST_PACKETS: usize = 256;
const PIPELINE: &str = "pipe";

struct MuxStage {
    _engine: Arc<MediaEngine>,
    _ingest: IngestLease,
    _lease: StageLease,
    source: Arc<RingBuffer>,
    reader: TsChunkReader,
    packets: Vec<MediaPacket>,
    /// TS chunks one burst of `packets` produces.
    chunks_per_burst: usize,
}

impl MuxStage {
    async fn start(flv: bool) -> Self {
        let engine = Arc::new(MediaEngine::new());
        let ingest = engine
            .try_register_ingest(PIPELINE, "key", if flv { "rtmp" } else { "srt" })
            .await
            .unwrap();
        let (video, audio, mut packets) =
            restream_engine::test_fixtures::primary_av_packets_for_codec("h264")
                .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            packets.len() >= BURST_PACKETS,
            "fixture has only {} packets",
            packets.len()
        );
        packets.truncate(BURST_PACKETS);
        if flv {
            let (video_header, audio_header) =
                restream_engine::test_fixtures::wrap_packets_for_rtmp_ingest(
                    &video,
                    &audio,
                    &mut packets,
                );
            if let Some(header) = video_header {
                engine.cache_sequence_header(PIPELINE, true, header).await;
            }
            if let Some(header) = audio_header {
                engine.cache_sequence_header(PIPELINE, false, header).await;
            }
        }
        engine
            .update_ingest_meta(PIPELINE, Some(video), audio.first().cloned(), None)
            .await;
        let source = Arc::new(RingBuffer::new(4096));
        source.set_audio_tracks(audio);
        // Sizes the TS ring past one burst, as the probe does for a live
        // pipeline.
        source.set_estimated_pkt_rate(2_000.0);
        let (ts_ring, lease) = engine
            .get_or_create_ts_muxer_stage(PIPELINE, "source", source.clone())
            .await;
        let reader = TsChunkReader::new("bench_shared_ts_mux".to_string(), &ts_ring);
        let mut stage = Self {
            _engine: engine,
            _ingest: ingest.lease,
            _lease: lease,
            source,
            reader,
            packets,
            chunks_per_burst: 0,
        };
        stage.chunks_per_burst = stage.calibrate().await;
        stage
    }

    /// Chunks from one steady-state burst. The first burst warms up the
    /// stage (the reader joins at a keyframe; parameter sets are cached), so
    /// the count is taken from the second.
    async fn calibrate(&mut self) -> usize {
        self.drain_burst().await;
        let total = self.drain_burst().await;
        assert!(total > 0, "the muxer produced no TS");
        total
    }

    /// Pushes one burst and reads until the stage stays idle.
    async fn drain_burst(&mut self) -> usize {
        self.source.push_batch(self.packets.iter().cloned());
        let mut chunks = Vec::new();
        let mut total = 0;
        while tokio::time::timeout(
            Duration::from_millis(300),
            self.reader.wait_for_data_or_cancelled(),
        )
        .await
        .is_ok()
        {
            chunks.clear();
            total += self.reader.pull_burst(&mut chunks, 4096).unwrap();
        }
        total
    }

    async fn run(&mut self, iterations: u64) -> Duration {
        let mut chunks = Vec::new();
        let mut elapsed = Duration::ZERO;
        for _ in 0..iterations {
            let start = Instant::now();
            self.source.push_batch(self.packets.iter().cloned());
            let mut received = 0;
            while received < self.chunks_per_burst {
                tokio::time::timeout(
                    Duration::from_secs(5),
                    self.reader.wait_for_data_or_cancelled(),
                )
                .await
                .expect("the muxer stalled");
                chunks.clear();
                received += self.reader.pull_burst(&mut chunks, 4096).unwrap();
            }
            elapsed += start.elapsed();
            assert_eq!(received, self.chunks_per_burst, "burst output changed");
        }
        elapsed
    }
}

fn bench_shared_ts_mux(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("shared_ts_mux");
    group.throughput(Throughput::Elements(BURST_PACKETS as u64));
    for (name, flv) in [("raw_h264", false), ("flv_h264", true)] {
        let mut stage = rt.block_on(MuxStage::start(flv));
        group.bench_function(name, |b| {
            b.iter_custom(|iterations| rt.block_on(stage.run(iterations)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_shared_ts_mux);
criterion_main!(benches);
