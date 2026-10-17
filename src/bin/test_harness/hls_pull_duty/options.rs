//! Parsed command-line options for the `hls-pull-duty` mode.
//!
//! Every knob has a default, so a bare `hls-pull-duty` is a short, standalone
//! run of the whole gate.

use std::time::Duration;

/// A source that cannot resolve (RFC 2606 reserves `.invalid`), so the
/// puller's fetch fails at name resolution and it must *report* the failure
/// rather than sit idle.
pub(crate) const UNRESOLVABLE_PULL_URL: &str = "http://hls-pull-duty.invalid/master.m3u8";

const MAX_DURATION_SECS: u64 = 600;
const MIN_RECONCILE_INTERVAL_MS: u64 = 100;
const MAX_RECONCILE_INTERVAL_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HlsPullOptions {
    /// Rated window, in seconds, after the pull session has registered.
    pub(crate) duration_secs: u64,
    /// Passed to the engine as `RESTREAM_RECONCILE_INTERVAL_MS`. The reconciler
    /// backstop is ten intervals, and the lifecycle checks depend on a pass,
    /// so lowering this is what keeps the run short.
    pub(crate) reconcile_interval_ms: u64,
    /// Point the good pull at [`UNRESOLVABLE_PULL_URL`] instead of the source
    /// preview: the failing variant, where the delivery checks must fail.
    pub(crate) fail_unresolvable: bool,
}

impl Default for HlsPullOptions {
    fn default() -> Self {
        Self {
            duration_secs: 15,
            reconcile_interval_ms: 1_000,
            fail_unresolvable: false,
        }
    }
}

pub(crate) const USAGE: &str =
    "hls-pull-duty [--duration-secs N] [--reconcile-interval-ms N] [--fail-unresolvable]";

impl HlsPullOptions {
    pub(crate) fn from_args(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut index = 0;
        while index < args.len() {
            let (name, inline) = match args[index].split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (args[index].as_str(), None),
            };
            let value = |index: &mut usize| -> Result<String, String> {
                if let Some(inline) = inline.clone() {
                    return Ok(inline);
                }
                *index += 1;
                args.get(*index)
                    .cloned()
                    .ok_or_else(|| format!("{name} requires a value"))
            };
            match name {
                "--duration-secs" => options.duration_secs = parse_u64(&value(&mut index)?, name)?,
                "--reconcile-interval-ms" => {
                    options.reconcile_interval_ms = parse_u64(&value(&mut index)?, name)?
                }
                "--fail-unresolvable" => {
                    if inline.is_some() {
                        return Err(format!("{name} takes no value"));
                    }
                    options.fail_unresolvable = true;
                }
                other => {
                    return Err(format!(
                        "unknown hls-pull-duty option {other:?}; usage: {USAGE}"
                    ));
                }
            }
            index += 1;
        }
        options.validate()?;
        Ok(options)
    }

    pub(crate) fn window(&self) -> Duration {
        Duration::from_secs(self.duration_secs)
    }

    pub(crate) fn reconcile_interval(&self) -> Duration {
        Duration::from_millis(self.reconcile_interval_ms)
    }

    /// The reconciler's backstop cadence: ten intervals (see the engine's
    /// `BACKSTOP_INTERVALS`). The lifecycle finding is stated against this.
    pub(crate) fn backstop(&self) -> Duration {
        self.reconcile_interval() * 10
    }

    /// The URL the good pull is pointed at: the source preview normally, or
    /// the unresolvable source in the failing variant.
    pub(crate) fn pull_source_url(&self, preview_url: &str) -> String {
        if self.fail_unresolvable {
            UNRESOLVABLE_PULL_URL.to_string()
        } else {
            preview_url.to_string()
        }
    }

    /// The URL the failure-reporting pull is pointed at. In the failing
    /// variant the good pull already carries it, and the second pipeline is
    /// skipped.
    pub(crate) fn bad_pull_url(&self) -> &'static str {
        UNRESOLVABLE_PULL_URL
    }

    fn validate(&self) -> Result<(), String> {
        if self.duration_secs == 0 || self.duration_secs > MAX_DURATION_SECS {
            return Err(format!(
                "--duration-secs must be in 1..={MAX_DURATION_SECS} (got {})",
                self.duration_secs
            ));
        }
        if self.reconcile_interval_ms < MIN_RECONCILE_INTERVAL_MS
            || self.reconcile_interval_ms > MAX_RECONCILE_INTERVAL_MS
        {
            return Err(format!(
                "--reconcile-interval-ms must be in {MIN_RECONCILE_INTERVAL_MS}..={MAX_RECONCILE_INTERVAL_MS} (got {})",
                self.reconcile_interval_ms
            ));
        }
        Ok(())
    }
}

fn parse_u64(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{name} needs a positive integer (got {value:?})"))
}
