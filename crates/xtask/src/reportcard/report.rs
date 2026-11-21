//! The report: Markdown on stdout and in `target/reportcard/`, JSON beside
//! it. Only the cyclomatic ratchet gates; the rest informs.

use std::env;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;

use serde_json::{Value, json};

use super::gate::{Baseline, Finding};
use super::units::Unit;
use super::{ANALYZER_VERSION, REPORT_DIR, SCOPE, Summary};

const PERCENTILES: [(&str, f64); 4] = [("p50", 0.50), ("p90", 0.90), ("p95", 0.95), ("p99", 0.99)];
const TOP: usize = 20;

/// What one run measured and decided.
pub(super) struct Report<'a> {
    pub(super) summary: &'a Summary,
    pub(super) limit: u64,
    pub(super) offenders: &'a Baseline,
    pub(super) baselined: usize,
    pub(super) findings: &'a [Finding],
}

impl Report<'_> {
    pub(super) fn markdown(&self) -> String {
        let summary = self.summary;
        let status = if self.findings.is_empty() {
            "PASS"
        } else {
            "FAIL"
        };
        let mut out = format!(
            "# restream report card\n\nStatus: **{status}**\n\nScope: {SCOPE} (inline tests included)\n\n"
        );
        out.push_str("| Gate | Limit | Over limit | Baselined | Findings | Result |\n|---|---:|---:|---:|---:|:---:|\n");
        let _ = writeln!(
            out,
            "| Cyclomatic complexity per function | {} | {} | {} | {} | {status} |\n",
            self.limit,
            self.offenders.len(),
            self.baselined,
            self.findings.len(),
        );
        out.push_str("Cognitive complexity is reported only; clippy `cognitive_complexity` gates it (`clippy.toml`).\n\n");
        let _ = writeln!(
            out,
            "Files: {} · functions: {} named + {} closures · SLOC: {}\n",
            summary.files,
            summary.units.len() - closures(summary),
            closures(summary),
            summary.sloc,
        );
        out.push_str("| Metric | Total | Average | p50 | p90 | p95 | p99 | Max |\n|---|---:|---:|---:|---:|---:|---:|---:|\n");
        for (label, metric) in [
            ("Cyclomatic", Metric::Cyclomatic),
            ("Cognitive", Metric::Cognitive),
        ] {
            let stats = Stats::of(summary, metric);
            let _ = write!(out, "| {label} | {} | {:.2} |", stats.total, stats.average);
            for value in stats.percentiles {
                let _ = write!(out, " {value} |");
            }
            let _ = writeln!(out, " {} |", stats.max);
        }
        let _ = writeln!(out, "\n## Top {TOP} by cyclomatic\n");
        out.push_str("| Function | Cyclomatic | Cognitive | Location |\n|---|---:|---:|---|\n");
        for unit in top(summary) {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | `{}:{}-{}` |",
                scope_path(unit),
                unit.cyclomatic,
                unit.cognitive,
                unit.path,
                unit.start_line,
                unit.end_line,
            );
        }
        if !self.findings.is_empty() {
            out.push_str("\n## Findings\n\n");
            for finding in self.findings {
                let _ = writeln!(out, "- {finding}");
            }
        }
        out
    }

    fn json(&self) -> Value {
        let summary = self.summary;
        json!({
            "tool": {"name": "rust-code-analysis-cli", "version": ANALYZER_VERSION},
            "scope": SCOPE,
            "files": summary.files,
            "named_functions": summary.units.len() - closures(summary),
            "closures": closures(summary),
            "sloc": summary.sloc,
            "cyclomatic": Stats::of(summary, Metric::Cyclomatic).json(),
            "cognitive": Stats::of(summary, Metric::Cognitive).json(),
            "limits": {"max_cyclomatic": self.limit},
            "baselined": self.baselined,
            "offenders": self.offenders,
            "findings": self.findings.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "top_cyclomatic": top(summary).map(|unit| json!({
                "key": unit.key,
                "path": unit.path,
                "start_line": unit.start_line,
                "end_line": unit.end_line,
                "cyclomatic": unit.cyclomatic,
                "cognitive": unit.cognitive,
            })).collect::<Vec<_>>(),
        })
    }

    /// Writes `reportcard.md` and `reportcard.json`, and appends the
    /// Markdown to the GitHub step summary when there is one.
    pub(super) fn write(&self, root: &Path, markdown: &str) -> io::Result<()> {
        let directory = root.join(REPORT_DIR);
        fs::create_dir_all(&directory)?;
        fs::write(directory.join("reportcard.md"), markdown)?;
        let json = serde_json::to_string_pretty(&self.json()).map_err(io::Error::other)?;
        fs::write(directory.join("reportcard.json"), json + "\n")?;
        if let Some(path) = env::var_os("GITHUB_STEP_SUMMARY") {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?
                .write_all(markdown.as_bytes())?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Metric {
    Cyclomatic,
    Cognitive,
}

struct Stats {
    total: u64,
    average: f64,
    percentiles: [u64; PERCENTILES.len()],
    max: u64,
}

impl Stats {
    fn of(summary: &Summary, metric: Metric) -> Self {
        let mut values: Vec<u64> = summary
            .units
            .iter()
            .map(|unit| match metric {
                Metric::Cyclomatic => unit.cyclomatic,
                Metric::Cognitive => unit.cognitive,
            })
            .collect();
        values.sort_unstable();
        let total: u64 = values.iter().sum();
        let rank = |fraction: f64| {
            let index = ((values.len() as f64 * fraction).ceil() as usize).saturating_sub(1);
            values.get(index).copied().unwrap_or_default()
        };
        Self {
            total,
            average: if values.is_empty() {
                0.0
            } else {
                total as f64 / values.len() as f64
            },
            percentiles: PERCENTILES.map(|(_, fraction)| rank(fraction)),
            max: values.last().copied().unwrap_or_default(),
        }
    }

    fn json(&self) -> Value {
        let mut value = json!({"total": self.total, "average": self.average, "max": self.max});
        for ((name, _), percentile) in PERCENTILES.iter().zip(self.percentiles) {
            value[*name] = json!(percentile);
        }
        value
    }
}

fn closures(summary: &Summary) -> usize {
    summary
        .units
        .iter()
        .filter(|unit| unit.is_closure())
        .count()
}

/// The `TOP` units by cyclomatic, then cognitive, then key.
fn top(summary: &Summary) -> impl Iterator<Item = &Unit> {
    let mut units: Vec<&Unit> = summary.units.iter().collect();
    units.sort_by(|a, b| {
        (b.cyclomatic, b.cognitive)
            .cmp(&(a.cyclomatic, a.cognitive))
            .then_with(|| a.key.cmp(&b.key))
    });
    units.into_iter().take(TOP)
}

/// The key without its file path: `Engine::run#0`.
fn scope_path(unit: &Unit) -> &str {
    unit.key
        .strip_prefix(unit.path.as_str())
        .and_then(|rest| rest.strip_prefix("::"))
        .unwrap_or(&unit.key)
}
