//! Measurement binaries: `build-bench` (inner loop, `target/bench/`) and
//! `build-release` (committed evidence, `target/qual-release/`).

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{StepResult, run};

const BINARIES: [&str; 2] = ["restream", "test_harness"];

/// Bench-profile `restream` and `test_harness` in `target/bench/`, with a
/// provenance stamp the packet-rate contract checks before it calls a rung
/// baseline-eligible.
pub(crate) fn build_bench() -> StepResult {
    let features = std::env::var("RESTREAM_BENCH_FEATURES").unwrap_or_default();
    let mut args = vec![
        "build",
        "--profile",
        "bench",
        "--bin",
        "restream",
        "--bin",
        "test_harness",
    ];
    if !features.is_empty() {
        args.extend(["--features", features.as_str()]);
    }
    run("cargo", &args, tmpdir_fallback())?;
    // Cargo writes a profile named `bench` to target/release and that name
    // cannot change, so this copy is the one bridge into target/bench/.
    copy_binaries("target/bench")?;

    let sha = git(&["rev-parse", "HEAD"]).unwrap_or_default();
    let dirty = git(&["status", "--porcelain"]).is_some_and(|status| !status.is_empty());
    let provenance = serde_json::json!({
        "gitSha": sha,
        "gitDirty": dirty,
        "features": features,
        "builtAt": utc_timestamp(SystemTime::now()),
    });
    write(
        Path::new("target/bench/build-provenance.json"),
        &format!("{provenance:#}\n"),
    )?;
    println!(
        "bench build provenance: sha={} dirty={dirty}",
        if sha.is_empty() { "unknown" } else { &sha }
    );
    println!("Bench-profile measurement binaries are ready:");
    println!("  target/bench/restream");
    println!("  target/bench/test_harness");
    println!();
    println!("Use scripts/harness/run.sh for measurement modes so bench binaries stay");
    println!("fresh and launches remain comparable.");
    Ok(())
}

/// Release-profile binaries for qualification and performance evidence, kept
/// apart from target/release (which the bench profile also writes).
pub(crate) fn build_release() -> StepResult {
    run(
        "cargo",
        &[
            "build",
            "--release",
            "--bin",
            "restream",
            "--bin",
            "test_harness",
        ],
        tmpdir_fallback(),
    )?;
    copy_binaries("target/qual-release")?;
    let mut commit = git(&["rev-parse", "HEAD"]).unwrap_or_default();
    commit.push('\n');
    if git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty()) {
        commit.push_str("dirty\n");
    }
    write(Path::new("target/qual-release/COMMIT"), &commit)?;
    println!(
        "release binaries: target/qual-release (commit {})",
        commit.lines().next().unwrap_or_default()
    );
    Ok(())
}

/// `TMPDIR=/tmp` when the inherited TMPDIR is unset or unusable; rustc and
/// the native build scripts fail on a missing temporary directory.
fn tmpdir_fallback() -> &'static [(&'static str, &'static str)] {
    let usable = std::env::var_os("TMPDIR").is_some_and(|dir| {
        fs::metadata(&dir).is_ok_and(|meta| meta.is_dir() && !meta.permissions().readonly())
    });
    if usable { &[] } else { &[("TMPDIR", "/tmp")] }
}

fn copy_binaries(dir: &str) -> StepResult {
    fs::create_dir_all(dir).map_err(|error| format!("cannot create {dir}: {error}"))?;
    for binary in BINARIES {
        let (from, to) = (
            format!("target/release/{binary}"),
            format!("{dir}/{binary}"),
        );
        fs::copy(&from, &to).map_err(|error| format!("cannot copy {from} to {to}: {error}"))?;
    }
    Ok(())
}

fn write(path: &Path, text: &str) -> StepResult {
    fs::write(path, text).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Trimmed stdout of a successful `git` command.
pub(crate) fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// `YYYY-MM-DDTHH:MM:SSZ`, as `date -u +%Y-%m-%dT%H:%M:%SZ` prints it.
pub(crate) fn utc_timestamp(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Proleptic Gregorian date for a count of days since 1970-01-01
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn utc_timestamp_matches_known_instants() {
        let at = |secs| utc_timestamp(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        // Leap day and a year boundary.
        assert_eq!(at(951_825_600), "2000-02-29T12:00:00Z");
        assert_eq!(at(1_735_689_599), "2024-12-31T23:59:59Z");
        assert_eq!(at(1_791_072_000), "2026-10-04T00:00:00Z");
    }
}
