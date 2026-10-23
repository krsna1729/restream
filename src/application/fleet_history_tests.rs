use super::*;
use crate::application::diagnosis::{Cause, Diagnosis};

fn sample(at_ms: u64) -> FleetSample {
    FleetSample {
        at_ms,
        headroom_percent: 50.0,
        delivering_percent: None,
        degraded: DegradedByLayer::default(),
        sent_bytes: 0,
    }
}

fn times(window: &FleetHistoryWindow) -> Vec<u64> {
    window.samples.iter().map(|sample| sample.at_ms).collect()
}

/// Full, the history drops its oldest sample per new one and says from when
/// it is complete; until then it is complete from its start.
#[test]
fn a_full_history_drops_the_oldest_and_moves_complete_from() {
    let history = FleetHistory::started_at(500, 3);
    for at in [1_000, 2_000, 3_000] {
        history.push(sample(at));
    }
    let window = history.window(0, u64::MAX);
    assert_eq!(times(&window), [1_000, 2_000, 3_000]);
    assert_eq!(window.complete_from_ms, 500, "nothing dropped yet");

    history.push(sample(4_000));
    history.push(sample(5_000));
    let window = history.window(0, u64::MAX);
    assert_eq!(
        times(&window),
        [3_000, 4_000, 5_000],
        "oldest first, newest kept"
    );
    assert_eq!(window.complete_from_ms, 2_000, "the latest dropped sample");
    assert_eq!(window.interval_ms, FLEET_SAMPLE_INTERVAL.as_millis() as u64);
}

/// Samples older than the span go even when the count allows them (a
/// sampler that stalled must not stretch the history past its span).
#[test]
fn samples_older_than_the_span_go_whatever_the_count() {
    let span = FLEET_HISTORY_SPAN.as_millis() as u64;
    let history = FleetHistory::started_at(0, FLEET_HISTORY_CAP);
    history.push(sample(1_000));
    history.push(sample(2_000));
    history.push(sample(2_000 + span));
    let window = history.window(0, u64::MAX);
    assert_eq!(
        times(&window),
        [2_000, 2_000 + span],
        "exactly the span is kept"
    );
    assert_eq!(window.complete_from_ms, 1_000);
}

/// `since` and `until` are inclusive and the window comes oldest first: a
/// resolved incident's span ends at its close, not at now.
#[test]
fn a_window_returns_the_samples_from_since_through_until() {
    let history = FleetHistory::started_at(0, 10);
    for at in [1_000, 2_000, 3_000] {
        history.push(sample(at));
    }
    let all = u64::MAX;
    assert_eq!(times(&history.window(2_000, all)), [2_000, 3_000]);
    assert_eq!(times(&history.window(1_999, all)), [2_000, 3_000]);
    assert_eq!(times(&history.window(3_001, all)), Vec::<u64>::new());
    assert_eq!(times(&history.window(0, all)), [1_000, 2_000, 3_000]);
    assert_eq!(times(&history.window(0, 2_000)), [1_000, 2_000]);
    assert_eq!(times(&history.window(0, 2_999)), [1_000, 2_000]);
    assert_eq!(times(&history.window(1_500, 2_500)), [2_000]);
    assert_eq!(times(&history.window(3_000, 1_000)), Vec::<u64>::new());
}

/// The wall clock can step back between samples (an NTP correction); the
/// history stays in time order, so a window still returns the right
/// samples instead of a binary search over unsorted times.
#[test]
fn a_clock_step_back_keeps_the_samples_in_order() {
    let history = FleetHistory::started_at(0, 10);
    for at in [1_000, 2_000, 3_000] {
        history.push(sample(at));
    }
    // The clock stepped back 2.5 s.
    history.push(sample(500));
    history.push(sample(4_000));
    assert_eq!(
        times(&history.window(0, u64::MAX)),
        [1_000, 2_000, 3_000, 3_001, 4_000]
    );
    assert_eq!(
        times(&history.window(2_500, u64::MAX)),
        [3_000, 3_001, 4_000]
    );
    // A client that holds the sample at 3 000 asks from 3 001 and still gets
    // the one taken while the clock was back.
    assert_eq!(times(&history.window(3_001, u64::MAX)), [3_001, 4_000]);
}

/// A closed window reports how far the history reaches beyond it, so a
/// client knows when every sample in the window has been taken.
#[test]
fn a_window_reports_the_newest_sample_beyond_its_end() {
    let history = FleetHistory::started_at(0, 10);
    assert_eq!(history.window(0, 2_500).newest_ms, None);
    for at in [1_000, 2_000, 3_000] {
        history.push(sample(at));
    }
    let window = history.window(0, 2_500);
    assert_eq!(times(&window), [1_000, 2_000]);
    assert_eq!(window.newest_ms, Some(3_000));
}

fn output(
    desired: DesiredOutputState,
    convergence: Convergence,
    flow: Flow,
    layer: Option<Layer>,
) -> OutputState {
    OutputState {
        id: String::new(),
        pipeline_id: "pipe".to_string(),
        name: String::new(),
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
        diagnosis: layer.map(|layer| Diagnosis {
            layer,
            cause: Cause::NoPublisher,
            ruled_out: Vec::new(),
        }),
        state_since_ms: 0,
    }
}

/// The server's sample is the control room's (`takeFleetSample`,
/// `isDelivering`): only wanted outputs count; one delivers when it is
/// converged *and* flowing; a diagnosed one counts under its layer; headroom
/// is what the projected utilization leaves, never below 0.
#[tokio::test]
async fn the_sample_counts_wanted_outputs_as_the_control_room_does() {
    use Convergence::{Converged, Starting};
    use DesiredOutputState::{Running, Stopped};
    use Flow::{Flowing, Idle};
    let outputs = [
        output(Running, Converged, Flowing, None),
        // Converged but not moving media: not delivering.
        output(Running, Converged, Idle, None),
        // Moving but not converged: not delivering.
        output(Running, Starting, Flowing, Some(Layer::Destination)),
        output(Running, Starting, Idle, Some(Layer::Ingest)),
        // Not wanted: neither delivering nor degraded, whatever it says.
        output(Stopped, Converged, Flowing, Some(Layer::Feed)),
    ];
    let mut capacity = MediaEngine::new().capacity_snapshot().await;
    capacity.projected_utilization = 0.25;
    let sample = take_fleet_sample(7, &outputs, &capacity);
    assert_eq!(sample.at_ms, 7);
    assert_eq!(sample.delivering_percent, Some(25.0), "1 of 4 wanted");
    assert_eq!(
        sample.degraded,
        DegradedByLayer {
            ingest: 1,
            feed: 0,
            shard: 0,
            destination: 1,
        }
    );
    assert_eq!(sample.headroom_percent, 75.0);

    capacity.projected_utilization = 1.5;
    let none_wanted = take_fleet_sample(8, &outputs[4..], &capacity);
    assert_eq!(
        none_wanted.delivering_percent, None,
        "nothing wanted: no figure"
    );
    assert_eq!(none_wanted.degraded, DegradedByLayer::default());
    assert_eq!(
        none_wanted.headroom_percent, 0.0,
        "over capacity: no headroom"
    );
}

#[test]
fn bytes_sent_count_growth_and_a_restarted_or_new_output_whole() {
    let mut seen = HashMap::new();
    let at = |pairs: &[(&str, u64)]| {
        pairs
            .iter()
            .map(|(id, bytes)| (id.to_string(), *bytes))
            .collect()
    };
    assert_eq!(
        bytes_sent_since(&mut seen, at(&[("a", 100), ("b", 50)])),
        150,
        "first look: all of each"
    );
    assert_eq!(
        bytes_sent_since(&mut seen, at(&[("a", 160), ("b", 50)])),
        60,
        "growth only"
    );
    // b restarted (its counter went back), c is new, a is gone.
    assert_eq!(bytes_sent_since(&mut seen, at(&[("b", 20), ("c", 5)])), 25);
    assert_eq!(seen.len(), 2, "a removed output is forgotten");
    assert_eq!(
        bytes_sent_since(&mut seen, at(&[("a", 30)])),
        30,
        "back again: counted from its start"
    );
}
