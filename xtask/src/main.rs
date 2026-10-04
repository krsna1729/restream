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

mod concurrency;
mod scan;
mod source_audit;

/// Outcome of one step; the error is the message printed before exiting.
type StepResult = Result<(), String>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let root = repo_root();
    if let Err(error) = env::set_current_dir(&root) {
        eprintln!("xtask: cannot enter {}: {error}", root.display());
        return ExitCode::FAILURE;
    }
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["source-audit"] => source_audit::run(),
        ["fixture-discipline"] => scan::fixture_discipline(),
        ["test-hygiene"] => scan::test_hygiene(),
        ["history-grouping"] => history_grouping(),
        ["api-contract"] => api_contract(),
        ["concurrency", "fast"] => concurrency::fast(),
        ["concurrency", "contract"] => concurrency::contract(),
        ["loom", target] => concurrency::loom(target),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "usage: cargo xtask <command>

commands:
  source-audit          layering, file-size, env-var and guardrail audit
  fixture-discipline    fixture contract test + no inline media generators
  test-hygiene          fmt check + full test run whose passing log stays quiet
  history-grouping      frontend history/activity render tests
  api-contract          frontend/backend API contract + api-smoke harness run
  concurrency fast      loom models and focused concurrency regressions
  concurrency contract  fast set + live fault/recovery harness modes
  loom <test-target>    build one tests/<target>.rs with --cfg loom and run it";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the repository root")
        .to_path_buf()
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
    CAPTURE.with(|capture| match &*capture.borrow() {
        Some(log) => {
            let clone = || {
                log.try_clone()
                    .map_err(|error| format!("cannot reuse log: {error}"))
            };
            Ok((clone()?.into(), clone()?.into()))
        }
        None => Ok((Stdio::inherit(), Stdio::inherit())),
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

fn history_grouping() -> StepResult {
    let out = env::temp_dir().join("restream-history-grouping-js");
    compile_frontend_for_node(&out)?;
    run(
        "node",
        &[
            "--test",
            "test/frontend/history-nearby-render.test.mjs",
            "test/frontend/overview-activity-render.test.mjs",
            "test/frontend/frontend-chaos-scenarios.test.mjs",
        ],
        &[("API_CONTRACT_JS_DIR", path_str(&out)?)],
    )
}

fn api_contract() -> StepResult {
    let out = env::temp_dir().join("restream-api-contract-js");
    run("npx", &["tsc", "-p", "tsconfig.json", "--noEmit"], &[])?;
    run("node", &["./scripts/check/api-drift.mjs"], &[])?;
    compile_frontend_for_node(&out)?;
    run(
        "node",
        &["--test", "test/frontend/frontend-api-contract.test.mjs"],
        &[("API_CONTRACT_JS_DIR", path_str(&out)?)],
    )?;
    run("npm", &["run", "build:frontend"], &[])?;
    history_grouping()?;
    cargo(&["test", "--test", "api", "--", "--nocapture"])?;
    cargo(&["build", "--bin", "restream", "--bin", "test_harness"])?;
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

/// Emits the frontend as plain JavaScript into `out` for `node --test`.
fn compile_frontend_for_node(out: &Path) -> StepResult {
    match std::fs::remove_dir_all(out) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot clear {}: {error}", out.display())),
    }
    run(
        "npx",
        &["tsc", "-p", "tsconfig.json", "--outDir", path_str(out)?],
        &[],
    )
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
