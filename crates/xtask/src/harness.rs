//! Native entrypoints for bench preparation and CPU launchers.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::workflow::{self, Error, WorkflowResult};

mod affinity;
mod parallel;

pub(crate) use affinity::segregated;
pub(crate) use parallel::parallel_fast_breadth;

const USAGE: &str = "usage:
  cargo xtask harness [--prepare] <mode> [-- <harness args...>]
  cargo xtask harness --prepare
  cargo xtask harness --help

BENCH_BUILD=never skips bench preparation. Any other accepted value (always,
if-needed, the default) runs build-bench, and Cargo decides what is stale.
RESTREAM_BENCH_FEATURES passes optional Cargo features to build-bench, for
example restream-engine/srt-shard-bench.";

struct Invocation<'a> {
    prepare_only: bool,
    mode: Option<&'a str>,
    args: &'a [&'a str],
}

fn invocation<'a>(args: &'a [&'a str]) -> Result<Invocation<'a>, Error> {
    let (prepare_only, args) = match args.split_first() {
        Some((&"--prepare", rest)) => (true, rest),
        _ => (false, args),
    };
    let (mode, mut args) = match args.split_first() {
        Some((mode, rest)) => (Some(*mode), rest),
        None if prepare_only => (None, args),
        None => return Err(Error::usage(USAGE)),
    };
    if args.first() == Some(&"--") {
        args = &args[1..];
    }
    Ok(Invocation {
        prepare_only,
        mode,
        args,
    })
}

pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    if matches!(args.first(), Some(&"--help" | &"-h")) {
        eprintln!("{USAGE}");
        return Ok(());
    }
    let parsed = invocation(args)?;
    let root = crate::repo_root();
    let binary = prepare(&root)?;
    let Some(mode) = parsed.mode.filter(|_| !parsed.prepare_only) else {
        return Ok(());
    };
    if env::var("HARNESS_USE_CGROUP_WRAPPER")
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    {
        // The harness execs this path (core/setup.rs CGROUP_WRAP_BIN).
        crate::container::install_executable(&root.join(".local/bin"), "restream-cgroup-wrap")?;
    }
    workflow::checked(
        Command::new(binary)
            .current_dir(root)
            .arg(mode)
            .args(parsed.args),
    )
}

pub(super) fn env_default(name: &str, default: &str) -> std::ffi::OsString {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.into())
}

fn build_requested(value: &str) -> Result<bool, Error> {
    match value {
        "1" | "true" | "TRUE" | "always" | "if-needed" => Ok(true),
        "0" | "false" | "FALSE" | "never" => Ok(false),
        _ => Err(Error::usage(format!(
            "invalid BENCH_BUILD value '{value}'; expected always|if-needed|never"
        ))),
    }
}

fn prepare(root: &Path) -> Result<PathBuf, Error> {
    let selected = PathBuf::from(env_default("HARNESS_BIN", "target/bench/test_harness"));
    let binary = root.join(selected);
    if build_requested(&env_default("BENCH_BUILD", "if-needed").to_string_lossy())? {
        // Invoke the existing build owner, with the same root and inherited
        // feature environment; never duplicate its profile/provenance logic.
        workflow::checked(
            Command::new(env::current_exe()?)
                .arg("build-bench")
                .current_dir(root),
        )?;
    }
    if !workflow::executable(&binary) {
        return Err(format!(
            "missing harness binary at {}; run cargo xtask build-bench",
            binary.display()
        )
        .into());
    }
    Ok(binary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_never_skips_the_bench_build() {
        for value in ["1", "true", "TRUE", "always", "if-needed"] {
            assert!(build_requested(value).unwrap(), "{value}");
        }
        for value in ["0", "false", "FALSE", "never"] {
            assert!(!build_requested(value).unwrap(), "{value}");
        }
        assert!(build_requested("sometimes").is_err());
    }

    #[test]
    fn arguments_and_prepare_preserve_shell_boundary() {
        let parsed =
            invocation(&["mixed.matrix", "--", "--run-id", "space in name", "--"]).unwrap();
        assert_eq!(parsed.mode, Some("mixed.matrix"));
        assert_eq!(parsed.args, ["--run-id", "space in name", "--"]);
        assert!(invocation(&["--prepare"]).unwrap().prepare_only);
        assert!(
            invocation(&["--prepare", "ignored-mode", "ignored-arg"])
                .unwrap()
                .prepare_only
        );
        assert!(invocation(&[]).is_err());
    }
}
