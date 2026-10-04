//! Source-wide audit: layering, god-file growth, un-centralized environment
//! variables and a few removed-API guardrails. `ARCHITECTURE_GUARDRAILS.md`
//! points here as the authority.

use std::path::{Path, PathBuf};

use crate::StepResult;
use crate::scan::{Hit, files_under, grep_files, read_text};

const RUST_LINE_LIMIT: usize = 999;
const RUST_LINE_WARNING: usize = 800;
const FRONTEND_LINE_LIMIT: usize = 999;

/// Rust responsibility classes reported by the audit, in print order.
const RUST_CLASSES: [&str; 6] = [
    "build-script",
    "production",
    "dedicated-test",
    "harness",
    "benchmark",
    "integration-test",
];

/// `(class, patterns)` in match order; `*` matches any run of characters,
/// including `/`, as in a shell `case` pattern.
const CLASSIFICATION: &[(&str, &[&str])] = &[
    ("build-script", &["build.rs"]),
    ("benchmark", &["benches/*.rs"]),
    ("integration-test", &["tests/*.rs"]),
    (
        "harness",
        &[
            "src/bin/test_harness.rs",
            "src/bin/test_harness/*.rs",
            "test/harness/*.rs",
        ],
    ),
    (
        "dedicated-test",
        &[
            "src/*/tests/*.rs",
            "src/*_test/*.rs",
            "src/*_tests/*.rs",
            "src/*/test.rs",
            "src/*/tests.rs",
            "src/*_test.rs",
            "src/*_tests.rs",
        ],
    ),
    ("production", &["src/*.rs"]),
    ("frontend-test", &["test/*"]),
    ("frontend-production", &["web/ts/*"]),
];

/// Files allowed to read `std::env::var` directly. Matched against the whole
/// `path:line:text` hit, as the original `grep -v` chain did.
const ENV_VAR_OWNERS: &[&str] = &[
    "src/config.rs",
    "src/main.rs",
    "src/lib.rs",
    "src/ffmpeg_binary.rs",
    "src/planner/",
    "src/bin/test_harness",
    "src/bin/restream-mcp.rs",
    "tests/",
    "benches/",
    "test_fixtures.rs",
];

const API_STAGE_STARTS: &[&str] = &[
    "Command::new",
    "ensure_ffmpeg",
    "ffmpeg_bin_path",
    "get_or_create_transcoder",
    "get_or_create_h264_transcoder",
    "spawn_ffmpeg",
    "external_transcoder",
];

pub(crate) fn run() -> StepResult {
    println!("=== Restream Source Audit ===");
    let mut failed = false;

    failed |= report(
        "forbidden imports in src/media/",
        "Media modules must not import api types",
        "No forbidden imports found.",
        grep_files(&files_under(&["src/media"])?, |line| {
            line.contains("use crate::api")
        }),
    );

    failed |= audit_file_sizes()?;

    let env_hits = grep_files(&files_under(&["src"])?, |line| {
        line.contains("std::env::var")
    })
    .into_iter()
    .filter(|hit| {
        let row = hit.to_string();
        !ENV_VAR_OWNERS.iter().any(|owner| row.contains(owner))
    })
    .collect();
    failed |= report(
        "inline std::env::var usage outside src/config.rs",
        "Found raw std::env::var usage outside approved config/test harness owners",
        "No raw std::env::var usage found outside configuration module.",
        env_hits,
    );

    failed |= report(
        "API stage-start guardrails",
        "API route/view modules must not manually start FFmpeg/transcoder stages",
        "API modules do not start FFmpeg/transcoder stages.",
        grep_files(&files_under(&["src/api"])?, starts_a_stage),
    );

    // A module-level allow silences the lint for a whole file, which is how
    // stale annotations and test-only re-implementations stayed hidden. An
    // item-level allow names one item and stays reviewable.
    failed |= report(
        "module-level dead-code suppression",
        "module-level #![allow(dead_code)] hides unused and duplicated code; annotate the specific item instead",
        "No module-level dead-code suppression in src/.",
        grep_files(&files_under(&["src"])?, |line| {
            line.starts_with("#![allow(dead_code)]")
        }),
    );

    failed |= report(
        "harness status-schema guardrails",
        "Harness reads a non-schema output status field named state",
        "Harness does not read the removed output status state field.",
        grep_files(
            &files_under(&["src/bin/test_harness", "src/bin/test_harness.rs"])?,
            |line| line.contains("[\"state\"]"),
        ),
    );

    println!();
    if failed {
        Err("=== AUDIT FAILED ===".into())
    } else {
        println!("=== AUDIT PASSED ===");
        Ok(())
    }
}

fn starts_a_stage(line: &str) -> bool {
    API_STAGE_STARTS.iter().any(|needle| line.contains(needle))
        || line
            .find("run_")
            .is_some_and(|at| line[at..].contains("ffmpeg"))
}

/// Prints the section verdict; returns true when it failed.
fn report(section: &str, failure: &str, success: &str, hits: Vec<Hit>) -> bool {
    println!();
    println!("Checking {section}...");
    if hits.is_empty() {
        println!("OK: {success}");
        return false;
    }
    eprintln!("FAIL: {failure}:");
    for hit in hits {
        eprintln!("{hit}");
    }
    true
}

fn audit_file_sizes() -> Result<bool, String> {
    println!();
    println!("Checking file size limits...");
    let mut files: Vec<PathBuf> = Vec::new();
    if Path::new("build.rs").is_file() {
        files.push("build.rs".into());
    }
    let roots: Vec<&str> = ["src", "web/ts", "test", "tests", "benches"]
        .into_iter()
        .filter(|root| Path::new(root).is_dir())
        .collect();
    files.extend(files_under(&roots)?.into_iter().filter(|path| {
        let name = path.to_string_lossy();
        !name.contains(".local/artifacts")
            && !name.starts_with("test/fixtures/")
            && matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("rs" | "ts" | "mjs" | "js")
            )
    }));

    let mut counts = [0usize; RUST_CLASSES.len()];
    let mut failed = false;
    let mut warnings = 0;
    for path in files {
        let name = path.to_string_lossy().into_owned();
        let class = classify(&name);
        let lines = raw_line_count(&read_text(&path)?);
        if path.extension().is_some_and(|ext| ext == "rs") {
            if let Some(slot) = RUST_CLASSES.iter().position(|known| *known == class) {
                counts[slot] += 1;
            }
            if lines > RUST_LINE_LIMIT {
                eprintln!(
                    "FAIL [{class}]: {name} has {lines} raw lines (Rust hard maximum: {RUST_LINE_LIMIT}; 1000 fails)"
                );
                failed = true;
            } else if lines >= RUST_LINE_WARNING {
                warnings += 1;
                eprintln!(
                    "WARN [{class}]: {name} has {lines} raw lines (Rust pressure band: {RUST_LINE_WARNING}-{RUST_LINE_LIMIT})"
                );
            }
        } else if class == "frontend-production" && lines > FRONTEND_LINE_LIMIT {
            eprintln!(
                "FAIL [{class}]: {name} has {lines} raw lines (frontend hard maximum: {FRONTEND_LINE_LIMIT}; 1000 fails)"
            );
            failed = true;
        }
    }

    println!(
        "Line policies: Rust hard maximum {RUST_LINE_LIMIT} (warn at {RUST_LINE_WARNING}); TypeScript/JavaScript hard maximum {FRONTEND_LINE_LIMIT} (same as Rust)."
    );
    println!("Audited Rust files by responsibility:");
    for (class, count) in RUST_CLASSES.iter().zip(counts) {
        println!("  {:<16} {count}", format!("{class}:"));
    }
    if !failed {
        println!("OK: All audited files are within their language-specific raw-line maximum.");
    }
    if warnings > 0 {
        println!(
            "WARN: {warnings} Rust file(s) are in the {RUST_LINE_WARNING}-{RUST_LINE_LIMIT} pressure band."
        );
        println!(
            "      Near-cap clustering is architectural pressure, not success; split by ownership before adding more code."
        );
    }
    Ok(failed)
}

fn classify(path: &str) -> &'static str {
    CLASSIFICATION
        .iter()
        .find(|(_, patterns)| patterns.iter().any(|pattern| glob(pattern, path)))
        .map_or("other", |(class, _)| class)
}

/// Lines as `awk 'END { print NR }'` counts them: a final line without a
/// trailing newline still counts.
fn raw_line_count(text: &str) -> usize {
    text.lines().count()
}

/// Shell `case` matching for patterns whose only wildcard is `*`.
fn glob(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let tail: Vec<&str> = parts.collect();
    let Some((last, middle)) = tail.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_matches_shell_case_order() {
        for (path, class) in [
            ("build.rs", "build-script"),
            ("benches/api_health.rs", "benchmark"),
            ("tests/api/health.rs", "integration-test"),
            ("src/bin/test_harness.rs", "harness"),
            ("src/bin/test_harness/modes/fault.rs", "harness"),
            ("src/media/engine_tests.rs", "dedicated-test"),
            ("src/media/engine_tests/egress_fabric.rs", "dedicated-test"),
            ("src/api/tests.rs", "dedicated-test"),
            ("src/media/engine.rs", "production"),
            ("test/frontend/x.test.mjs", "frontend-test"),
            ("web/ts/core/api.ts", "frontend-production"),
            ("xtask/src/main.rs", "other"),
        ] {
            assert_eq!(classify(path), class, "{path}");
        }
    }

    #[test]
    fn glob_star_spans_directories_and_anchors_both_ends() {
        assert!(glob("src/*.rs", "src/a/b/c.rs"));
        assert!(!glob("src/*.rs", "src/a.rs.bak"));
        assert!(!glob("src/*_tests.rs", "src/x_tests/y.rs"));
        assert!(glob("src/*_tests/*.rs", "src/x_tests/y.rs"));
        // The suffix must not overlap the prefix.
        assert!(!glob("src/*/tests.rs", "src/tests.rs"));
    }

    #[test]
    fn raw_line_count_counts_unterminated_last_line() {
        assert_eq!(raw_line_count(""), 0);
        assert_eq!(raw_line_count("a\nb\n"), 2);
        assert_eq!(raw_line_count("a\nb"), 2);
    }

    #[test]
    fn stage_start_detection_needs_ffmpeg_after_run_prefix() {
        assert!(starts_a_stage("    run_remux_ffmpeg(&args)"));
        assert!(!starts_a_stage("    ffmpeg_version(); run_report()"));
        assert!(starts_a_stage("let c = Command::new(bin);"));
    }
}
