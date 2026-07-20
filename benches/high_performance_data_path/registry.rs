use criterion::Criterion;
use restream::media::engine::MediaEngine;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

fn bench_control_plane_lookup(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let engine = Arc::new(MediaEngine::new());
    let cached = runtime.block_on(engine.get_or_create_pipeline("data-path-bench"));
    let mut group = c.benchmark_group("data_path/control_plane_lookup");

    group.bench_function("locked_hashmap_get_or_create", |b| {
        b.iter_custom(|iterations| {
            runtime.block_on(async {
                let started = Instant::now();
                for _ in 0..iterations {
                    black_box(engine.get_or_create_pipeline("data-path-bench").await);
                }
                started.elapsed()
            })
        });
    });

    group.bench_function("cached_hot_handle_clone", |b| {
        b.iter(|| black_box(cached.clone()));
    });

    group.finish();
}

fn bench_ingest_hot_handle(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let engine = Arc::new(MediaEngine::new());
    runtime
        .block_on(engine.try_register_ingest("hot-ingest-bench", "key", "rtmp"))
        .expect("register benchmark ingest");
    let cached_ring = runtime.block_on(engine.get_or_create_pipeline("hot-ingest-bench"));
    let direct_counter = runtime.block_on(async {
        engine.ingests.active.read().await["hot-ingest-bench"]
            .bytes_received
            .clone()
    });
    let facade_counter = runtime.block_on(async {
        engine
            .with_active_ingest("hot-ingest-bench", |ingest| ingest.bytes_received.clone())
            .await
            .expect("ingest handle via facade")
    });
    let mut group = c.benchmark_group("data_path/ingest_hot_handle");

    group.bench_function("registry_ring_and_counter", |b| {
        b.iter_custom(|iterations| {
            runtime.block_on(async {
                let started = Instant::now();
                for _ in 0..iterations {
                    let ring = engine.get_or_create_pipeline("hot-ingest-bench").await;
                    engine.update_ingest_bytes("hot-ingest-bench", 1316).await;
                    black_box(ring);
                }
                started.elapsed()
            })
        });
    });

    group.bench_function("cached_ring_and_counter", |b| {
        b.iter(|| {
            facade_counter.fetch_add(1316, Ordering::Relaxed);
            black_box(&cached_ring);
        });
    });

    group.bench_function("direct_registry_counter_lookup", |b| {
        b.iter_custom(|iterations| {
            runtime.block_on(async {
                let started = Instant::now();
                for _ in 0..iterations {
                    let counter = {
                        let ingests = engine.ingests.active.read().await;
                        ingests["hot-ingest-bench"].bytes_received.clone()
                    };
                    counter.fetch_add(1316, Ordering::Relaxed);
                }
                started.elapsed()
            })
        });
    });

    group.bench_function("facade_counter_lookup", |b| {
        b.iter_custom(|iterations| {
            runtime.block_on(async {
                let started = Instant::now();
                for _ in 0..iterations {
                    let counter = engine
                        .with_active_ingest("hot-ingest-bench", |ingest| {
                            ingest.bytes_received.clone()
                        })
                        .await
                        .expect("facade counter lookup");
                    counter.fetch_add(1316, Ordering::Relaxed);
                }
                started.elapsed()
            })
        });
    });

    black_box(direct_counter);

    group.finish();
}

fn bench_egress_progress_hot_handle(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let engine = Arc::new(MediaEngine::new());
    runtime.block_on(async {
        engine
            .register_egress(
                "hot-egress-bench",
                "pipe-bench",
                "rtmp://127.0.0.1/live/key",
            )
            .await;
    });
    let (cached_bytes, cached_metrics, cached_progress) = runtime.block_on(async {
        engine
            .with_active_egress("hot-egress-bench", |egress| {
                (
                    egress.bytes_sent.clone(),
                    egress.metrics.clone(),
                    egress.last_progress_ms.clone(),
                )
            })
            .await
            .expect("egress handle via facade")
    });
    let mut group = c.benchmark_group("data_path/egress_progress");

    group.bench_function("registry_progress_update", |b| {
        b.iter_custom(|iterations| {
            runtime.block_on(async {
                let started = Instant::now();
                for _ in 0..iterations {
                    engine
                        .record_egress_progress("hot-egress-bench", black_box(1316))
                        .await;
                }
                started.elapsed()
            })
        });
    });

    group.bench_function("cached_sampled_progress_update", |b| {
        let progress_sample_interval = Duration::from_millis(250);
        let mut last_progress_sample = Instant::now();
        b.iter(|| {
            cached_bytes.fetch_add(black_box(1316), Ordering::Relaxed);
            cached_metrics.record_out(black_box(1316));
            if last_progress_sample.elapsed() >= progress_sample_interval {
                cached_progress.store(black_box(1), Ordering::Relaxed);
                last_progress_sample = Instant::now();
            }
        });
    });

    group.bench_function("direct_registry_progress_lookup", |b| {
        b.iter_custom(|iterations| {
            runtime.block_on(async {
                let started = Instant::now();
                for _ in 0..iterations {
                    let (bytes_sent, metrics, progress) = {
                        let egresses = engine.egresses.active.read().await;
                        let egress = &egresses["hot-egress-bench"];
                        (
                            egress.bytes_sent.clone(),
                            egress.metrics.clone(),
                            egress.last_progress_ms.clone(),
                        )
                    };
                    bytes_sent.fetch_add(black_box(1316), Ordering::Relaxed);
                    metrics.record_out(black_box(1316));
                    progress.store(black_box(1), Ordering::Relaxed);
                }
                started.elapsed()
            })
        });
    });

    group.bench_function("facade_progress_lookup", |b| {
        b.iter_custom(|iterations| {
            runtime.block_on(async {
                let started = Instant::now();
                for _ in 0..iterations {
                    let (bytes_sent, metrics, progress) = engine
                        .with_active_egress("hot-egress-bench", |egress| {
                            (
                                egress.bytes_sent.clone(),
                                egress.metrics.clone(),
                                egress.last_progress_ms.clone(),
                            )
                        })
                        .await
                        .expect("facade progress lookup");
                    bytes_sent.fetch_add(black_box(1316), Ordering::Relaxed);
                    metrics.record_out(black_box(1316));
                    progress.store(black_box(1), Ordering::Relaxed);
                }
                started.elapsed()
            })
        });
    });

    group.finish();
}

/// Fix #3 evidence: models the SRT ingest keyframe recording path.
///
/// Before: self.engine.record_keyframe() — async RwLock read + HashMap lookup
///         + Mutex lock per IDR frame.
/// After:  direct Arc<Mutex<Vec<i64>>> lock — no registry lookup.
fn bench_keyframe_record(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("tokio");
    let engine = std::sync::Arc::new(MediaEngine::new());
    runtime
        .block_on(engine.try_register_ingest("kf-bench", "key", "srt"))
        .expect("register");

    // Simulate the cached handle the fixed code creates once at connection setup:
    // a standalone Arc<Mutex<Vec<i64>>> representing the same cost as a direct
    // lock on a cached field — valid regardless of whether keyframe_times is
    // Arc-wrapped in the engine struct (which Fix #3 changes).
    let cached_kf_times = std::sync::Arc::new(std::sync::Mutex::new(Vec::<i64>::new()));
    // Populate it the same way the engine does.
    runtime.block_on(async {
        let ingests = engine.ingests.active.read().await;
        let times = ingests["kf-bench"]
            .keyframe_times
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _ = times.len(); // warm up
    });

    let mut group = c.benchmark_group("data_path/keyframe_record");

    // --- BEFORE: full registry lookup per keyframe ---
    group.bench_function("registry_lookup", |b| {
        b.iter_custom(|iterations| {
            runtime.block_on(async {
                let started = Instant::now();
                for i in 0..iterations {
                    engine.record_keyframe("kf-bench", i as i64).await;
                }
                started.elapsed()
            })
        });
    });

    // --- AFTER: direct cached Mutex lock ---
    group.bench_function("cached_direct_lock", |b| {
        b.iter(|| {
            let mut times = cached_kf_times.lock().unwrap_or_else(|e| e.into_inner());
            times.push(black_box(42i64));
            if times.len() > 30 {
                times.remove(0);
            }
            black_box(times.len());
        });
    });

    group.finish();
}

pub(super) fn register_hot_handles(c: &mut Criterion) {
    bench_control_plane_lookup(c);
    bench_ingest_hot_handle(c);
    bench_egress_progress_hot_handle(c);
}

pub(super) fn register_keyframe(c: &mut Criterion) {
    bench_keyframe_record(c);
}
