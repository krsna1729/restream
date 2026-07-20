use std::hint::black_box;

use bytes::{Bytes, BytesMut};
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use restream::media::avio::MemoryQueue;

use super::support::PACKET_BYTES;

fn bench_memory_queue(c: &mut Criterion) {
    let packet = vec![0x47u8; PACKET_BYTES];
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("benchmark runtime");
    let mut group = c.benchmark_group("data_path/memory_queue");

    for burst in [1usize, 4, 8, 16, 32, 64] {
        let total_bytes = PACKET_BYTES * burst;
        group.throughput(Throughput::Bytes(total_bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("byte_vecdeque_round_trip", burst),
            &burst,
            |b, &burst| {
                b.iter_batched(
                    MemoryQueue::new,
                    |queue| {
                        for _ in 0..burst {
                            runtime.block_on(queue.write(&packet));
                        }
                        let mut output = vec![0u8; total_bytes];
                        let mut offset = 0usize;
                        while offset < output.len() {
                            let read = queue.read(&mut output[offset..]);
                            if read == 0 {
                                break;
                            }
                            offset += read;
                        }
                        black_box((queue, output, offset));
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("byte_vecdeque_batch_round_trip", burst),
            &burst,
            |b, &burst| {
                b.iter_batched(
                    MemoryQueue::new,
                    |queue| {
                        let written = runtime.block_on(
                            queue.write_batch(std::iter::repeat_n(packet.as_slice(), burst)),
                        );
                        black_box(written);
                        let mut output = vec![0u8; total_bytes];
                        let mut offset = 0usize;
                        while offset < output.len() {
                            let read = queue.read(&mut output[offset..]);
                            if read == 0 {
                                break;
                            }
                            offset += read;
                        }
                        black_box((queue, output, offset));
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_segment_finalize(c: &mut Criterion) {
    let mut group = c.benchmark_group("data_path/segment_finalize");
    group.sample_size(20);

    for size in [2usize * 1024 * 1024, 8 * 1024 * 1024] {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("copy_from_slice", size),
            &size,
            |b, &size| {
                b.iter_batched(
                    || vec![0x47u8; size],
                    |data| black_box(Bytes::copy_from_slice(&data)),
                    BatchSize::LargeInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("split_and_freeze", size),
            &size,
            |b, &size| {
                b.iter_batched(
                    || {
                        let mut data = BytesMut::with_capacity(size);
                        data.resize(size, 0x47);
                        data
                    },
                    |mut data| black_box(data.split().freeze()),
                    BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

pub(super) fn register(c: &mut Criterion) {
    bench_memory_queue(c);
    bench_segment_finalize(c);
}
