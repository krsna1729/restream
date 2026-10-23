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
