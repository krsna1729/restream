//! Concurrency proof gates (`docs/concurrency-proofing.md`): loom models,
//! focused lifecycle regressions, and, for `contract`, live fault and
//! recovery harness runs with a guard that no media process outlives them.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use crate::{StepResult, capture_into, cargo, child_output, history_grouping, run};

/// `tests/<target>.rs` loom models, each built with `--cfg loom`.
const LOOM_TARGETS: &[&str] = &[
    "avio_loom",
    "egress_feed_wake_loom",
    "input_selection_loom",
    "ring_migration_loom",
    "ts_chunk_ring_loom",
    "ts_muxer_stage_loom",
    "transcoder_stage_loom",
];

/// `(label, cargo args)` shared by `fast` and `contract`; one test per row.
#[rustfmt::skip]
const COMMON_TESTS: &[(&str, &[&str])] = &[
    ("api-health", &["test", "health_endpoint_exposes_probe_and_egress_fault_fields", "--test", "api", "--", "--nocapture"]),
    ("api-output-recent-failure", &["test", "output_status_and_health_preserve_recent_egress_failure_after_unregister", "--test", "api", "--", "--nocapture"]),
    ("api-output-restart-retry", &["test", "active_output_status_ignores_stale_retry_state_after_restart", "--test", "api", "--", "--nocapture"]),
    ("output-status-active", &["test", "active_output_status_matches_health_runtime_fields", "--test", "output_status_contract", "--", "--nocapture"]),
    ("output-status-stalled", &["test", "stalled_output_status_matches_health_runtime_fields", "--test", "output_status_contract", "--", "--nocapture"]),
    ("api-disconnect-clears", &["test", "health_endpoint_clears_recent_disconnect_details_after_reconnect", "--test", "api", "--", "--nocapture"]),
    ("api-disconnect-flapping", &["test", "health_endpoint_surfaces_repeated_transient_disconnects_as_flapping", "--test", "api", "--", "--nocapture"]),
    ("api-egress-flapping", &["test", "recovered_output_surfaces_flapping_after_repeated_sink_failures", "--test", "api", "--", "--nocapture"]),
    ("db-stale-job-update", &["test", "stale_job_update_cannot_clobber_replacement_attempt", "--test", "db", "--", "--nocapture"]),
    ("db-multiple-stale-job-updates", &["test", "multiple_stale_job_updates_cannot_clobber_newest_attempt", "--test", "db", "--", "--nocapture"]),
    ("lib-stale-ingest-unregister", &["test", "stale_ingest_unregister_cannot_clobber_replacement_attempt", "--lib", "--", "--nocapture"]),
    ("lib-stale-ingest-disconnect", &["test", "stale_ingest_disconnect_cannot_poison_replacement_attempt", "--lib", "--", "--nocapture"]),
    ("lib-stale-egress-unregister", &["test", "stale_egress_unregister_cannot_clobber_replacement_attempt", "--lib", "--", "--nocapture"]),
    ("lib-stale-egress-error", &["test", "stale_egress_error_cannot_poison_replacement_attempt", "--lib", "--", "--nocapture"]),
    ("lib-stale-egress-queue", &["test", "stale_egress_queue_removal_cannot_drop_replacement_queue", "--lib", "--", "--nocapture"]),
    ("ring-proptest", &["test", "prop_no_loss_no_gap_no_duplication", "--test", "ring_migration", "--", "--nocapture"]),
    ("ring-multi-reader-proptest", &["test", "prop_multi_reader_migration_preserves_each_reader_order", "--test", "ring_migration", "--", "--nocapture"]),
    ("input-selection-proptest", &["test", "gate_matches_sequential_selection_model", "--test", "input_selection", "--", "--nocapture"]),
    ("standby-gop-proptest", &["test", "cache_never_exceeds_its_declared_limits", "--test", "standby_gop", "--", "--nocapture"]),
    ("lib-avio-batch", &["test", "write_batch_round_trips_random_chunks", "--lib", "--", "--nocapture"]),
    ("lib-avio-unit", &["test", "media::avio::tests", "--lib", "--", "--nocapture"]),
    ("lib-srt-stream-id-normalization", &["test", "media::srt_stream_id::tests", "--lib", "--", "--nocapture"]),
    ("lib-srt-ingress-owner", &["test", "media::srt::ingress_live_tests", "--lib", "--", "--nocapture"]),
    ("lib-srt-ingress-bridges", &["test", "media::srt::ingress_bridge_tests", "--lib", "--", "--nocapture"]),
    ("lib-srt-ingress-admission", &["test", "media::srt::ingress_admission", "--lib", "--", "--nocapture"]),
    ("external-transcoder-routing", &["test", "external_output_stream_idx_routes_known_tracks_without_aliasing", "--lib", "--", "--nocapture"]),
    ("external-transcoder-routing-proptest", &["test", "proptest_external_output_dts_routing_preserves_per_stream_monotonicity", "--lib", "--", "--nocapture"]),
    ("external-transcoder-h264-live", &["test", "external_720p_stage_emits_live_packets_for_h264_marker_fixture", "--lib", "--", "--nocapture"]),
    ("external-transcoder-h264-dts-remux", &["test", "external_1080p_stage_remuxes_marker_fixture_with_monotone_dts", "--lib", "--", "--nocapture"]),
    ("internal-transcoder-chunked-scale", &["test", "internal_scale_stage_chunked_remux_input_preserves_video_timestamp_order", "--test", "transcoder", "--", "--nocapture"]),
    ("internal-transcoder-source-proptest", &["test", "prop_source_stage_chunked_input_preserves_per_stream_dts_order", "--test", "transcoder", "--", "--nocapture"]),
    ("internal-transcoder-replacement-metadata", &["test", "replacement_video_stage_preserves_codec_hint_and_audio_tracks", "--test", "transcoder", "--", "--nocapture"]),
    ("hls-segment-dts-boundaries", &["test", "hls_segment_boundaries_preserve_non_decreasing_dts_per_stream", "--lib", "--", "--nocapture"]),
    ("recording-remux-continuity-retention-disabled", &["test", "remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_disabled", "--lib", "--", "--nocapture"]),
    ("recording-remux-continuity-retention-enabled", &["test", "remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_enabled", "--lib", "--", "--nocapture"]),
    ("test-harness-process-lifecycle", &["test", "--bin", "test_harness", "tests::kill_and_wait_child_terminates_spawned_process", "--", "--exact", "--nocapture"]),
    ("rtmp-ingress-listener-shutdown", &["test", "compio_rtmp_listener_shutdown_joins_acceptor_and_session_workers", "--lib", "--", "--nocapture"]),
    ("rtmp-sharded-ingress-lifecycle", &["test", "media::rtmp::listener::tests", "--lib"]),
    ("lib-compio-rtmp-readiness-fairness", &["test", "media::egress::backends::compio_tcp::tests", "--lib", "--", "--nocapture"]),
    ("test-harness-slow-sink-sibling-count", &["test", "--bin", "test_harness", "tests::fault_output_stall_sibling_count_honors_n_per_group_cap", "--", "--exact", "--nocapture"]),
    ("lib-recent-egress", &["test", "recent_egress", "--lib", "--", "--nocapture"]),
    ("lib-ingest-grace", &["test", "recent_ingest_disconnect_respects_grace_window", "--lib", "--", "--nocapture"]),
    ("lib-ingest-flap-window", &["test", "build_recent_ingest_outcome_resets_flap_streak_outside_window", "--lib", "--", "--nocapture"]),
    ("lib-ingest-proptest", &["test", "prop_ingest_lifecycle_preserves_health_invariants", "--lib", "--", "--nocapture"]),
    ("lib-egress-flap-window", &["test", "build_recent_egress_outcome_resets_flap_streak_outside_window", "--lib", "--", "--nocapture"]),
    ("lib-health-reconnect-flapping", &["test", "health_snapshot_surfaces_flapping_after_repeated_reconnects", "--lib", "--", "--nocapture"]),
    ("lib-health-egress-flapping", &["test", "health_snapshot_surfaces_flapping_after_repeated_egress_recoveries", "--lib", "--", "--nocapture"]),
    ("lib-late-retry-state", &["test", "late_retry_state_update_is_ignored_after_output_restarts", "--lib", "--", "--nocapture"]),
    ("lib-multi-late-retry-state", &["test", "repeated_late_retry_updates_cannot_poison_newest_output_attempt", "--lib", "--", "--nocapture"]),
    ("lib-output-retry-backoff", &["test", "output_status_surfaces_retry_backoff_after_failure", "--lib", "--", "--nocapture"]),
    ("lib-egress-proptest", &["test", "prop_egress_lifecycle_preserves_runtime_and_health_invariants", "--lib", "--", "--nocapture"]),
    ("lib-egress-leaf-cursor-priming", &["test", "first_visit_primes", "--lib", "--", "--nocapture"]),
    ("lib-egress-leaf-live-start", &["test", "fresh_leaf_first_visit", "--lib", "--", "--nocapture"]),
    ("recording-drain-bounded-on-cancel", &["test", "media::recording::tests::drain_ready_bursts", "--lib", "--", "--nocapture"]),
    ("lib-media-executor", &["test", "media::executor::tests", "--lib"]),
    ("lib-egress-sizing", &["test", "media::egress::sizing", "--lib"]),
    ("lib-egress-resize", &["test", "media::egress::runtime", "--lib"]),
    ("lib-media-control-isolation", &["test", "while_control_thread_is_blocked", "--lib"]),
    ("lib-media-file-ingest", &["test", "media::external_file_ingest::tests", "--lib"]),
    ("recording-media-owner-abort", &["test", "aborting_control_owner_closes_media_feeder_and_writer", "--lib"]),
    ("hls-media-owner-abort", &["test", "control_owner_abort_flushes_final_segment", "--lib"]),
    ("recording-media-writer-failure", &["test", "recording_media_writer_failure_reports_failed_without_finalization", "--lib"]),
    ("hls-media-replacement", &["test", "detached_teardown_preserves_replacement", "--lib"]),
];

/// Run by `fast` only, after the shared set.
#[rustfmt::skip]
const FAST_ONLY_TESTS: &[(&str, &[&str])] = &[
    ("lib-rtmp-feed-wake-after-media", &["test", "feed_wake_delivers_media_after_idle_when_factory_start_is_delayed", "--lib", "--", "--nocapture"]),
    ("lib-srt-ingress-two-owners", &["test", "two_owners_share_the_port_and_each_session_command_reaches_its_owner", "--lib", "--", "--nocapture"]),
    ("health-view", &["test", "api_runtime_views::status::tests::health", "--lib"]),
    ("test-harness-unit", &["test", "--bin", "test_harness", "--", "--nocapture"]),
];

/// Live harness modes run by `contract`, each with its own work directory.
const CONTRACT_MODES: &[(&str, &str)] = &[
    ("fault.resilience", ".local/artifacts/concurrency-contract"),
    (
        "fault.egress-retry",
        ".local/artifacts/concurrency-fault-egress-retry",
    ),
    (
        "fault.output-stall",
        ".local/artifacts/concurrency-fault-output-stall",
    ),
    ("recovery", ".local/artifacts/concurrency-recovery"),
];

/// Process names a harness run may start; none may survive it.
const RUNTIME_PROCESSES: &[&str] = &["restream", "mediamtx", "ffmpeg", "ffprobe", "test_harness"];

const LOG_DIR: &str = ".local/artifacts/concurrency-contract-logs";

pub(crate) fn fast() -> StepResult {
    for target in LOOM_TARGETS {
        loom(target)?;
    }
    for (_, args) in COMMON_TESTS.iter().chain(FAST_ONLY_TESTS) {
        cargo(args)?;
    }
    Ok(())
}

pub(crate) fn contract() -> StepResult {
    fs::create_dir_all(LOG_DIR).map_err(|error| format!("cannot create {LOG_DIR}: {error}"))?;
    let baseline: HashSet<u32> = runtime_processes().into_iter().map(|row| row.pid).collect();
    let result = contract_steps(&baseline);
    // Like the shell trap on EXIT: clean up after success and failure alike.
    stop_new_processes(&baseline);
    result
}

fn contract_steps(baseline: &HashSet<u32>) -> StepResult {
    logged("history-grouping", history_grouping)?;
    logged("process-lifecycle-guards", || {
        process_lifecycle_guards(Path::new("src/bin/test_harness.rs"))
    })?;
    for target in LOOM_TARGETS {
        logged(&format!("loom-{target}"), || loom(target))?;
    }
    for (label, args) in COMMON_TESTS {
        logged(label, || cargo(args))?;
    }
    logged("build-harness-bins", || {
        cargo(&["build", "--bin", "restream", "--bin", "test_harness"])
    })?;
    let extra = std::env::var("CONCURRENCY_HARNESS_ARGS").unwrap_or_default();
    for (mode, work_dir) in CONTRACT_MODES {
        stop_new_processes(baseline);
        let mut args = vec![*mode];
        args.extend(extra.split_whitespace());
        let outcome = logged(mode, || {
            run(
                "target/debug/test_harness",
                &args,
                &[
                    ("RESTREAM_BIN", "target/debug/restream"),
                    ("WORK_DIR", work_dir),
                ],
            )
        });
        stop_new_processes(baseline);
        let label = if outcome.is_ok() {
            mode.to_string()
        } else {
            format!("{mode} failure cleanup")
        };
        assert_no_new_processes(baseline, &label)?;
        outcome?;
    }
    Ok(())
}

/// Runs `step` with every child's stdout and stderr in
/// `LOG_DIR/<label>.log`, printing the log only when the step fails.
fn logged(label: &str, step: impl FnOnce() -> StepResult) -> StepResult {
    let path = PathBuf::from(LOG_DIR).join(format!("{label}.log"));
    let log = fs::File::create(&path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    let result = capture_into(log, step);
    if result.is_err()
        && let Ok(text) = fs::read_to_string(&path)
    {
        print!("{text}");
    }
    result.map_err(|error| format!("{label}: {error}"))
}

/// Every child spawn in the harness must arm `kill_on_drop(true)` first, and
/// the shared `kill_and_wait_child` teardown helper must exist.
fn process_lifecycle_guards(harness: &Path) -> StepResult {
    let text = fs::read_to_string(harness)
        .map_err(|error| format!("cannot read {}: {error}", harness.display()))?;
    let unarmed = unarmed_spawns(&text);
    if let Some(line) = unarmed.first() {
        return Err(format!(
            "process lifecycle guard failed: spawn without preceding kill_on_drop(true) near {}:{line}",
            harness.display()
        ));
    }
    if !text.contains("async fn kill_and_wait_child") {
        return Err(format!(
            "process lifecycle guard failed: missing kill_and_wait_child helper in {}",
            harness.display()
        ));
    }
    Ok(())
}

/// 1-based lines with a `.spawn()` not preceded by `kill_on_drop(true)` since
/// the previous spawn (the arming check also counts the spawn line itself).
fn unarmed_spawns(text: &str) -> Vec<usize> {
    let mut armed = false;
    let mut unarmed = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.contains("kill_on_drop(true)") {
            armed = true;
        }
        if line.contains(".spawn()") {
            if !armed {
                unarmed.push(index + 1);
            }
            armed = false;
        }
    }
    unarmed
}

pub(crate) fn loom(target: &str) -> StepResult {
    eprintln!("==> loom {target}");
    let (_, stderr) = child_output()?;
    let output = Command::new("cargo")
        .args([
            "rustc",
            "--test",
            target,
            "--message-format=json-render-diagnostics",
            "--",
            "--cfg",
            "loom",
        ])
        .stderr(stderr)
        .output()
        .map_err(|error| format!("cannot start cargo rustc: {error}"))?;
    if !output.status.success() {
        return Err(format!("loom build of {target} failed ({})", output.status));
    }
    let binary = loom_executable(&String::from_utf8_lossy(&output.stdout), target)
        .ok_or_else(|| format!("failed to locate compiled loom binary for {target}"))?;
    run(&binary, &["--nocapture"], &[])
}

/// The last `compiler-artifact` executable built for `target`.
fn loom_executable(messages: &str, target: &str) -> Option<String> {
    messages
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| {
            message["reason"] == "compiler-artifact" && message["target"]["name"] == target
        })
        .filter_map(|message| message["executable"].as_str().map(str::to_owned))
        .next_back()
}

struct RuntimeProcess {
    pid: u32,
    row: String,
}

fn runtime_processes() -> Vec<RuntimeProcess> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(comm) = fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        let comm = comm.trim_end();
        if !RUNTIME_PROCESSES.contains(&comm) {
            continue;
        }
        let args = fs::read(entry.path().join("cmdline"))
            .map(|raw| {
                String::from_utf8_lossy(&raw)
                    .replace('\0', " ")
                    .trim_end()
                    .to_owned()
            })
            .unwrap_or_default();
        rows.push(RuntimeProcess {
            pid,
            row: format!("{pid} {comm} {args}"),
        });
    }
    rows
}

fn new_processes(baseline: &HashSet<u32>) -> Vec<RuntimeProcess> {
    runtime_processes()
        .into_iter()
        .filter(|process| !baseline.contains(&process.pid))
        .collect()
}

/// TERM, then KILL, any runtime process this run started; never touches a
/// process that was already running when the gate began.
fn stop_new_processes(baseline: &HashSet<u32>) {
    for signal in ["-TERM", "-KILL"] {
        let pids: Vec<String> = new_processes(baseline)
            .iter()
            .map(|process| process.pid.to_string())
            .collect();
        if pids.is_empty() {
            return;
        }
        let _ = Command::new("kill")
            .arg(signal)
            .arg("--")
            .args(&pids)
            .stderr(Stdio::null())
            .status();
        for _ in 0..10 {
            if new_processes(baseline).is_empty() {
                return;
            }
            sleep(Duration::from_millis(500));
        }
    }
}

fn assert_no_new_processes(baseline: &HashSet<u32>, label: &str) -> StepResult {
    for _ in 0..10 {
        let survivors = new_processes(baseline);
        if survivors.is_empty() {
            return Ok(());
        }
        sleep(Duration::from_millis(500));
    }
    let rows: Vec<String> = new_processes(baseline)
        .into_iter()
        .map(|process| process.row)
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    Err(format!(
        "runtime cleanup guard failed after {label}:\n{}",
        rows.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_must_be_armed_since_the_previous_spawn() {
        let text = "\
let a = cmd.kill_on_drop(true);
a.spawn()?;
b.spawn()?;
c.kill_on_drop(true).spawn()?;
";
        assert_eq!(unarmed_spawns(text), vec![3]);
    }

    #[test]
    fn loom_executable_takes_last_matching_artifact() {
        let messages = r#"{"reason":"compiler-artifact","target":{"name":"other"},"executable":"/t/other"}
{"reason":"compiler-artifact","target":{"name":"avio_loom"},"executable":null}
{"reason":"build-finished","success":true}
{"reason":"compiler-artifact","target":{"name":"avio_loom"},"executable":"/t/avio_loom-1"}"#;
        assert_eq!(
            loom_executable(messages, "avio_loom").as_deref(),
            Some("/t/avio_loom-1")
        );
        assert_eq!(loom_executable(messages, "missing"), None);
    }
}
