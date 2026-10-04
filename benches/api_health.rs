//! Cost of one full `/api/v1/engine/health` body: build the snapshot for a
//! pipeline with N active outputs and serialize it, as the dashboard and the
//! capacity harness do every second. Allocation and JSON tree building, not
//! I/O, is what grows with N (runtime-crossings O2).

use std::collections::HashSet;
use std::hint::black_box;
use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use restream::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
use restream::domain::srt_ingest::SrtGlobalIngestConfig;
use restream::media::engine::MediaEngine;
use restream::media::security::IngestSecurityService;
use restream::{api, db};
use sqlx::SqlitePool;
use tokio::sync::{RwLock as TokioRwLock, broadcast};

const PIPELINE: &str = "bench-pipeline";

async fn state_with_outputs(outputs: usize) -> api::AppState {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&pool).await.unwrap();
    let engine = Arc::new(MediaEngine::new());
    for index in 0..outputs {
        engine
            .register_egress(
                &format!("output-{index:04}"),
                PIPELINE,
                &format!("rtmp://127.0.0.1:1935/live/key-{index:04}"),
            )
            .await;
    }
    let (log_broadcast, _) = broadcast::channel(32);
    api::AppState::test_new(
        restream::infrastructure::service_wiring::SqliteServiceFactory::new(&pool).compose(),
        pool,
        Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG)),
        Arc::new(restream::media::srt::SrtIngestPolicyStore::new(
            SrtGlobalIngestConfig::default(),
            &[],
        )),
        Arc::new(TokioRwLock::new(HashSet::new())),
        engine,
        log_broadcast,
    )
}

fn bench_health(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let pipeline_ids = vec![PIPELINE.to_string()];
    let mut group = c.benchmark_group("api_health");
    for outputs in [50usize, 500] {
        let state = runtime.block_on(state_with_outputs(outputs));
        group.throughput(Throughput::Elements(outputs as u64));
        group.bench_with_input(
            BenchmarkId::new("full_snapshot_serialized", outputs),
            &outputs,
            |b, _| {
                b.iter(|| {
                    let snapshot = runtime.block_on(
                        api::health::build_health_snapshot_for_pipeline_ids(&state, &pipeline_ids),
                    );
                    black_box(serde_json::to_vec(&snapshot).unwrap())
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("http_response_body", outputs),
            &outputs,
            |b, _| {
                b.iter(|| {
                    let view = runtime.block_on(api::health::build_health_view_for_pipeline_ids(
                        &state,
                        &pipeline_ids,
                    ));
                    black_box(serde_json::to_vec(&view).unwrap())
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_health);
criterion_main!(benches);
