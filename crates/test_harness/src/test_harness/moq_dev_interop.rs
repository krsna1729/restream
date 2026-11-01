//! `moq-dev-interop`: Restream's MoQ egress, origin and ingest against
//! moq.dev tools: moq-lite-06 + hang by default, MOQT + MSF/LOC on draft 16,
//! 18 or 19 with `--ietf <draft>`.
//!
//! The peers are the moq.dev release binaries built from the commit of
//! `moq-net` 0.3.15, which Restream's MoQ code is pinned to: `moq` (moq-cli
//! 0.14.7) and `moq-relay` 0.17.7, installed by
//! `cargo xtask fetch moq-dev` under `.local/bin/` with their release
//! SHA256s checked. Each run generates its own CA and ECDSA P-256 leaf
//! (13 days, SANs `localhost` and `127.0.0.1`); the relay and Restream's MoQ
//! listener both serve the leaf, the CLI trusts the CA, browsers pin the
//! leaf's SHA-256 through `serverCertificateHashes`. CLI sessions offer only
//! the selected version; browser sessions continue to exercise moq-lite-06.
//!
//! Phases, each on the real delivery path:
//!
//! 1. **Egress.** An FFmpeg RTMP publisher (checked-in B-frame-free fixture)
//!    feeds one pipeline with a `moqt://` output to moq-relay (`broadcast=`
//!    names the announced path, `ca=` the CA) and an RTMP sibling to the
//!    harness's in-process sink. The relay must announce the broadcast with an
//!    H.264 + AAC catalog (MSF/LOC with `--ietf`); `moq export ts` reads it back for at least
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
//!    restarted. The MoQ output must publish again on the pinned version, and the
//!    RTMP sibling must keep delivering: no video gap above one second and
//!    no reconnect.
//! 5. **Ingest.** moq-dev's TS publisher recipe (`ffmpeg … | moq import ts`)
//!    publishes `live/<stream key>.hang` into Restream's MoQ listener. The
//!    pipeline must report a `moq` session with video and audio, and its
//!    RTMP output, received by an FFmpeg RTMP listener, passes the FFprobe
//!    checks.
//!    With `--ietf`, a separate source pipeline publishes MSF/LOC to a
//!    dedicated moq-relay, whose cluster dial forwards the broadcast into
//!    Restream on the same draft. No export/re-import bridge bypasses the relay.
//!
//! ```text
//! cargo xtask fetch moq-dev
//! npm ci --prefix test/e2e/moq && npx --prefix test/e2e/moq playwright install chromium firefox
//! cargo xtask harness moq-dev-interop -- --no-netns [--ietf 16|18|19] [--browsers]
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
pub(crate) use peers::*;
use run::*;

use super::*;
use crate::moq_relay_interop::{
    Checks, InteropTls, SIBLING_MAX_STALL, fixture_frame_rate, longest_counter_stall,
    planned_harness_ports, round3, sample_sibling,
};

const USAGE: &str = "usage: moq-dev-interop [--no-netns] [--ietf 16|18|19] [--browsers]";

/// What the command line asks for beyond the harness's own `--no-netns`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DevOptions {
    /// Run the Playwright browser specs while the endpoints are live.
    pub(crate) browsers: bool,
    /// Pin native sessions to this IETF MOQT draft and exercise MSF/LOC
    /// through the relay; moq-lite-06 + hang without it.
    pub(crate) ietf: Option<IetfDraft>,
}

impl DevOptions {
    pub(crate) fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = strip_netns_opt(args).into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--browsers" => options.browsers = true,
                "--ietf" => {
                    let draft = args.next().unwrap_or_default();
                    options.ietf = Some(IetfDraft::parse(&draft).ok_or_else(|| {
                        format!("moq-dev-interop: --ietf takes a draft, 16, 18 or 19 (got {draft:?}); {USAGE}")
                    })?);
                }
                other => {
                    return Err(format!(
                        "moq-dev-interop: unknown option {other:?}; {USAGE}"
                    ));
                }
            }
        }
        Ok(options)
    }

    /// The CLI and relay-log spelling of the pinned version.
    pub(crate) fn version(self) -> &'static str {
        self.ietf.map_or(LITE_VERSION, IetfDraft::version)
    }

    /// The native ALPN, as a Restream `moqt://` output's `version=` names it.
    pub(crate) fn alpn(self) -> &'static str {
        self.ietf.map_or(LITE_VERSION, IetfDraft::alpn)
    }
}

/// Entry point for the `moq-dev-interop` command.
pub(crate) async fn moq_dev_interop(args: &[String]) -> Result<Value, String> {
    run_moq_dev_interop(DevOptions::parse(args)?).await
}
