//! `egress-duty`: live Restream SRT egress, media pipeline included, against
//! the independent pinned upstream receiver process.
//!
//! One Restream process fans an 8 Mbps SRT ingress into plaintext or bounded
//! AES-128/AES-256 SRT outputs. WI3.6 remains the legacy single-shard arm;
//! WI3.7 enables exact 1..=4 shard requests with `RESTREAM_BENCH_FEATURES`
//! and records the actual `/metrics/system` topology.
//!
//! Environment knobs (all `EGRESS_DUTY_`-prefixed):
//!
//! | knob | default |
//! |---|---|
//! | `OUTPUTS` | 11 |
//! | `DEST_BASE` | `$RESOURCE_SWEEP_SRT_PEER_HOSTS`, else `10.53.1.1` |
//! | `PORT_BASE` | 12000 |
//! | `WINDOW_SECS` | 30 |
//! | `SHARD_CPUS` | `SHARD_CPU`, else `0`; one CPU per WI3.7 shard index |
//! | `REQUESTED_SHARDS` | unset; WI3.7 exact count, valid values 1..=4 |
//! | `PEER_CPUS` | `2-5` |
//! | `HARNESS_CPUS` | 1 |
//! | `RESTREAM_CPUS` | every CPU except `SHARD_CPUS` |
//! | `CRYPTO` | `plain`; bounded cells also accept `128` and `256` |
//! | `CAPACITY_MODE` | false; permits retransmit/duplicate quality outputs |
//! | `RECEIVER_BIN` | the pinned `target/release/srt-bench` |
//! | `RECEIVER_SECS` | `WINDOW_SECS + 40`, capped at 70 |
//! | `RECEIVER_LATENCY_MS` | 120 |
//! | `RECEIVER_QUEUE_HORIZON_MS` | unset; receiver default |
//! | `RESTREAM_BIN` | `target/bench/restream` |
//! | `NETNS` | `$RESTREAM_BENCH_NETNS` |
//! | `WORK_DIR` | harness artifact dir + `/egress-duty` |
//! | `BITRATE` | `8M` |
//! | `PROGRESS_TIMEOUT_SECS`, `DRAIN_SETTLE_SECS`, `EGRESS_SHARDS` | 45, 8, 1 |
//!
//! The receiver has no HTTP endpoint and no per-interval output. Its whole-run
//! totals are reconciled after the rated window; receiver and kernel drop
//! counters remain apparatus-validity fences, while retransmits and duplicates
//! are measured quality outputs in capacity mode.
//! The receiver has **no HTTP endpoint and no per-interval output**: stdout is
//! `LISTENING` then one final `STATS` line, and `--out` appends exactly one
//! aggregate TSV row per process at teardown. So the mode never polls it, waits
//! only for its exit, and reconciles on **whole-run** totals — its receiver
//! fence is whole-run, not window-only, because the receiver cannot delimit or
//! reset a window. `duration_secs` is only the receiver's own backstop; the
//! rated window is delimited by this harness, which stops the receiver with
//! `SIGTERM` (its clean-stop path, which writes the row).
//!
//! It measures; it does not optimize. Every rejected attempt is preserved:
//! the partial artifact is written before the error is returned.
//!
//! SRT needs a *symmetric* destination address: the receiver is wildcard-bound
//! inside the lane namespace, so its replies carry the namespace's own address.
//! A caller pointed at any other address in the namespace-local `/16` (the
//! `10.53.1.1` in the hand-off note) never completes the handshake — measured
//! with the upstream sender itself against the upstream receiver — so the
//! default destination is the lane peer address from
//! `RESOURCE_SWEEP_SRT_PEER_HOSTS`, and only that default is what makes the
//! rated window measurable on this lane.

#[path = "egress_duty/config.rs"]
pub(super) mod config;
#[path = "egress_duty/cpu.rs"]
pub(super) mod cpu;
#[path = "egress_duty/engine.rs"]
pub(super) mod engine;
#[path = "egress_duty/receiver.rs"]
pub(super) mod receiver;
#[path = "egress_duty/run.rs"]
pub(super) mod run;
#[cfg(test)]
#[path = "egress_duty/tests.rs"]
mod tests;
#[path = "egress_duty/verdict.rs"]
pub(super) mod verdict;

use config::*;
use cpu::*;
use engine::*;
use receiver::*;
use run::*;
use verdict::*;

use super::*;

pub(crate) async fn egress_duty() -> Result<Value, String> {
    let cfg = EgressDutyConfig::from_env()?;
    std::fs::create_dir_all(&cfg.work_dir).map_err(|error| error.to_string())?;
    let mut artifact = Artifact::new(&cfg);
    println!(
        "[egress-duty] {}",
        json!({
            "phase": "config",
            "outputs": cfg.outputs,
            "windowSecs": cfg.window_secs,
            "destBase": cfg.dest_base,
            "portBase": cfg.port_base,
            "shardCpus": cfg.shard_cpus.iter().collect::<Vec<_>>(),
            "requestedShards": cfg.requested_shards,
            "crypto": cfg.crypto,
            "capacityMode": cfg.capacity_mode,
            "restreamCpus": cfg.restream_mask(),
            "netns": cfg.netns,
        })
    );
    match run_duty(&cfg, &mut artifact).await {
        Ok(()) => {
            let verdict = artifact.value["verdict"].clone();
            println!(
                "[egress-duty] {}",
                json!({"phase": "done", "verdict": verdict})
            );
            Ok(artifact.value.take())
        }
        Err(error) => {
            artifact.reject(&error);
            Err(error)
        }
    }
}
