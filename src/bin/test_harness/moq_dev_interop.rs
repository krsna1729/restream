//! `moq-dev-interop`: Restream's MoQ egress, origin and ingest against the
//! moq.dev tools, the phase-1 profile (moq-lite-06 + hang).
//!
//! The peers are the moq.dev release binaries built from the commit of
//! `moq-net` 0.3.15, which Restream's MoQ code is pinned to: `moq` (moq-cli
//! 0.14.7) and `moq-relay` 0.17.7, installed by
//! `scripts/dev/moq/fetch-moq-dev.sh` under `.local/bin/` with their release
//! SHA256s checked. Each run generates its own CA and ECDSA P-256 leaf
//! (13 days, SANs `localhost` and `127.0.0.1`); the relay and Restream's MoQ
//! listener both serve the leaf, the CLI trusts the CA, browsers pin the
//! leaf's SHA-256 through `serverCertificateHashes`. Every CLI session offers
//! moq-lite-06 only and the relay log must show it negotiated.
//!
//! Phases, each on the real delivery path:
//!
//! 1. **Egress.** An FFmpeg RTMP publisher (checked-in B-frame-free fixture)
//!    feeds one pipeline with a `moqt://` output to moq-relay (`broadcast=`
//!    names the announced path, `ca=` the CA) and an RTMP sibling to the
//!    harness's in-process sink. The relay must announce the broadcast with an
//!    H.264 + AAC hang catalog; `moq export ts` reads it back for at least
//!    20 s and FFprobe checks both codecs, monotonic timestamps and a video
//!    frame count within 5 % of the input frame rate.
//! 2. **Origin.** The harness mints a subscribe token through the API and
//!    `moq export ts` subscribes to the same pipeline on Restream's own MoQ
//!    listener with it; same checks over a shorter window. Without the
//!    token, or with a tampered one, the session must be refused.
//! 3. **Browsers** (`--browsers`). Playwright Chromium and Firefox play the
//!    origin and the relay with `@moq/watch` (`test/e2e/moq/`), and the
//!    origin must refuse the page without its token.
//! 4. **Isolation.** Ten seconds later the relay is killed (SIGKILL) and
//!    restarted. The MoQ output must publish again on moq-lite-06, and the
//!    RTMP sibling must keep delivering: no video gap above one second and
//!    no reconnect.
//! 5. **Ingest.** moq-dev's TS publisher recipe (`ffmpeg … | moq import ts`)
//!    publishes `live/<stream key>.hang` into Restream's MoQ listener. The
//!    pipeline must report a `moq` session with video and audio, and its
//!    RTMP output, received by an FFmpeg RTMP listener, passes the FFprobe
//!    checks.
//!
//! ```text
//! scripts/dev/moq/fetch-moq-dev.sh
//! npm ci --prefix test/e2e/moq && npx --prefix test/e2e/moq playwright install chromium firefox
//! scripts/harness/run.sh moq-dev-interop -- --no-netns [--browsers]
//! ```

#[path = "moq_dev_interop/checks.rs"]
mod checks;
#[path = "moq_dev_interop/peers.rs"]
mod peers;
#[path = "moq_dev_interop/run.rs"]
mod run;
#[cfg(test)]
#[path = "moq_dev_interop/tests.rs"]
mod tests;

use checks::*;
use peers::*;
use run::*;

use super::*;
use crate::moq_relay_interop::{
    Checks, InteropTls, SIBLING_MAX_STALL, fixture_frame_rate, longest_counter_stall,
    planned_harness_ports, round3, sample_sibling,
};

const USAGE: &str = "usage: moq-dev-interop [--no-netns] [--browsers]";

/// What the command line asks for beyond the harness's own `--no-netns`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DevOptions {
    /// Run the Playwright browser specs while the endpoints are live.
    pub(crate) browsers: bool,
}

impl DevOptions {
    pub(crate) fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        for arg in strip_netns_opt(args) {
            match arg.as_str() {
                "--browsers" => options.browsers = true,
                other => {
                    return Err(format!(
                        "moq-dev-interop: unknown option {other:?}; {USAGE}"
                    ));
                }
            }
        }
        Ok(options)
    }
}

/// Entry point for the `moq-dev-interop` command.
pub(crate) async fn moq_dev_interop(args: &[String]) -> Result<Value, String> {
    run_moq_dev_interop(DevOptions::parse(args)?).await
}
