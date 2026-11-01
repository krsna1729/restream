//! `moq-relay-interop`: diagnostic reproduction against unmodified MediaMTX.
//!
//! MediaMTX v1.21.2 has SETUP, namespace discovery and LOC dialect mismatches
//! documented with source/spec evidence in the backlog. This is not a passing
//! IETF gate; use `moq-dev-interop --ietf <draft>` for that. It runs the same pinned
//! MediaMTX as every other live mode (`scripts/data/mediamtx.tsv`). Each run generates its
//! own CA and ECDSA P-256 leaf with `openssl` and its own MediaMTX config with
//! native QUIC, RTSP and RTMP on harness-allocated loopback ports.
//!
//! The path under test is the real delivery path, in four phases:
//!
//! 1. **Egress.** An FFmpeg RTMP publisher (checked-in B-frame-free fixture)
//!    feeds one Restream pipeline with two outputs: `moqt://` to MediaMTX's
//!    native QUIC listener (the CA passed with `?ca=`) and an RTMP sibling to
//!    the harness's in-process sink. MediaMTX must show the path published by
//!    a MoQ session with H.264 and AAC; FFprobe reads it back over RTSP for at
//!    least 20 s and checks both codecs, monotonic timestamps and a video frame
//!    count within 5 % of the input frame rate.
//! 2. **Isolation.** Ten seconds later MediaMTX is killed (SIGKILL) and
//!    restarted. The MoQ output must publish again, and the RTMP sibling must
//!    keep delivering throughout: no video gap above one second and no
//!    reconnect.
//! 3. **Ingest.** FFmpeg publishes RTSP into a MediaMTX path whose `forward`
//!    pushes it over MoQ into Restream's ingest listener (the leaf's SHA-256
//!    as `destFingerprint`). The pipeline must report a `moq` session with
//!    video and audio, and its RTMP output, re-published into MediaMTX, passes
//!    the same FFprobe checks.
//!
//! ```text
//! cargo xtask fetch mediamtx
//! cargo xtask harness moq-relay-interop -- --no-netns
//! ```

#[path = "moq_relay_interop/checks.rs"]
mod checks;
#[path = "moq_relay_interop/relay.rs"]
mod relay;
#[path = "moq_relay_interop/run.rs"]
mod run;
#[cfg(test)]
#[path = "moq_relay_interop/tests.rs"]
mod tests;

use checks::*;
use relay::*;
use run::*;

// Shared with `moq-dev-interop`: the same TLS material, read-back verdicts,
// sibling sampling and check ledger serve both relays.
pub(crate) use checks::{longest_counter_stall, round3};
pub(crate) use relay::{InteropTls, planned_harness_ports};
pub(crate) use run::{Checks, SIBLING_MAX_STALL, fixture_frame_rate, sample_sibling};

use super::*;

/// Entry point for the `moq-relay-interop` command. The mode takes no
/// options of its own; `--no-netns` is consumed by the harness root.
pub(crate) async fn moq_relay_interop(args: &[String]) -> Result<Value, String> {
    let args = strip_netns_opt(args);
    if let Some(unknown) = args.first() {
        return Err(format!(
            "moq-relay-interop takes no options (got {unknown:?}); usage: moq-relay-interop [--no-netns]"
        ));
    }
    run_moq_relay_interop().await
}
