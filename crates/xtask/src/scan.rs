//! Text scans over repository files and command output, plus the two checks
//! built on them: fixture discipline and test-log hygiene.

use std::fmt;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{StepResult, cargo, run};

/// One matching line, printed as `path:line:text` like `grep -n`.
pub(crate) struct Hit {
    path: PathBuf,
    line: usize,
    text: String,
}

impl fmt::Display for Hit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.path.display(), self.line, self.text)
    }
}

/// Regular files below each root (a root may itself be a file), sorted.
/// Symlinks found while walking are skipped, as `grep -r` skips them.
pub(crate) fn files_under(roots: &[&str]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut pending: Vec<PathBuf> = roots.iter().map(PathBuf::from).collect();
    while let Some(path) = pending.pop() {
        let meta = fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot stat {}: {error}", path.display()))?;
        if meta.is_dir() {
            let entries = fs::read_dir(&path)
                .map_err(|error| format!("cannot list {}: {error}", path.display()))?;
            for entry in entries {
                let entry =
                    entry.map_err(|error| format!("cannot list {}: {error}", path.display()))?;
                pending.push(entry.path());
            }
        } else if meta.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

pub(crate) fn read_text(path: &Path) -> Result<String, String> {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

/// Lines of text files (binary files, by a NUL in the first 4 KiB, are
/// skipped) for which `matches` holds. Unreadable files are skipped too.
pub(crate) fn grep_files(files: &[PathBuf], matches: impl Fn(&str) -> bool) -> Vec<Hit> {
    let mut hits = Vec::new();
    for path in files {
        let Ok(bytes) = fs::read(path) else { continue };
        if bytes[..bytes.len().min(4096)].contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (index, line) in text.lines().enumerate() {
            if matches(line) {
                hits.push(Hit {
                    path: path.clone(),
                    line: index + 1,
                    text: line.to_owned(),
                });
            }
        }
    }
    hits
}

/// Test-facing code must take media from `test/fixtures/` through
/// `src/test_fixtures.rs`, not synthesize it with FFmpeg filter sources.
const INLINE_GENERATORS: &[&str] = &[
    "lavfi",
    "testsrc",
    "testsrc2",
    "smptebars",
    "mandelbrot",
    "anullsrc",
    "anoisesrc",
    "sine=",
];

pub(crate) fn fixture_discipline() -> StepResult {
    eprintln!("[fixture-discipline] validating checked-in fixture contract");
    cargo(&["test", "--test", "fixtures", "--", "--nocapture"])?;

    eprintln!("[fixture-discipline] scanning test and benchmark code for inline media generators");
    let listed = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
        ])
        .args(["src", "tests", "test", "benches"])
        .output()
        .map_err(|error| format!("cannot run git ls-files: {error}"))?;
    if !listed.status.success() {
        return Err(format!("git ls-files failed ({})", listed.status));
    }
    let files: Vec<PathBuf> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
        .map(|raw| PathBuf::from(String::from_utf8_lossy(raw).into_owned()))
        .filter(|path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()))
        .collect();
    let hits = grep_files(&files, |line| {
        let line = line.to_ascii_lowercase();
        INLINE_GENERATORS
            .iter()
            .any(|pattern| line.contains(pattern))
    });
    if !hits.is_empty() {
        for hit in &hits {
            println!("{hit}");
        }
        return Err(
            "[fixture-discipline] inline generator patterns found in test-facing code.
Use checked-in assets from test/fixtures/ via src/test_fixtures.rs.
If a case truly cannot be covered by an existing asset, add a dedicated fixture
generation workflow and document why the committed fixture set is insufficient."
                .into(),
        );
    }
    eprintln!("[fixture-discipline] passed");
    Ok(())
}

/// Output that must not appear in a passing test run.
const NOISE: &[&str] = &[
    "warning:",
    "panicked at",
    "proptest:",
    "failed to find lib.rs or main.rs",
    "specified frame type is not compatible with max B-frames",
    "Could not find codec parameters",
    "not enough frames to estimate rate",
    "405 Method Not Allowed",
    "Blocking waiting for file lock on build directory",
];

pub(crate) fn test_hygiene() -> StepResult {
    eprintln!("[test-hygiene] checking Rust formatting with pinned toolchain");
    run(
        "cargo",
        &["fmt", "--all", "--check"],
        &[("CARGO_TERM_COLOR", "never")],
    )?;

    eprintln!("[test-hygiene] running Rust test graph with captured output");
    let (log, passed) = tee_combined_output(
        Command::new("cargo")
            .args(["test", "--workspace", "--", "--nocapture"])
            .env("CARGO_TERM_COLOR", "never"),
        &mut std::io::stdout().lock(),
    )?;
    if !passed {
        return Err("[test-hygiene] cargo test failed before noise scan".into());
    }

    eprintln!("[test-hygiene] scanning passing log for known noisy patterns");
    let noisy = noisy_lines(&log);
    if !noisy.is_empty() {
        for (number, line) in noisy {
            println!("{number}:{line}");
        }
        return Err("[test-hygiene] noisy output detected in a passing test run.
Quiet the helper or test harness at the source instead of teaching CI to ignore it."
            .into());
    }
    eprintln!("[test-hygiene] passed");
    Ok(())
}

fn noisy_lines(log: &str) -> Vec<(usize, &str)> {
    log.lines()
        .enumerate()
        .filter(|(_, line)| NOISE.iter().any(|pattern| line.contains(pattern)))
        .map(|(index, line)| (index + 1, line))
        .collect()
}

/// Runs `command` with stdout and stderr on one pipe, echoing each line as it
/// arrives to `echo` (like `2>&1 | tee`), and returns the whole log and the
/// exit status.
fn tee_combined_output(
    command: &mut Command,
    echo: &mut dyn Write,
) -> Result<(String, bool), String> {
    let (reader, writer) =
        std::io::pipe().map_err(|error| format!("cannot create pipe: {error}"))?;
    let writer_err = writer
        .try_clone()
        .map_err(|error| format!("cannot clone pipe: {error}"))?;
    let mut child = command
        .stdout(writer)
        .stderr(writer_err)
        .spawn()
        .map_err(|error| format!("cannot start cargo test: {error}"))?;
    // The Command still owns both write ends; drop them so the read loop sees
    // end-of-file once the child exits.
    drop(std::mem::replace(command, Command::new("true")));

    let mut log = String::new();
    for line in BufReader::new(reader).lines() {
        let line = line.map_err(|error| format!("cannot read test output: {error}"))?;
        // Best effort: a closed terminal must not fail the check.
        let _ = writeln!(echo, "{line}");
        log.push_str(&line);
        log.push('\n');
    }
    let status = child
        .wait()
        .map_err(|error| format!("cannot wait for cargo test: {error}"))?;
    Ok((log, status.success()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_scan_reports_one_based_line_numbers() {
        let log =
            "running 3 tests\nwarning: unused import\nok\nthread 'x' panicked at src/a.rs:1\n";
        let noisy = noisy_lines(log);
        assert_eq!(
            noisy,
            vec![
                (2, "warning: unused import"),
                (4, "thread 'x' panicked at src/a.rs:1"),
            ]
        );
        assert!(noisy_lines("test result: ok. 3 passed\n").is_empty());
    }

    #[test]
    fn tee_collects_both_streams_and_exit_status() {
        let mut echoed = Vec::new();
        let (log, passed) = tee_combined_output(
            Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]),
            &mut echoed,
        )
        .unwrap();
        assert!(!passed);
        assert!(log.contains("out\n") && log.contains("err\n"), "{log:?}");
        assert_eq!(echoed, log.as_bytes());
    }
}
