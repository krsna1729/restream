use super::egress::next_output_job_id;
use crate::config::RuntimeTuning;

#[test]
fn runtime_tuning_defaults_preserve_existing_operational_behavior() {
    let tuning = RuntimeTuning::default();

    assert_eq!(tuning.nofile_limit, 65_536);
    assert_eq!(tuning.reconciler_interval_ms, 1_000);
    assert_eq!(tuning.ingest_disconnect_grace_ms, 5_000);
    assert_eq!(tuning.session_prune_every_ticks(), 3_600);
    assert_eq!(tuning.output_max_retries, 10);
    // Equal jitter halves the deterministic ceiling: backoff lands in
    // [ceiling / 2, ceiling], never below half nor above the unjittered
    // value that used to be asserted exactly here.
    let backoff_1 = tuning.output_backoff_ms(1, "output-a");
    assert!(
        (5_000..=10_000).contains(&backoff_1),
        "expected [5000, 10000], got {backoff_1}"
    );
    let backoff_6 = tuning.output_backoff_ms(6, "output-a");
    assert!(
        (150_000..=300_000).contains(&backoff_6),
        "expected [150000, 300000], got {backoff_6}"
    );
    assert_eq!(tuning.hls_idle_timeout_ms, 60_000);
}

#[test]
fn runtime_tuning_prune_cadence_tracks_reconciler_interval() {
    let tuning = RuntimeTuning {
        reconciler_interval_ms: 250,
        ..RuntimeTuning::default()
    };

    assert_eq!(tuning.session_prune_every_ticks(), 14_400);
}

#[test]
fn output_job_ids_are_unique_per_attempt() {
    let first = next_output_job_id("out-1");
    let second = next_output_job_id("out-1");

    assert!(first.starts_with("job_out-1_"));
    assert!(second.starts_with("job_out-1_"));
    assert_ne!(first, second);
}
