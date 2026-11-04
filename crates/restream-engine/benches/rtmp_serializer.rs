//! RTMP media framing cost per message.
//!
//! `production_vectored` is the egress media path: `bench_frame_media`
//! drives the shard's `RtmpWireMessage` (fixed header storage, payload
//! shared by reference into vectored writes). `rml_owned_vec` is
//! `rml_rtmp`'s `ChunkSerializer`, which production still uses for
//! sequence headers and for media sent before the stream id is known.

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use restream_engine::egress::backends::rtmp::bench_frame_media;
use rml_rtmp::chunk_io::ChunkSerializer;
use rml_rtmp::messages::MessagePayload;
use rml_rtmp::time::RtmpTimestamp;
use std::hint::black_box;

/// `RESTREAM_EGRESS_VISIT_MAX_BYTES` default: the most one visit writes.
const MAX_WRITE: usize = 256 * 1024;
const MESSAGES: u32 = 64;

fn media_payload(type_id: u8, size: usize, timestamp: u32) -> MessagePayload {
    MessagePayload {
        timestamp: RtmpTimestamp::new(timestamp),
        type_id,
        message_stream_id: 1,
        data: Bytes::from(vec![0x55; size]),
    }
}

fn message_sequence(payload_size: usize, type_id: u8) -> Vec<MessagePayload> {
    (0..MESSAGES)
        .map(|index| media_payload(type_id, payload_size, 33 * index))
        .collect()
}

fn bench_rtmp_serializer(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtmp/serializer");

    for (label, type_id, payload_size) in [
        ("audio_200b", 8, 200),
        ("video_pframe_8k", 9, 8 * 1024),
        ("video_pframe_30k", 9, 30 * 1024),
        ("video_idr_80k", 9, 80 * 1024),
    ] {
        let messages = message_sequence(payload_size, type_id);
        for chunk_size in [4096_u32, 16 * 1024, 64 * 1024] {
            let case_label = format!("{label}_chunk{chunk_size}");
            // One type-0 header, then one type-3 byte per continuation.
            let chunks = payload_size.div_ceil(chunk_size as usize);
            assert_eq!(
                bench_frame_media(
                    type_id,
                    0,
                    messages[0].data.clone(),
                    chunk_size as usize,
                    MAX_WRITE
                ),
                payload_size + 12 + (chunks - 1),
            );
            group.throughput(Throughput::Bytes(payload_size as u64 * u64::from(MESSAGES)));

            group.bench_with_input(
                BenchmarkId::new("rml_owned_vec", &case_label),
                &messages,
                |b, messages| {
                    b.iter_batched(
                        || {
                            let mut serializer = ChunkSerializer::new();
                            serializer
                                .set_max_chunk_size(chunk_size, RtmpTimestamp::new(0))
                                .unwrap();
                            serializer
                        },
                        |mut serializer| {
                            for message in messages {
                                black_box(serializer.serialize(message, false, false).unwrap());
                            }
                        },
                        BatchSize::SmallInput,
                    )
                },
            );

            group.bench_with_input(
                BenchmarkId::new("production_vectored", &case_label),
                &messages,
                |b, messages| {
                    b.iter(|| {
                        for message in messages {
                            black_box(bench_frame_media(
                                message.type_id,
                                message.timestamp.value,
                                message.data.clone(),
                                chunk_size as usize,
                                MAX_WRITE,
                            ));
                        }
                    })
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_rtmp_serializer);
criterion_main!(benches);
