//! `hls-push-duty`: the harness mode that drives HLS push (and pull) ingest.
//!
//! The mode exists for two purposes: it is the Q-067 harness gate for HLS
//! push ingest, and it is the treatment arm of an executor-isolation
//! measurement — does adding HLS ingest sessions degrade the existing
//! RTMP/SRT ingest-and-egress guarantees?
//!
//! All arms are the same run with the same base load, and they differ only
//! by the HLS addition:
//!
//! - **Baseline arm** (`--hls-publishers 0`, the default): `--base-publishers`
//!   RTMP publishers, each an ingest input and one RTMP sink output.
//! - **Treatment arm** (`--hls-publishers N`): the same base load plus `N`
//!   pipelines whose input is an HLS push session driven by FFmpeg
//!   (`-f hls -method PUT`, or POST with `--method post`), each with the same
//!   RTMP sink output.
//! - **Pull arm** (`--pull-sessions N`): the same base load plus one
//!   `pull-source` RTMP pipeline and `N` pipelines pulling its HLS preview
//!   through a selected `hlsPull` input, each with the same RTMP sink output.
//!
//! Every pipeline's output goes to its own in-process RTMP sink, so delivery
//! is asserted per pipeline and the egress side of the fabric carries real
//! work. The result JSON carries the numbers the isolation comparison needs:
//! per-pipeline published packet counts and executor CPU per poll, the media
//! executor's queue-latency max and per-class window (thread CPU per poll,
//! polls above 1/4/16 ms of CPU per session-minute), the host's memory
//! management counters over the window, per-output egress resync/stall/drop
//! counters, the wall-clock duration and the host CPU count.
//!
//! ```text
//! scripts/harness/run.sh hls-push-duty
//! scripts/harness/run.sh hls-push-duty -- --hls-publishers 4 --segment-type fmp4
//! scripts/harness/run.sh hls-push-duty -- --hls-publishers 2 --method post
//! scripts/harness/run.sh hls-push-duty -- --pull-sessions 4 --poll-usage
//! ```
//!
//! FFmpeg is resolved through the project's `ffmpeg_binary` resolver; the
//! media comes from the checked-in transport fixture contract.

#[path = "hls_push_duty/host_memory.rs"]
pub(super) mod host_memory;
#[path = "hls_push_duty/metrics.rs"]
pub(super) mod metrics;
#[path = "hls_push_duty/options.rs"]
pub(super) mod options;
#[path = "hls_push_duty/run.rs"]
pub(super) mod run;
#[cfg(test)]
#[path = "hls_push_duty/tests.rs"]
mod tests;

use metrics::*;
use options::*;
use run::*;

use super::*;

/// Entry point for the `hls-push-duty` command.
pub(crate) async fn hls_push_duty(args: &[String]) -> Result<Value, String> {
    // `--no-netns` is consumed by the harness root, not the mode.
    let args = strip_netns_opt(args);
    let options = HlsPushOptions::from_args(&args)?;
    run_hls_push_duty(options, &args).await
}
