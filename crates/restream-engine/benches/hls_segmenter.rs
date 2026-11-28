//! The production HLS segmenter task (`MediaEngine::start_hls_segmenter`) on one live
//! pipeline: each iteration publishes the primary H.264 fixture to the ring
//! and waits until the segmenter's reader has caught up. Covers the
//! per-packet path of `run_hls_segmenter`: ring pull, keyframe segment cuts,
//! TS feeding and accumulation. The last pulled burst may still be in
//! processing when the reader reaches the head; that tail is at most one
//! burst per iteration.

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use restream_core::MediaPacket;
use restream_core::MediaType;
use restream_engine::engine::MediaEngine;
use restream_engine::hls::{HlsConfig, HlsSegmenterStart, HlsStore};
use restream_engine::hls_fmp4::Fmp4HlsStore;
use restream_sync::RingBuffer;
use tokio_util::sync::CancellationToken;

const PIPELINE: &str = "bench-hls";
const READER: &str = "hls:bench-hls";

/// Publishes one pass of the fixture, shifted past the previous pass.
fn publish(ring: &RingBuffer, packets: &[MediaPacket], offset_ms: i64) {
    ring.push_batch(packets.iter().cloned().map(|mut packet| {
        packet.dts += offset_ms;
        packet.pts += offset_ms;
        packet
    }));
}

/// Waits until the segmenter's reader has pulled everything published.
async fn wait_caught_up(ring: &RingBuffer) {
    let target = ring.get_write_idx();
    let caught_up = async {
        loop {
            let caught_up = ring
                .reader_snapshots()
                .iter()
                .any(|reader| reader.name == READER && reader.read_idx >= target);
            if caught_up {
                return;
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), caught_up)
        .await
        .expect("the HLS segmenter consumes every published packet");
}

fn bench_hls_segmenter(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (video_meta, audio_tracks, packets) =
        restream_engine::test_fixtures::primary_av_packets_for_codec("h264")
            .unwrap_or_else(|error| panic!("{error}"));
    let cycle_ms = packets.iter().map(|packet| packet.dts).max().unwrap()
        - packets.iter().map(|packet| packet.dts).min().unwrap()
        + 1000;
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let (ring, segmenter, _ingest) = runtime.block_on(async {
        let ring = engine.get_or_create_pipeline(PIPELINE).await;
        let ingest = engine
            .try_register_ingest(PIPELINE, "key", "srt")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                PIPELINE,
                Some(video_meta.clone()),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks(PIPELINE, audio_tracks.clone())
            .await;
        // The bench publishes far faster than real time: cut at every
        // keyframe and keep a short window so segment memory stays bounded.
        let store = Arc::new(HlsStore::with_config(HlsConfig {
            min_segment_secs: 0.0,
            max_segments: 3,
            ..HlsConfig::default()
        }));
        let segmenter = tokio::spawn(engine.start_hls_segmenter(
            PIPELINE.to_string(),
            store,
            ring.clone(),
            None,
            cancel.clone(),
            HlsSegmenterStart {
                video_meta_override: Some(video_meta.clone()),
                planned_stage_key: None,
            },
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ring
            .reader_snapshots()
            .iter()
            .any(|reader| reader.name == READER)
        {
            assert!(
                Instant::now() < deadline,
                "the segmenter attaches its reader"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        // Warm up: the first pass creates the TS feeder.
        publish(&ring, &packets, 0);
        wait_caught_up(&ring).await;
        (ring, segmenter, ingest)
    });

    let mut offset_ms = cycle_ms;
    let mut group = c.benchmark_group("hls_segmenter");
    group.throughput(Throughput::Elements(packets.len() as u64));
    group.bench_function("h264_fixture_pass", |b| {
        b.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                elapsed += runtime.block_on(async {
                    let start = Instant::now();
                    publish(&ring, &packets, offset_ms);
                    wait_caught_up(&ring).await;
                    start.elapsed()
                });
                offset_ms += cycle_ms;
            }
            elapsed
        });
    });
    group.finish();

    cancel.cancel();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), segmenter)
            .await
            .expect("the HLS segmenter stops")
            .expect("the HLS segmenter does not panic");
    });
}

fn bench_hls_fmp4_segmenter(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (video, audio, packets) =
        restream_engine::test_fixtures::primary_av_packets_for_codec("h264").unwrap();
    let audio_track = audio.first().expect("fixture has AAC audio").track_index;
    let mut keyframes = packets
        .iter()
        .enumerate()
        .filter(|(_, packet)| packet.media_type == MediaType::Video && packet.is_keyframe);
    let (first, keyframe) = keyframes.next().expect("fixture has a starting keyframe");
    let (end, next_keyframe) = keyframes.next().expect("fixture has a complete GOP");
    let tail = &packets[first + 1..end];
    let period = next_keyframe.dts - keyframe.dts;
    let ring = Arc::new(RingBuffer::new(8192));
    ring.set_audio_tracks(audio);
    let store = Arc::new(Fmp4HlsStore::with_config(HlsConfig {
        min_segment_secs: 0.0,
        max_segments: 3,
        ..HlsConfig::default()
    }));
    let cancel = CancellationToken::new();
    let engine = MediaEngine::new();
    let segmenter = runtime.spawn(engine.start_hls_fmp4_segmenter(
        "bench-hls-fmp4".to_string(),
        store.clone(),
        ring.clone(),
        cancel.clone(),
        HlsSegmenterStart {
            video_meta_override: Some(video),
            planned_stage_key: None,
        },
    ));
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while ring.reader_snapshots().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the fMP4 segmenter attaches its reader");
        ring.push(keyframe.clone());
    });
    let mut index = 0;
    let mut offset = 0;
    let mut group = c.benchmark_group("hls_fmp4_segmenter");
    group.throughput(Throughput::Elements((tail.len() + 1) as u64));
    group.bench_function("h264_gop_to_store", |b| {
        b.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                elapsed += runtime.block_on(async {
                    let start = Instant::now();
                    // The next keyframe closes this GOP and primes the next.
                    ring.push_batch(
                        tail.iter()
                            .map(|packet| (packet, offset))
                            .chain(std::iter::once((keyframe, offset + period)))
                            .map(|(packet, offset)| {
                                let mut packet = packet.clone();
                                packet.dts += offset;
                                packet.pts += offset;
                                packet
                            }),
                    );
                    tokio::time::timeout(Duration::from_secs(10), async {
                        while store.get_video_segment(index).is_none()
                            || store.get_audio_segment(audio_track, index).is_none()
                        {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("both renditions publish this GOP");
                    start.elapsed()
                });
                offset += period;
                index += 1;
            }
            elapsed
        });
    });
    group.finish();
    cancel.cancel();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), segmenter)
            .await
            .expect("the fMP4 segmenter stops")
            .expect("the fMP4 segmenter does not panic");
    });
}

criterion_group!(benches, bench_hls_segmenter, bench_hls_fmp4_segmenter);
criterion_main!(benches);
