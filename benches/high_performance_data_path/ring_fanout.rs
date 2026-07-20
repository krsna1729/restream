use std::{
    hint::black_box,
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput};
use restream::media::ring_buffer::{Reader, RingBuffer};

use super::support::{PACKET_BYTES, RING_CAPACITY, packet};

fn bench_ring_producer(c: &mut Criterion) {
    let payload = Bytes::from(vec![0x47; PACKET_BYTES]);
    let mut group = c.benchmark_group("data_path/ring_producer");

    for burst in [1usize, 4, 8, 16, 32, 64] {
        group.throughput(Throughput::Elements(burst as u64));
        group.bench_with_input(
            BenchmarkId::new("current_push_loop", burst),
            &burst,
            |b, &burst| {
                let ring = RingBuffer::new(RING_CAPACITY);
                let mut sequence = 0usize;
                b.iter(|| {
                    for _ in 0..burst {
                        ring.push(packet(sequence, &payload));
                        sequence = sequence.wrapping_add(1);
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("push_batch", burst),
            &burst,
            |b, &burst| {
                let ring = RingBuffer::new(RING_CAPACITY);
                let mut sequence = 0usize;
                b.iter(|| {
                    let start = sequence;
                    sequence = sequence.wrapping_add(burst);
                    black_box(ring.push_batch(
                        (start..start + burst).map(|sequence| packet(sequence, &payload)),
                    ));
                });
            },
        );
    }

    group.finish();
}

fn bench_ring_consumer(c: &mut Criterion) {
    let payload = Bytes::from(vec![0x47; PACKET_BYTES]);
    let mut group = c.benchmark_group("data_path/ring_consumer");

    for burst in [1usize, 4, 8, 16, 32, 64] {
        group.throughput(Throughput::Elements(burst as u64));
        group.bench_with_input(
            BenchmarkId::new("current_pull_loop", burst),
            &burst,
            |b, &burst| {
                b.iter_custom(|iterations| {
                    let mut remaining = iterations;
                    let mut elapsed = Duration::ZERO;
                    let mut sequence = 0usize;

                    while remaining > 0 {
                        let chunk = remaining.min(64) as usize;
                        let ring = Arc::new(RingBuffer::new(chunk * burst + 1));
                        let mut reader = Reader::new("bench_data_path_1".to_string(), ring.clone());
                        for _ in 0..chunk * burst {
                            ring.push(packet(sequence, &payload));
                            sequence = sequence.wrapping_add(1);
                        }

                        let started = Instant::now();
                        let mut packets = 0usize;
                        let mut bytes = 0usize;
                        let mut checksum = 0i64;
                        while packets < chunk * burst {
                            match reader.pull() {
                                Ok(Some(packet)) => {
                                    packets += 1;
                                    bytes += packet.payload.len();
                                    checksum = checksum.wrapping_add(packet.pts ^ packet.dts);
                                }
                                Ok(None) => break,
                                Err(error) => panic!("{error}"),
                            }
                        }
                        elapsed += started.elapsed();
                        black_box((packets, bytes, checksum));
                        remaining -= chunk as u64;
                    }

                    elapsed
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("pull_burst", burst),
            &burst,
            |b, &burst| {
                b.iter_custom(|iterations| {
                    let mut remaining = iterations;
                    let mut elapsed = Duration::ZERO;
                    let mut sequence = 0usize;

                    while remaining > 0 {
                        let chunk = remaining.min(64) as usize;
                        let ring = Arc::new(RingBuffer::new(chunk * burst + 1));
                        let mut reader = Reader::new("bench_data_path_2".to_string(), ring.clone());
                        ring.push_batch((0..chunk * burst).map(|_| {
                            let value = packet(sequence, &payload);
                            sequence = sequence.wrapping_add(1);
                            value
                        }));
                        let mut packets = Vec::with_capacity(burst);

                        let started = Instant::now();
                        let mut received = 0usize;
                        let mut bytes = 0usize;
                        let mut checksum = 0i64;
                        for _ in 0..chunk {
                            packets.clear();
                            let loaded = reader
                                .pull_burst(&mut packets, burst)
                                .expect("reader overflow");
                            received += loaded;
                            for packet in &packets {
                                bytes += packet.payload.len();
                                checksum = checksum.wrapping_add(packet.pts ^ packet.dts);
                            }
                        }
                        elapsed += started.elapsed();
                        black_box((received, bytes, checksum));
                        remaining -= chunk as u64;
                    }

                    elapsed
                });
            },
        );
    }

    group.finish();
}

fn bench_fanout_delivery(c: &mut Criterion) {
    let payload = Bytes::from(vec![0x47; PACKET_BYTES]);
    let mut group = c.benchmark_group("data_path/fanout_delivery");
    group.sample_size(20);

    for readers in [1usize, 32, 128, 500] {
        for burst in [1usize, 32] {
            let deliveries = readers * burst;
            group.throughput(Throughput::Elements(deliveries as u64));
            group.bench_with_input(
                BenchmarkId::new(format!("readers_{readers}"), burst),
                &(readers, burst),
                |b, &(readers, burst)| {
                    b.iter_custom(|iterations| {
                        let mut remaining = iterations;
                        let mut elapsed = Duration::ZERO;
                        let mut sequence = 0usize;

                        while remaining > 0 {
                            let chunk = remaining.min(4) as usize;
                            let ring = Arc::new(RingBuffer::new(chunk * burst + 1));
                            let mut consumers = (0..readers)
                                .map(|i| {
                                    Reader::new(
                                        format!("bench_data_path_multi_{}", i),
                                        ring.clone(),
                                    )
                                })
                                .collect::<Vec<_>>();
                            for _ in 0..chunk * burst {
                                ring.push(packet(sequence, &payload));
                                sequence = sequence.wrapping_add(1);
                            }

                            let started = Instant::now();
                            let mut delivered = 0usize;
                            let mut checksum = 0i64;
                            for consumer in &mut consumers {
                                for _ in 0..chunk * burst {
                                    let packet = consumer
                                        .pull()
                                        .expect("reader overflow")
                                        .expect("missing packet");
                                    delivered += 1;
                                    checksum = checksum
                                        .wrapping_add(packet.pts)
                                        .wrapping_add(packet.payload.len() as i64);
                                }
                            }
                            elapsed += started.elapsed();
                            black_box((delivered, checksum));
                            remaining -= chunk as u64;
                        }

                        elapsed
                    });
                },
            );
        }
    }

    group.finish();
}

pub(super) fn register(c: &mut Criterion) {
    bench_ring_producer(c);
    bench_ring_consumer(c);
    bench_fanout_delivery(c);
}
