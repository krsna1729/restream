//! Native entrypoints for bench freshness, bounded validation, CPU launchers
//! and the frozen-SRT-destination campaign.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::workflow::{self, Error, WorkflowResult};

mod affinity;
mod media;
mod parallel;
mod srt_frozen;

pub(crate) use affinity::{pinned, segregated};
pub(crate) use media::media_validation;
pub(crate) use parallel::parallel_fast_breadth;
pub(crate) use srt_frozen::srt_frozen;

const USAGE: &str = "usage:
  cargo xtask harness [--prepare] <mode> [-- <harness args...>]
  cargo xtask harness --prepare
  cargo xtask harness --help

BENCH_BUILD=always|if-needed|never controls bench preparation (default: if-needed).
RESTREAM_BENCH_FEATURES passes optional Cargo features to build-bench.
Freshness includes src/, scripts/, test/harness/, workspace crates, Cargo.toml,
Cargo.lock, build.rs and rust-toolchain.toml, excluding nested target directories.";

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
    workflow::checked(&mut harness_command(&root, &binary, mode, parsed.args)?)
}

/// The `test_harness` invocation for one mode, run from the repository root.
/// Callers that own a process group (`workflow::checked`, `srt-frozen`)
/// launch it directly so every media child stays in that group.
pub(super) fn harness_command(
    root: &Path,
    binary: &Path,
    mode: &str,
    args: &[&str],
) -> Result<Command, Error> {
    if env::var("HARNESS_USE_CGROUP_WRAPPER")
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    {
        // The harness execs this path (core/setup.rs CGROUP_WRAP_BIN).
        crate::container::install_executable(&root.join(".local/bin"), "restream-cgroup-wrap")?;
    }
    let mut command = Command::new(binary);
    command.current_dir(root).arg(mode).args(args);
    Ok(command)
}

pub(super) fn env_default(name: &str, default: &str) -> std::ffi::OsString {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.into())
}

pub(super) fn self_harness(args: &[&str]) -> Result<Command, Error> {
    let mut command = Command::new(env::current_exe()?);
    command.arg("harness").args(args);
    Ok(command)
}

/// The bounded workflows take no arguments; `--help` describes them instead
/// of starting a run. Returns whether the workflow should run.
pub(super) fn no_arguments(args: &[&str], usage: &str) -> Result<bool, Error> {
    match args {
        [] => Ok(true),
        ["-h" | "--help"] => {
            println!("{usage}");
            Ok(false)
        }
        _ => Err(Error::usage(usage)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BuildMode {
    Always,
    IfNeeded,
    Never,
}

impl BuildMode {
    fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "1" | "true" | "TRUE" | "always" => Ok(Self::Always),
            "0" | "false" | "FALSE" | "never" => Ok(Self::Never),
            "if-needed" => Ok(Self::IfNeeded),
            _ => Err(Error::usage(format!(
                "invalid BENCH_BUILD value '{value}'; expected always|if-needed|never"
            ))),
        }
    }

    fn rebuild(self, features: bool, stale: impl FnOnce() -> bool) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::IfNeeded => features || stale(),
        }
    }
}

fn prepare(root: &Path) -> Result<PathBuf, Error> {
    let selected = PathBuf::from(env_default("HARNESS_BIN", "target/bench/test_harness"));
    let binary = root.join(selected);
    let mode = BuildMode::parse(&env_default("BENCH_BUILD", "if-needed").to_string_lossy())?;
    let features = env::var_os("RESTREAM_BENCH_FEATURES").is_some_and(|s| !s.is_empty());
    if mode.rebuild(features, || needs_bench_rebuild(root, &binary)) {
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

fn needs_bench_rebuild(root: &Path, binary: &Path) -> bool {
    if !workflow::executable(binary) {
        return true;
    }
    let Ok(stamp) = fs::symlink_metadata(binary).and_then(|metadata| metadata.modified()) else {
        return true;
    };
    let stamp = seconds(stamp);
    [
        "src",
        "scripts",
        "test/harness",
        "crates",
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
        "rust-toolchain.toml",
    ]
    .iter()
    .any(|input| newer_input(&root.join(input), stamp))
}

fn newer_input(path: &Path, stamp: i128) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.is_file() {
        return metadata
            .modified()
            .is_ok_and(|modified| seconds(modified) > stamp);
    }
    if !metadata.is_dir() || path.file_name().is_some_and(|name| name == "target") {
        return false;
    }
    fs::read_dir(path).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| newer_input(&entry.path(), stamp))
    })
}

fn seconds(time: SystemTime) -> i128 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i128::from(duration.as_secs()),
        Err(error) => {
            -i128::from(error.duration().as_secs())
                - i128::from(error.duration().subsec_nanos() != 0)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, FileTimes};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::time::Duration;

    fn stamped(path: &Path, second: u64, executable: bool) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = File::create(path).unwrap();
        file.set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(second)))
            .unwrap();
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
        )
        .unwrap();
    }

    #[test]
    fn build_modes_respect_features_without_overriding_never() {
        assert!(BuildMode::IfNeeded.rebuild(true, || panic!("features force a build")));
        assert!(!BuildMode::Never.rebuild(true, || panic!("never cannot inspect freshness")));
        assert!(BuildMode::Always.rebuild(false, || panic!("always cannot inspect freshness")));
        for value in ["1", "true", "TRUE", "always"] {
            assert_eq!(BuildMode::parse(value).unwrap(), BuildMode::Always);
        }
        for value in ["0", "false", "FALSE", "never"] {
            assert_eq!(BuildMode::parse(value).unwrap(), BuildMode::Never);
        }
        assert!(BuildMode::parse("sometimes").is_err());
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

    #[test]
    fn workspace_and_lock_changes_invalidate_bench_but_target_and_symlinks_do_not() {
        let dir = crate::workflow::TempDir::new("restream-harness-test").unwrap();
        let bin = dir.path().join("target/bench/test_harness");
        stamped(&bin, 100, true);
        stamped(&dir.path().join("crates/codec/src/lib.rs"), 99, false);
        stamped(
            &dir.path().join("crates/codec/target/ignored.rs"),
            200,
            false,
        );
        stamped(&dir.path().join("outside.rs"), 200, false);
        symlink(
            dir.path().join("outside.rs"),
            dir.path().join("crates/codec/src/link.rs"),
        )
        .unwrap();
        assert!(!needs_bench_rebuild(dir.path(), &bin));
        stamped(&dir.path().join("Cargo.lock"), 101, false);
        assert!(needs_bench_rebuild(dir.path(), &bin));
        stamped(&dir.path().join("Cargo.lock"), 100, false);
        assert!(!needs_bench_rebuild(dir.path(), &bin));
        stamped(&dir.path().join("crates/codec/src/lib.rs"), 101, false);
        assert!(needs_bench_rebuild(dir.path(), &bin));
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(needs_bench_rebuild(dir.path(), &bin));
    }

    #[test]
    fn freshness_uses_whole_seconds_like_stat() {
        assert_eq!(seconds(UNIX_EPOCH + Duration::from_millis(100_999)), 100);
        assert_eq!(seconds(UNIX_EPOCH - Duration::from_millis(1)), -1);
    }
}
