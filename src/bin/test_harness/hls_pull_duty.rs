//! `hls-pull-duty`: the harness mode that drives HLS **pull** ingest.
//!
//! The mode exists to prove Q-068 end to end against the product's own output
//! rather than a synthetic origin: a source pipeline is fed by a real RTMP
//! publisher, and a second pipeline pulls that source's own HLS preview
//! (`/hls/{pipeline_id}/master.m3u8`) through the API's `hlsPull` input with
//! the harness's session cookie presented as a `headers` credential.
//!
//! The checks are the gate, so each one names the failure it exists for:
//!
//! - the pull input is created and the source URL read back is the one sent;
//! - the pull pipeline's ingest session registers with protocol `hls-pull`;
//! - media reaches the pull pipeline's ring and its sink delivers it;
//! - the pulled media clock keeps up with the wall clock;
//! - the audio gap is reported, not hidden: the pull follows one rendition, so
//!   the preview master's separate `EXT-X-MEDIA` audio rendition is not
//!   followed. The pull states that once per pull and the mode records it as
//!   the report's `audioGap` finding (`reported`, the log line it quotes, the
//!   probe's audio-track count and the sink's audio count). It is a finding,
//!   never a check: audio cannot reach this pipeline until multi-rendition
//!   following exists;
//! - a pull pointed at an unresolvable URL reports the failure (a `WARN` line
//!   naming the input in the engine log) instead of idling silently, and does
//!   not stop its pipeline;
//! - the reconciler-owned lifecycle: disabling the input through the API stops
//!   the pull, and re-enabling it starts the pull again.
//!
//! ```text
//! scripts/harness/run.sh hls-pull-duty
//! scripts/harness/run.sh hls-pull-duty -- --duration-secs 20
//! scripts/harness/run.sh hls-pull-duty -- --fail-unresolvable   # must fail
//! ```
//!
//! The `--fail-unresolvable` run points the *good* pull at an unresolvable
//! URL; the session, delivery and keep-up checks then cannot pass, so the mode
//! reports `passed: false`. That is the mutation that keeps the mode from
//! passing vacuously.

#[path = "hls_pull_duty/metrics.rs"]
pub(super) mod metrics;
#[path = "hls_pull_duty/options.rs"]
pub(super) mod options;
#[path = "hls_pull_duty/run.rs"]
pub(super) mod run;
#[cfg(test)]
#[path = "hls_pull_duty/tests.rs"]
mod tests;

use metrics::*;
use options::*;
use run::*;

use super::*;

/// Entry point for the `hls-pull-duty` command.
pub(crate) async fn hls_pull_duty(args: &[String]) -> Result<Value, String> {
    // `--no-netns` is consumed by the harness root, not the mode.
    let args = strip_netns_opt(args);
    let options = HlsPullOptions::from_args(&args)?;
    run_hls_pull_duty(options, &args).await
}
