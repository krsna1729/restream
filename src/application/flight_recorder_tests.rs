use super::*;
use crate::application::diagnosis::{Cause, Diagnosis, Layer};
use crate::application::runtime_state::RetryState;

fn output(
    id: &str,
    desired: DesiredOutputState,
    convergence: Convergence,
    flow: Flow,
) -> OutputState {
    OutputState {
        id: id.to_string(),
        pipeline_id: "pipe".to_string(),
        name: id.to_string(),
        protocol: crate::domain::output_spec::EgressProtocol::Rtmp,
        shard: None,
        stage: None,
        desired,
        actual: None,
        convergence,
        flow,
        backpressure: None,
        retry: None,
        evidence: None,
        diagnosis: None,
    }
}

fn ingest(input: &str, bytes: u64) -> IngestProbe {
    IngestProbe {
        input_id: input.to_string(),
        protocol: "srt".to_string(),
        bytes_received: bytes,
        keyframe_ms: vec![0, 2_000, 4_000, 10_000],
        ..IngestProbe::default()
    }
}

fn probe(pipeline: &str, ingest: Option<IngestProbe>) -> PipelineProbe {
    PipelineProbe {
        pipeline_id: pipeline.to_string(),
        ingest,
        ..PipelineProbe::default()
    }
}

fn bitrates(recorder: &FlightRecorder, pipeline: &str) -> Vec<Option<f64>> {
    recorder
        .trace(pipeline, 0)
        .expect("trace")
        .samples
        .iter()
        .map(|sample| sample.ingest.as_ref().and_then(|ingest| ingest.bitrate_bps))
        .collect()
}

#[test]
fn keyframe_intervals_skip_resets_and_need_two_keyframes() {
    assert_eq!(keyframe_intervals(&[]), (None, None));
    assert_eq!(keyframe_intervals(&[5_000]), (None, None));
    assert_eq!(
        keyframe_intervals(&[0, 2_000, 4_000, 10_000]),
        (Some(10.0 / 3.0), Some(6.0))
    );
    // A timestamp that runs backwards is a reset, not a zero-length GOP.
    assert_eq!(
        keyframe_intervals(&[8_000, 0, 2_000]),
        (Some(2.0), Some(2.0))
    );
}

/// Bitrate is the byte counter's change over the interval, only within one
/// session: a failover to another input, a counter that went down, or a gap
/// with no ingest has no rate.
#[test]
fn bitrate_differences_one_session_and_never_spans_a_failover() {
    let recorder = FlightRecorder::started_at(0, 10);
    recorder.record(1_000, vec![probe("p", Some(ingest("a", 0)))]);
    recorder.record(3_000, vec![probe("p", Some(ingest("a", 1_000_000)))]);
    recorder.record(5_000, vec![probe("p", Some(ingest("b", 2_000_000)))]);
    recorder.record(7_000, vec![probe("p", Some(ingest("b", 1_000)))]);
    recorder.record(9_000, vec![probe("p", None)]);
    recorder.record(11_000, vec![probe("p", Some(ingest("b", 9_000)))]);
    assert_eq!(
        bitrates(&recorder, "p"),
        [None, Some(4_000_000.0), None, None, None, None],
    );
    let first = &recorder.trace("p", 0).expect("trace").samples[0];
    let health = first.ingest.as_ref().expect("ingest");
    assert_eq!(
        (health.gop_sec, health.max_gop_sec),
        (Some(10.0 / 3.0), Some(6.0))
    );
}

#[test]
fn a_full_trace_drops_the_oldest_and_a_gone_pipeline_drops_its_trace() {
    let recorder = FlightRecorder::started_at(500, 3);
    for at in [1_000, 2_000, 3_000, 4_000] {
        recorder.record(at, vec![probe("p", None), probe("q", None)]);
    }
    let trace = recorder.trace("p", 0).expect("trace");
    assert_eq!(
        trace.samples.iter().map(|s| s.at_ms).collect::<Vec<_>>(),
        [2_000, 3_000, 4_000]
    );
    assert_eq!(trace.complete_from_ms, 1_000);
    assert_eq!(trace.schema, TRACE_SCHEMA);
    assert_eq!(
        recorder.trace("p", 3_000).expect("trace").samples.len(),
        2,
        "since is inclusive"
    );
    // The wall clock stepping back still appends strictly after the newest.
    recorder.record(3_500, vec![probe("p", None)]);
    assert_eq!(
        recorder
            .trace("p", 0)
            .expect("trace")
            .samples
            .last()
            .map(|s| s.at_ms),
        Some(4_001)
    );
    assert!(recorder.trace("q", 0).is_none(), "q left the runtime state");
}

#[test]
fn outputs_health_counts_wanted_outputs_and_sums_running_egresses() {
    let mut retrying = output(
        "r",
        DesiredOutputState::Running,
        Convergence::Retrying,
        Flow::Idle,
    );
    retrying.retry = Some(RetryState {
        attempts: 4,
        next_attempt_at_ms: 0,
    });
    retrying.diagnosis = Some(Diagnosis {
        layer: Layer::Destination,
        cause: Cause::NoProgress,
        ruled_out: Vec::new(),
    });
    let outputs = [
        output(
            "live",
            DesiredOutputState::Running,
            Convergence::Converged,
            Flow::Flowing,
        ),
        output(
            "stalled",
            DesiredOutputState::Running,
            Convergence::Converged,
            Flow::Stalled,
        ),
        retrying,
        output(
            "off",
            DesiredOutputState::Stopped,
            Convergence::Converged,
            Flow::Idle,
        ),
    ];
    let egresses = HashMap::from([
        (
            "live".to_string(),
            EgressProbe {
                offered_bps: Some(1_000.0),
                delivered_bps: Some(990.0),
                resyncs: 1,
                feed_lag: 3,
            },
        ),
        (
            "stalled".to_string(),
            EgressProbe {
                offered_bps: Some(1_000.0),
                delivered_bps: None,
                resyncs: 2,
                feed_lag: 900,
            },
        ),
    ]);
    let health = outputs_health(&outputs, &egresses);
    assert_eq!(
        health,
        OutputsHealth {
            wanted: 3,
            delivering: 1,
            degraded: 1,
            retrying: 1,
            stalled: 1,
            retry_attempts: 4,
            offered_bps: 2_000.0,
            delivered_bps: 990.0,
            resyncs: 3,
            worst_feed_lag: 900,
        }
    );
}

fn sample(at_ms: u64) -> FlightSample {
    FlightSample {
        at_ms,
        ingest: None,
        ring: None,
        outputs: OutputsHealth::default(),
    }
}

#[test]
fn thinning_keeps_the_ends_and_spreads_the_rest() {
    let samples: Vec<FlightSample> = (0..10).map(sample).collect();
    let kept: Vec<u64> = thin(samples.clone(), 4).iter().map(|s| s.at_ms).collect();
    assert_eq!(kept, vec![0, 3, 6, 9]);
    assert_eq!(thin(samples.clone(), 10).len(), 10, "nothing to thin");
    let kept: Vec<u64> = thin((0..1001).map(sample).collect(), 3)
        .iter()
        .map(|s| s.at_ms)
        .collect();
    assert_eq!(kept, vec![0, 500, 1000]);
}

#[tokio::test]
async fn a_window_reads_the_persisted_history_then_the_recent_samples() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    // An hour ago, persisted every 10 s for 50 minutes.
    let rows: Vec<(String, i64, String)> = (0..300)
        .map(|i| {
            let at = 1_000_000 + i * 10_000;
            (
                "p1".to_string(),
                at as i64,
                serde_json::to_string(&sample(at)).unwrap(),
            )
        })
        .collect();
    crate::db::flight_repo::insert_batch(&pool, &rows)
        .await
        .unwrap();
    // The recorder started after them and holds 2 s samples.
    let recorder = FlightRecorder::started_at(4_000_000, 300);
    for at in (4_000_000..4_020_000).step_by(2_000) {
        recorder.record(at, vec![probe("p1", None)]);
    }

    let trace = trace_window(&recorder, &pool, "p1", 0, u64::MAX, 2_000)
        .await
        .unwrap();
    assert_eq!(
        trace.complete_from_ms, 1_000_000,
        "complete from the oldest persisted sample"
    );
    assert_eq!(trace.detailed_from_ms, 4_000_000);
    assert_eq!(trace.samples.len(), 300 + 10);
    assert!(
        trace
            .samples
            .windows(2)
            .all(|pair| pair[0].at_ms < pair[1].at_ms),
        "in time order"
    );

    // A window inside the persisted part reads only it; one inside memory only memory.
    let old = trace_window(&recorder, &pool, "p1", 1_100_000, 1_200_000, 2_000)
        .await
        .unwrap();
    assert_eq!(old.samples.len(), 10);
    let recent = trace_window(&recorder, &pool, "p1", 4_010_000, u64::MAX, 2_000)
        .await
        .unwrap();
    assert_eq!(recent.samples.first().unwrap().at_ms, 4_010_000);
    // Thinned to the points asked for.
    assert_eq!(
        trace_window(&recorder, &pool, "p1", 0, u64::MAX, 50)
            .await
            .unwrap()
            .samples
            .len(),
        50
    );
    // A pipeline neither knows.
    assert!(
        trace_window(&recorder, &pool, "nope", 0, u64::MAX, 50)
            .await
            .is_none()
    );

    crate::db::flight_repo::prune(&pool, 2_000_000)
        .await
        .unwrap();
    let pruned = trace_window(&recorder, &pool, "p1", 0, u64::MAX, 2_000)
        .await
        .unwrap();
    assert_eq!(pruned.complete_from_ms, 2_000_000);
}

#[tokio::test]
async fn persisting_writes_each_pipelines_newest_sample_and_prunes_past_retention() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    crate::db::flight_repo::insert_batch(
        &pool,
        &[("p1".into(), 1, serde_json::to_string(&sample(1)).unwrap())],
    )
    .await
    .unwrap();
    let recorder = FlightRecorder::started_at(0, 10);
    recorder.record(5_000, vec![probe("p1", None), probe("p2", None)]);
    recorder.record(7_000, vec![probe("p1", None), probe("p2", None)]);
    persist(&recorder, &pool, 7_000, Duration::from_secs(5), true).await;
    for pipeline in ["p1", "p2"] {
        let rows = crate::db::flight_repo::window(&pool, pipeline, 0, i64::MAX)
            .await
            .unwrap();
        let times: Vec<i64> = rows.iter().map(|(at, _)| *at).collect();
        assert_eq!(
            times,
            vec![7_000],
            "{pipeline}: the newest only, the old row pruned"
        );
    }
}
