use super::*;
use proptest::prelude::*;

const STEP_MS: u64 = 2_000;

fn ratio(value: f64) -> Readings {
    Readings {
        delivery_ratio: Some(value),
        ..Readings::default()
    }
}

/// Feeds `values` to one output, one per 2 s from `start`, and returns the
/// anomalies after each.
fn run(baselines: &DestinationBaselines, start: u64, values: &[Readings]) -> Vec<Vec<Anomaly>> {
    values
        .iter()
        .enumerate()
        .map(|(index, readings)| {
            baselines.observe(
                start + index as u64 * STEP_MS,
                vec![("p".into(), "o".into(), readings.clone())],
            );
            baselines.anomalies()
        })
        .collect()
}

/// A steady ratio around 1.0 with a little deterministic wobble.
fn steady(count: usize) -> Vec<Readings> {
    (0..count)
        .map(|index| ratio(1.0 + 0.002 * ((index % 5) as f64 - 2.0) / 2.0))
        .collect()
}

#[test]
fn nothing_is_judged_during_warm_up() {
    let baselines = DestinationBaselines::default();
    let mut values = steady(10);
    values.extend(std::iter::repeat_n(
        ratio(0.2),
        WARM_UP_SAMPLES as usize - 20,
    ));
    assert!(run(&baselines, 0, &values).iter().all(Vec::is_empty));
}

#[test]
fn a_sustained_drop_is_flagged_dated_from_its_start_and_clears_after_recovery() {
    let baselines = DestinationBaselines::default();
    let warm = steady(WARM_UP_SAMPLES as usize + 20);
    run(&baselines, 0, &warm);
    let start = warm.len() as u64 * STEP_MS;
    // Four bad samples are not enough; the fifth confirms.
    let found = run(
        &baselines,
        start,
        &vec![ratio(0.9); SUSTAIN_SAMPLES as usize],
    );
    assert!(
        found[..SUSTAIN_SAMPLES as usize - 1]
            .iter()
            .all(Vec::is_empty)
    );
    let anomaly = &found[SUSTAIN_SAMPLES as usize - 1][0];
    assert_eq!(anomaly.signal, Signal::DeliveryRatio);
    assert_eq!(anomaly.since_ms, start, "dated from the first bad sample");
    assert!((anomaly.usual - 1.0).abs() < 0.01);
    assert_eq!(anomaly.value, 0.9);
    // A long fault is not learned as normal.
    let still = run(&baselines, start + 10 * STEP_MS, &vec![ratio(0.9); 300]);
    assert!(still.iter().all(|now| now.len() == 1));
    // Back to normal: clears after five good samples.
    let back = run(
        &baselines,
        start + 400 * STEP_MS,
        &steady(SUSTAIN_SAMPLES as usize),
    );
    assert!(back[SUSTAIN_SAMPLES as usize - 2].len() == 1);
    assert!(back[SUSTAIN_SAMPLES as usize - 1].is_empty());
}

#[test]
fn only_the_bad_side_counts_and_a_gone_output_is_forgotten() {
    let baselines = DestinationBaselines::default();
    let warm = (0..200)
        .map(|_| Readings {
            delivery_ratio: Some(0.97),
            rtt_ms: Some(40.0),
            retrans_per_sec: Some(0.0),
        })
        .collect::<Vec<_>>();
    run(&baselines, 0, &warm);
    // Better than usual on every signal: not an anomaly.
    let better = Readings {
        delivery_ratio: Some(1.0),
        rtt_ms: Some(10.0),
        retrans_per_sec: Some(0.0),
    };
    assert!(
        run(&baselines, 400_000, &vec![better; 20])
            .iter()
            .all(Vec::is_empty)
    );
    // RTT and retransmits worse: two anomalies.
    let worse = Readings {
        delivery_ratio: Some(0.97),
        rtt_ms: Some(120.0),
        retrans_per_sec: Some(25.0),
    };
    let found = run(&baselines, 500_000, &vec![worse; 6]).pop().unwrap();
    let signals: Vec<Signal> = found.iter().map(|anomaly| anomaly.signal).collect();
    assert_eq!(signals, [Signal::RttMs, Signal::RetransPerSec]);
    // The output stops: its baseline goes with it.
    baselines.observe(600_000, Vec::new());
    assert!(baselines.anomalies().is_empty());
}

#[test]
fn a_slow_decline_is_flagged_before_the_fixed_delivery_floor() {
    // Usual 1.0; then the ratio slides 0.002 per sample. The fixed rule
    // (0.95) fires after 25 samples; the baseline must fire earlier.
    let baselines = DestinationBaselines::default();
    let warm = steady(WARM_UP_SAMPLES as usize + 50);
    run(&baselines, 0, &warm);
    let slide: Vec<Readings> = (1..=40)
        .map(|step| ratio(1.0 - 0.002 * step as f64))
        .collect();
    let found = run(&baselines, warm.len() as u64 * STEP_MS, &slide);
    let flagged = found
        .iter()
        .position(|now| !now.is_empty())
        .expect("flagged");
    let fixed_rule = slide
        .iter()
        .position(|readings| readings.delivery_ratio.unwrap() < 0.95)
        .unwrap();
    assert!(
        flagged < fixed_rule,
        "baseline at sample {flagged}, fixed rule at {fixed_rule}"
    );
}

proptest! {
    /// Against a simple model: values that stay within 3 deviations of a
    /// steady mean never alert, whatever their order.
    #[test]
    fn noise_within_three_deviations_never_alerts(
        mean in 20.0f64..300.0,
        noise in proptest::collection::vec(-1.0f64..1.0, 200..600),
    ) {
        let baselines = DestinationBaselines::default();
        // RTT's floor is 10% of the mean: noise of 3% stays well inside.
        let values: Vec<Readings> = noise
            .iter()
            .map(|unit| Readings { rtt_ms: Some(mean * (1.0 + 0.03 * unit)), ..Readings::default() })
            .collect();
        prop_assert!(run(&baselines, 0, &values).iter().all(Vec::is_empty));
    }

    /// ...and a jump of more than six floors, held, always alerts once warm.
    #[test]
    fn a_held_large_jump_always_alerts_once_warm(
        mean in 20.0f64..300.0,
        hold in SUSTAIN_SAMPLES..40,
    ) {
        let baselines = DestinationBaselines::default();
        let mut values = vec![Readings { rtt_ms: Some(mean), ..Readings::default() }; WARM_UP_SAMPLES as usize];
        values.extend(vec![Readings { rtt_ms: Some(mean * 1.7 + 30.0), ..Readings::default() }; hold as usize]);
        let found = run(&baselines, 0, &values);
        prop_assert_eq!(found.last().unwrap().len(), 1);
    }
}
