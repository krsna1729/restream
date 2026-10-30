//! Repository checks: `cargo xtask <command>`.
//!
//! Each command is a fixed sequence of steps. A step either runs a program in
//! the repository root or runs a scan written in Rust. The first failing step
//! stops the command with a non-zero exit code.

use std::cell::RefCell;
use std::env;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{LazyLock, OnceLock};

mod api_drift;
mod bins;
mod capacity;
mod concurrency;
mod container;
mod deployment;
mod docs_audit;
mod fetch;
mod fixtures;
mod frontend;
mod frontend_style;
mod fuzz;
mod gates;
mod harness;
mod qualify;
mod release;
mod scan;
mod setup;
mod skills;
mod source_audit;
mod test_filters;
mod workflow;
mod worktree;

/// Outcome of one step; the error is the message printed before exiting.
type StepResult = Result<(), String>;

fn main() -> ExitCode {
    if let Err(error) = workflow::install_signals() {
        return exit(Err(error));
    }
    let mut argv = env::args();
    let argv0 = argv.next().unwrap_or_default();
    let args: Vec<String> = argv.collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // Hard-linked copies of this binary run as fixed executables (container
    // RESTREAM_BIN, launcher wrappers) without a checkout or Cargo.
    let alias = Path::new(&argv0).file_name().and_then(|name| name.to_str());
    let aliased = match alias {
        Some("restream-container-shim") => Some(container::shim(&args)),
        Some("restream-cgroup-wrap") => Some(deployment::cgroup_wrap(&args)),
        Some("restream-harness-pinned") => Some(harness::pinned(&args)),
        Some("restream-harness-segregated") => Some(harness::segregated(&args)),
        _ => None,
    };
    if let Some(result) = aliased {
        return exit(result);
    }
    let root = repo_root();
    if let Ok(cwd) = env::current_dir() {
        let _ = INVOCATION_DIR.set(cwd);
    }
    if let Err(error) = env::set_current_dir(&root) {
        eprintln!("xtask: cannot enter {}: {error}", root.display());
        return ExitCode::FAILURE;
    }
    if alias == Some("pre-commit") {
        // Same as `.githooks/pre-commit`: gate the staged diff at the root.
        return exit(gates::run_gates(&["--staged"]).map_err(Into::into));
    }
    let result = match args[..] {
        ["help" | "-h" | "--help"] => {
            println!("{USAGE}");
            Ok(())
        }
        ["source-audit"] => source_audit::run().map_err(Into::into),
        ["fixture-discipline"] => scan::fixture_discipline().map_err(Into::into),
        ["test-hygiene"] => scan::test_hygiene().map_err(Into::into),
        ["api-contract"] => api_contract().map_err(Into::into),
        ["concurrency", "fast"] => concurrency::fast().map_err(Into::into),
        ["concurrency", "contract"] => concurrency::contract().map_err(Into::into),
        ["loom", target] => concurrency::loom(target).map_err(Into::into),
        ["gates", ref rest @ ..] => gates::run_gates(rest).map_err(Into::into),
        ["build-bench"] => bins::build_bench().map_err(Into::into),
        ["build-release"] => bins::build_release().map_err(Into::into),
        ["capacity-ramp", ref rest @ ..] => capacity::capacity_ramp(rest).map_err(Into::into),
        ["host-jitter", ref rest @ ..] => capacity::host_jitter(rest).map_err(Into::into),
        ["setup-skills", ref rest @ ..] => skills::setup_skills(rest).map_err(Into::into),
        ["harness", ref rest @ ..] => harness::run(rest),
        ["media-validation", ref rest @ ..] => harness::media_validation(rest),
        ["parallel-fast-breadth", ref rest @ ..] => harness::parallel_fast_breadth(rest),
        ["harness-pinned", ref rest @ ..] => harness::pinned(rest),
        ["harness-segregated", ref rest @ ..] => harness::segregated(rest),
        ["srt-frozen", ref rest @ ..] => harness::srt_frozen(rest),
        ["frontend", ref rest @ ..] => frontend::run(rest),
        ["release", ref rest @ ..] => release::run(rest),
        ["qualify", ref rest @ ..] => qualify::run(rest),
        ["container", ref rest @ ..] => container::run(rest),
        ["setup", ref rest @ ..] => setup::run(rest),
        ["fetch", ref rest @ ..] => fetch::run(rest),
        ["worktree", ref rest @ ..] => worktree::run(rest),
        ["fixtures", ref rest @ ..] => fixtures::run(rest),
        ["deploy", ref rest @ ..] => deployment::run(rest),
        ["native", "cgroup-wrap", ref rest @ ..] => deployment::cgroup_wrap(rest),
        ["fuzz", ref rest @ ..] => fuzz::run(rest),
        ["docs-audit", ref rest @ ..] => docs_audit::run(rest),
        ["frontend-style", ref rest @ ..] => frontend_style::run(rest),
        ["api-drift", ref rest @ ..] => api_drift::run(rest),
        ["require-test-filters", ref rest @ ..] => test_filters::run(rest),
        _ => Err(workflow::Error::usage(USAGE)),
    };
    exit(result)
}

/// Maps a workflow outcome to the process status (usage 2, child status or
/// 128+signal preserved, otherwise 1).
fn exit(result: workflow::WorkflowResult) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::from(error.code)
        }
    }
}

const USAGE: &str = "usage: cargo xtask <command>

commands:
  source-audit          layering, file-size, env-var and guardrail audit
  fixture-discipline    fixture contract test + no inline media generators
  test-hygiene          fmt check + full test run whose passing log stays quiet
  api-contract          frontend/backend API contract + api-smoke harness run
  concurrency fast      loom models and focused concurrency regressions
  concurrency contract  fast set + live fault/recovery harness modes
  loom <test-target>    build one tests/<target>.rs with --cfg loom and run it
  gates [--staged|--unstaged|--base <ref>] [--dry-run]
                        route changed files to checks; run the pre-commit ones
  build-bench           bench-profile restream + test_harness into target/bench/
  build-release         release binaries into target/qual-release/ (committed evidence)
  capacity-ramp         egress capacity ramp (--help lists its environment)
  host-jitter [secs] [threshold_ms]
                        scheduling-gap probe for the current CPU (pin with taskset)
  setup-skills [--symlink]
                        generate local Claude Code skill shims

workflows (each prints its own usage):
  harness [--prepare] <mode> [-- <args>]   fresh bench binaries + test_harness
  media-validation      bounded media validation harness modes
  parallel-fast-breadth three port-isolated harness groups, merged report
  harness-pinned|harness-segregated <restream args>
                        CPU-affinity RESTREAM_BIN launchers
  srt-frozen --out <dir> --tag <tag> --variant baseline|candidate [args]
                        frozen-SRT-destination RSS campaign (Q-026)
  frontend compile-for-node <out> | node-tests [--coverage] | e2e [args]
  release prepare-build-tree|package-binaries|package-runtime-image|publish-image|
          tag-and-publish|app-native|emit-sbom|checksums|shards [args]
  qualify local-due-diligence|release-artifact-smoke|release-evidence|
          harness-shard|dispatch-dry-run [args]
  container smoke|restream-shim|run-in-harness-runtime|run-harness-shard-in-runtime
  setup prepare|install-git-hooks|refresh-docker-seccomp|harness-host-prereqs
  fetch mediamtx|moq-dev|moxygen
  worktree [args]       optional developer worktree setup (--help)
  fixtures generate-bench|generate-msr-1080p [args]
  deploy install-systemd-service [--now]
  native cgroup-wrap <args>
  fuzz seed-corpus
  docs-audit | frontend-style | api-drift
  require-test-filters <cargo test ...> -- <filter>...

argv0 aliases: restream-container-shim, restream-cgroup-wrap,
restream-harness-pinned, restream-harness-segregated, pre-commit";

/// Repository root: a nonempty `RESTREAM_REPO_ROOT`, else the
/// `CARGO_MANIFEST_DIR` that `cargo run` sets at runtime, else the Git
/// top level of the working directory (an installed binary outside Cargo),
/// else the compile-time manifest directory. Read the runtime variable first:
/// the compile-time value names the worktree the binary was built in, and a
/// `target/` moved to another worktree keeps the binary fresh (Cargo
/// fingerprints workspace members by relative path), so `env!` alone would
/// act on the old worktree.
fn repo_root() -> PathBuf {
    static ROOT: LazyLock<PathBuf> = LazyLock::new(resolve_root);
    ROOT.clone()
}

fn resolve_root() -> PathBuf {
    if let Some(root) = env::var_os("RESTREAM_REPO_ROOT").filter(|root| !root.is_empty()) {
        let root = PathBuf::from(root);
        return match env::current_dir() {
            Ok(cwd) if root.is_relative() => cwd.join(root),
            _ => root,
        };
    }
    let manifest_root = |dir: PathBuf| {
        dir.ancestors()
            .nth(2)
            .expect("xtask lives at crates/xtask below the repository root")
            .to_path_buf()
    };
    if let Some(dir) = env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.ends_with("crates/xtask"))
    {
        return manifest_root(dir);
    }
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim_end()))
        .filter(|root| !root.as_os_str().is_empty())
        .unwrap_or_else(|| manifest_root(PathBuf::from(env!("CARGO_MANIFEST_DIR"))))
}

/// The caller's working directory, recorded before `main` enters the root.
static INVOCATION_DIR: OnceLock<PathBuf> = OnceLock::new();

/// A user-supplied path as the original scripts saw it: relative paths are
/// relative to the directory the command was started from, not the root.
fn user_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    match INVOCATION_DIR.get() {
        Some(dir) if path.is_relative() => dir.join(path),
        _ => path.to_path_buf(),
    }
}

/// The caller's working directory (the root when it was not recorded).
fn invocation_dir() -> PathBuf {
    INVOCATION_DIR.get().cloned().unwrap_or_else(repo_root)
}

thread_local! {
    /// Where child output goes while `capture_into` runs; `None` inherits.
    static CAPTURE: RefCell<Option<File>> = const { RefCell::new(None) };
}

/// Runs `step` with the stdout and stderr of every child it starts written
/// to `log` (the shell's `step >log 2>&1`).
fn capture_into(log: File, step: impl FnOnce() -> StepResult) -> StepResult {
    let previous = CAPTURE.with(|capture| capture.replace(Some(log)));
    let result = step();
    CAPTURE.with(|capture| capture.replace(previous));
    result
}

/// `(stdout, stderr)` for a child: the capture file, or the terminal.
fn child_output() -> Result<(Stdio, Stdio), String> {
    Ok(captured_output()?.unwrap_or_else(|| (Stdio::inherit(), Stdio::inherit())))
}

/// The active `capture_into` log as child `(stdout, stderr)`, if any.
fn captured_output() -> Result<Option<(Stdio, Stdio)>, String> {
    CAPTURE.with(|capture| match &*capture.borrow() {
        Some(log) => {
            let clone = || {
                log.try_clone()
                    .map_err(|error| format!("cannot reuse log: {error}"))
            };
            Ok(Some((clone()?.into(), clone()?.into())))
        }
        None => Ok(None),
    })
}

/// Runs `program args...` with extra environment.
fn run(program: &str, args: &[&str], envs: &[(&str, &str)]) -> StepResult {
    eprintln!("==> {program} {}", args.join(" "));
    let (stdout, stderr) = child_output()?;
    let status = Command::new(program)
        .args(args)
        .envs(envs.iter().copied())
        .stdout(stdout)
        .stderr(stderr)
        .status()
        .map_err(|error| format!("cannot start {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program} {}` failed ({status})", args.join(" ")))
    }
}

fn cargo(args: &[&str]) -> StepResult {
    run("cargo", args, &[])
}

fn api_contract() -> StepResult {
    let out = env::temp_dir().join("restream-api-contract-js");
    run("npx", &["tsc", "-p", "tsconfig.json", "--noEmit"], &[])?;
    api_drift::run(&[]).map_err(|error| error.to_string())?;
    compile_frontend_for_node(&out)?;
    run(
        "node",
        &["--test", "test/frontend/frontend-api-contract.test.mjs"],
        &[("API_CONTRACT_JS_DIR", path_str(&out)?)],
    )?;
    run("npm", &["run", "build:frontend"], &[])?;
    cargo(&["test", "--test", "api", "--", "--nocapture"])?;
    cargo(&[
        "build",
        "-p",
        "restream",
        "--bin",
        "restream",
        "-p",
        "test_harness",
        "--bin",
        "test_harness",
    ])?;
    run(
        "target/debug/test_harness",
        &["api-smoke", "--no-netns"],
        &[
            ("RESTREAM_BIN", "target/debug/restream"),
            ("RESTREAM_INITIAL_ADMIN_PASSWORD", "admin"),
            ("WORK_DIR", ".local/artifacts/api-contract-smoke"),
        ],
    )
}

/// Emits the frontend as plain JavaScript into `out` for `node --test`
/// (the same compile `npm run test:frontend` uses).
fn compile_frontend_for_node(out: &Path) -> StepResult {
    frontend::run(&["compile-for-node", path_str(out)?]).map_err(|error| error.to_string())
}

fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_routes_both_child_streams_to_the_log_then_restores() {
        let path = env::temp_dir().join(format!("xtask-capture-{}.log", std::process::id()));
        let log = File::create(&path).unwrap();
        let failed = capture_into(log, || {
            run("sh", &["-c", "echo out; echo err >&2"], &[])?;
            run("sh", &["-c", "exit 4"], &[])
        });
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(failed.is_err());
        assert_eq!(text, "out\nerr\n");
        assert!(CAPTURE.with(|capture| capture.borrow().is_none()));
    }
}
