//! Cross-thread handoff of received SRT payloads from the ingress Owner to a
//! media worker (WI11). Two threads, one producer and one consumer, as in the
//! Owner -> worker hop: the producer hands over `(peer id, Bytes)` payloads in
//! passes of 32 and wakes the consumer once per pass; the consumer drains and
//! parks when empty. Compares today's Tokio mpsc (one event per payload) with
//! flume, crossbeam `ArrayQueue` and the wait-free SPSC `rtrb` ring.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};

const ITEMS: u64 = 200_000;
const PASS: u64 = 32;
const CAPACITY: usize = 1024;

type Item = (u64, Bytes);

fn payload() -> Bytes {
    static DATA: [u8; 1316] = [0x47; 1316];
    Bytes::from_static(&DATA)
}

/// Park/unpark wake: the producer unparks only when the consumer is parked.
struct Parker {
    parked: AtomicBool,
    consumer: thread::Thread,
}

impl Parker {
    fn wake(&self) {
        if self.parked.load(Ordering::SeqCst) {
            self.consumer.unpark();
        }
    }
}

fn run_queue<P, C>(mut push: P, mut pop: C) -> Duration
where
    P: FnMut(Item) -> Result<(), Item> + Send + 'static,
    C: FnMut() -> Option<Item> + Send + 'static,
{
    let done = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Arc<Parker>>();
    let consumer_done = done.clone();
    let consumer = thread::spawn(move || {
        let parker = Arc::new(Parker {
            parked: AtomicBool::new(false),
            consumer: thread::current(),
        });
        ready_tx.send(parker.clone()).unwrap();
        let mut received = 0u64;
        let mut bytes = 0usize;
        while received < ITEMS {
            match pop() {
                Some((_, payload)) => {
                    bytes += payload.len();
                    received += 1;
                }
                None => {
                    parker.parked.store(true, Ordering::SeqCst);
                    if let Some((_, payload)) = pop() {
                        parker.parked.store(false, Ordering::SeqCst);
                        bytes += payload.len();
                        received += 1;
                        continue;
                    }
                    thread::park_timeout(Duration::from_millis(1));
                    parker.parked.store(false, Ordering::SeqCst);
                }
            }
        }
        consumer_done.store(true, Ordering::SeqCst);
        std::hint::black_box(bytes);
    });
    let parker = ready_rx.recv().unwrap();
    let started = Instant::now();
    let data = payload();
    let mut sent = 0u64;
    while sent < ITEMS {
        let pass_end = (sent + PASS).min(ITEMS);
        while sent < pass_end {
            let mut item = (sent, data.clone());
            loop {
                match push(item) {
                    Ok(()) => break,
                    Err(back) => {
                        item = back;
                        parker.wake();
                        std::hint::spin_loop();
                    }
                }
            }
            sent += 1;
        }
        parker.wake();
    }
    consumer.join().unwrap();
    started.elapsed()
}

fn bench_handoff(c: &mut Criterion) {
    let mut group = c.benchmark_group("ingest_handoff");
    group.throughput(Throughput::Elements(ITEMS));
    group.sample_size(20);

    group.bench_function("tokio_mpsc_per_payload", |b| {
        b.iter_custom(|iters| {
            (0..iters)
                .map(|_| {
                    let (tx, mut rx) = tokio::sync::mpsc::channel::<Item>(256);
                    run_queue(
                        move |item| {
                            tx.try_send(item).map_err(|error| match error {
                                tokio::sync::mpsc::error::TrySendError::Full(item)
                                | tokio::sync::mpsc::error::TrySendError::Closed(item) => item,
                            })
                        },
                        move || rx.try_recv().ok(),
                    )
                })
                .sum()
        })
    });

    group.bench_function("flume_bounded", |b| {
        b.iter_custom(|iters| {
            (0..iters)
                .map(|_| {
                    let (tx, rx) = flume::bounded::<Item>(CAPACITY);
                    run_queue(
                        move |item| tx.try_send(item).map_err(flume::TrySendError::into_inner),
                        move || rx.try_recv().ok(),
                    )
                })
                .sum()
        })
    });

    group.bench_function("crossbeam_array_queue", |b| {
        b.iter_custom(|iters| {
            (0..iters)
                .map(|_| {
                    let queue = Arc::new(crossbeam_queue::ArrayQueue::<Item>::new(CAPACITY));
                    let consumer = queue.clone();
                    run_queue(move |item| queue.push(item), move || consumer.pop())
                })
                .sum()
        })
    });

    group.bench_function("rtrb_spsc", |b| {
        b.iter_custom(|iters| {
            (0..iters)
                .map(|_| {
                    let (mut producer, mut consumer) = rtrb::RingBuffer::<Item>::new(CAPACITY);
                    run_queue(
                        move |item| {
                            producer
                                .push(item)
                                .map_err(|rtrb::PushError::Full(item)| item)
                        },
                        move || consumer.pop().ok(),
                    )
                })
                .sum()
        })
    });

    group.finish();
}

criterion_group!(benches, bench_handoff);
criterion_main!(benches);
