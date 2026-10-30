use super::*;

#[test]
fn pipe_metrics_snapshot_correctness() {
    let pm = PipeMetrics::default();
    let snap = pm.snapshot();

    // All counters start at zero; avg fields are also zero.
    assert_eq!(snap.stalls, 0);
    assert_eq!(snap.stall_us, 0);
    assert_eq!(snap.avg_stall_us, 0);
    assert_eq!(snap.idles, 0);
    assert_eq!(snap.idle_us, 0);
    assert_eq!(snap.avg_idle_us, 0);

    // Stdin stall accumulation and average.
    pm.record_stall(2_000);
    pm.record_stall(6_000);
    let snap = pm.snapshot();
    assert_eq!(snap.stalls, 2);
    assert_eq!(snap.stall_us, 8_000);
    assert_eq!(snap.avg_stall_us, 4_000);

    // Stdout idle accumulation and average.
    pm.record_idle(3_000);
    let snap = pm.snapshot();
    assert_eq!(snap.idles, 1);
    assert_eq!(snap.idle_us, 3_000);
    assert_eq!(snap.avg_idle_us, 3_000);

    // StageMetricsSnapshot is a fixed typed struct with no pipe-metrics
    // fields, so the two counter families can no longer be conflated at the
    // type level.
    let sm = StageMetrics::new();
    sm.record_in(64);
    let ssnap = sm.snapshot();
    assert_eq!(ssnap.packets_in, 1);
    assert_eq!(ssnap.bytes_in, 64);
}

/// The SRT listener diagnostic counts SRT ingests only: a concurrent RTMP
/// ingest must not inflate the listener's stream number.
#[tokio::test]
async fn srt_listener_diag_counts_only_srt_ingests() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipeline-rtmp", "rtmp-key", "rtmp")
        .await
        .unwrap();
    engine
        .try_register_ingest("pipeline-srt", "srt-key", "srt")
        .await
        .unwrap();

    let stats = engine.srt_listener_diag_snapshot().await;
    assert_eq!(stats.active_ingest_count, 1);
    assert_eq!(
        engine.active_ingest_count().await,
        2,
        "the engine-wide ingest count still counts every protocol"
    );
}
