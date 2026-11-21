//! `cargo xtask reportcard`: per-function complexity of the product sources,
//! measured by `rust-code-analysis-cli` and gated on cyclomatic complexity
//! (`[workspace.metadata.reportcard] max_cyclomatic` in `Cargo.toml`).
//!
//! Functions already above the limit are recorded in [`BASELINE`], a
//! ratchet: an unrecorded offender, one that grew, one that shrank without
//! lowering its entry, and an entry whose function is gone or back within
//! the limit all fail. `--update-baseline` only removes and lowers entries,
//! so a new offender is split, never recorded. Cognitive complexity and SLOC
//! are reported only: clippy `cognitive_complexity` gates cognitive.

mod gate;
mod report;
#[cfg(test)]
mod tests;
mod units;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::StepResult;
use gate::Baseline;
use report::Report;
use units::Unit;

const ANALYZER: &str = "rust-code-analysis-cli";
const ANALYZER_VERSION: &str = "0.0.25";
const INSTALL: &str = "cargo install rust-code-analysis-cli --version 0.0.25 --locked";
const REPORT_DIR: &str = "target/reportcard";
const BASELINE: &str = "crates/xtask/reportcard-baseline.json";
/// What is measured: the product crates' sources, inline tests included.
/// `vendor/`, `crates/test_harness`, `crates/xtask`, `tests/`, `benches/` and
/// `fuzz/` are not product code.
const SCOPE: &str = "`src/**/*.rs`, `crates/restream-*/src/**/*.rs`";

#[derive(Default)]
struct Summary {
    files: usize,
    sloc: u64,
    units: Vec<Unit>,
}

/// The gate: measures, writes the report and fails on any finding.
pub(crate) fn run() -> StepResult {
    let root = crate::repo_root();
    let (summary, limit, baseline) = measure(&root)?;
    let offenders = gate::offenders(&summary.units, limit);
    let findings = gate::check(&summary.units, limit, &baseline);
    let report = Report {
        summary: &summary,
        limit,
        offenders: &offenders,
        baselined: baseline.len(),
        findings: &findings,
    };
    let markdown = report.markdown();
    report
        .write(&root, &markdown)
        .map_err(|error| format!("cannot write {REPORT_DIR}: {error}"))?;
    print!("{markdown}");
    if findings.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "reportcard: {} finding(s), listed above and in {REPORT_DIR}/reportcard.md",
            findings.len()
        ))
    }
}

/// `--update-baseline`: rewrites [`BASELINE`] to exactly the current
/// offenders, refusing to add or raise any entry.
pub(crate) fn update_baseline() -> StepResult {
    let root = crate::repo_root();
    let (summary, limit, baseline) = measure(&root)?;
    let updated = gate::updated(&summary.units, limit, &baseline).map_err(|refused| {
        let mut message = String::from(
            "reportcard: refusing to add or raise baseline entries; fix these functions instead:",
        );
        for finding in refused {
            message.push_str(&format!("\n  {finding}"));
        }
        message
    })?;
    let removed = baseline
        .keys()
        .filter(|key| !updated.contains_key(*key))
        .count();
    let lowered = updated
        .iter()
        .filter(|(key, value)| baseline.get(*key) != Some(value))
        .count();
    let text = serde_json::to_string_pretty(&updated).map_err(|error| error.to_string())?;
    fs::write(root.join(BASELINE), text + "\n")
        .map_err(|error| format!("cannot write {BASELINE}: {error}"))?;
    println!(
        "reportcard: {BASELINE}: {} entries ({removed} removed, {lowered} lowered)",
        updated.len()
    );
    Ok(())
}

fn measure(root: &Path) -> Result<(Summary, u64, Baseline), String> {
    let summary = analyze(root)?;
    let limit = max_cyclomatic()?;
    let baseline = match fs::read_to_string(root.join(BASELINE)) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("{BASELINE} is not a key-to-integer JSON object: {error}"))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Baseline::new(),
        Err(error) => return Err(format!("cannot read {BASELINE}: {error}")),
    };
    Ok((summary, limit, baseline))
}

/// Runs the analyzer over [`SCOPE`] into `target/reportcard/raw` and reads
/// its output.
fn analyze(root: &Path) -> Result<Summary, String> {
    ensure_analyzer()?;
    let raw = root.join(REPORT_DIR).join("raw");
    let io_error = |error: io::Error| format!("{}: {error}", raw.display());
    if raw.exists() {
        fs::remove_dir_all(&raw).map_err(io_error)?;
    }
    fs::create_dir_all(&raw).map_err(io_error)?;
    let mut command = Command::new(ANALYZER);
    command
        .args(["-m", "-I", "*.rs", "--pr", "-O", "json", "-o"])
        .arg(&raw)
        .current_dir(root);
    for dir in source_dirs(root).map_err(|error| format!("cannot list crates/: {error}"))? {
        command.arg("-p").arg(dir);
    }
    let status = command
        .status()
        .map_err(|error| format!("cannot run {ANALYZER}: {error}"))?;
    if !status.success() {
        return Err(format!("{ANALYZER} exited with {status}"));
    }
    let mut files = Vec::new();
    json_files(&raw, &mut files).map_err(io_error)?;
    files.sort();
    let mut summary = Summary::default();
    for path in files {
        let text = fs::read_to_string(&path).map_err(io_error)?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| format!("{}: invalid analyzer JSON: {error}", path.display()))?;
        units::add_file(&value, &mut summary)?;
    }
    if summary.units.is_empty() {
        return Err(format!("{ANALYZER} found no functions in {SCOPE}"));
    }
    Ok(summary)
}

/// `src` and every `crates/restream-*/src`, sorted.
fn source_dirs(root: &Path) -> io::Result<Vec<String>> {
    let mut dirs = vec!["src".to_string()];
    for entry in fs::read_dir(root.join("crates"))? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.starts_with("restream-") && root.join("crates").join(&name).join("src").is_dir() {
            dirs.push(format!("crates/{name}/src"));
        }
    }
    dirs[1..].sort();
    Ok(dirs)
}

fn json_files(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            json_files(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            files.push(path);
        }
    }
    Ok(())
}

/// The pinned analyzer: another version may count differently and move
/// every recorded value.
fn ensure_analyzer() -> Result<(), String> {
    match Command::new(ANALYZER).arg("--version").output() {
        Ok(output)
            if output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim()
                    == format!("{ANALYZER} {ANALYZER_VERSION}") =>
        {
            Ok(())
        }
        Ok(output) => Err(format!(
            "{ANALYZER} {ANALYZER_VERSION} is required, found {:?}; install it with `{INSTALL}`",
            String::from_utf8_lossy(&output.stdout).trim()
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(format!(
            "{ANALYZER} {ANALYZER_VERSION} is not installed; install it with `{INSTALL}`"
        )),
        Err(error) => Err(format!("cannot run {ANALYZER}: {error}")),
    }
}

/// `[workspace.metadata.reportcard] max_cyclomatic`, read through
/// `cargo metadata` so xtask needs no TOML parser.
fn max_cyclomatic() -> Result<u64, String> {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .map_err(|error| format!("cannot run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cargo metadata printed invalid JSON: {error}"))?;
    metadata
        .pointer("/metadata/reportcard/max_cyclomatic")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            "Cargo.toml needs a non-negative integer [workspace.metadata.reportcard] max_cyclomatic"
                .to_string()
        })
}
