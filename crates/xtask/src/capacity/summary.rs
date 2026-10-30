//! The ramp summary: per-rung pass counts and worst cases from the
//! resource-sweep CSVs, and the Markdown and JSON written beside them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::StepResult;

/// One resource-sweep CSV row by column name.
pub(super) type Row = BTreeMap<String, String>;

/// Rows of a CSV whose fields may be double-quoted (`""` escapes a quote).
pub(super) fn csv_rows(text: &str) -> Vec<Row> {
    let mut lines = text.lines().filter(|line| !line.is_empty());
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let header = csv_fields(header);
    lines
        .map(|line| header.iter().cloned().zip(csv_fields(line)).collect())
        .collect()
}

fn csv_fields(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        let field = fields.last_mut().expect("starts with one field");
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => fields.push(String::new()),
            _ => field.push(c),
        }
    }
    fields
}

/// Parses a float, with NaN for missing or malformed input.
pub(super) fn number(value: Option<&String>) -> f64 {
    value
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(f64::NAN)
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    match values.len() {
        0 => f64::NAN,
        n if n % 2 == 1 => values[n / 2],
        n => (values[n / 2 - 1] + values[n / 2]) / 2.0,
    }
}

struct Rung {
    protocol: String,
    outputs: u64,
    repeats: usize,
    passed: usize,
    rx_delivered_min: u64,
    rx_ratio_min: f64,
    rx_ratio_median_med: f64,
    rx_interval_min: f64,
    rx_jain_min: f64,
    restream_delivered_min: u64,
    restream_ratio_min: f64,
    cpu_avg_median: f64,
    cpu_peak_max: f64,
    rss_peak_mb_max: f64,
    threads_peak_max: f64,
}

fn rung_summary(protocol: &str, outputs: u64, runs: &[Row]) -> Rung {
    let column = |name: &str| {
        runs.iter()
            .map(|row| number(row.get(name)))
            .collect::<Vec<_>>()
    };
    let min = |name: &str| column(name).into_iter().fold(f64::NAN, f64::min);
    let max = |name: &str| column(name).into_iter().fold(f64::NAN, f64::max);
    let count = |name: &str| -> Vec<u64> {
        column(name)
            .into_iter()
            .map(|value| if value.is_nan() { 0 } else { value as u64 })
            .collect()
    };
    let delivered = count("delivery_delivered");
    Rung {
        protocol: protocol.to_owned(),
        outputs,
        repeats: runs.len(),
        passed: delivered.iter().filter(|&&d| d >= outputs).count(),
        rx_delivered_min: delivered.iter().copied().min().unwrap_or(0),
        rx_ratio_min: min("delivery_ratio_min"),
        rx_ratio_median_med: median(column("delivery_ratio_median")),
        rx_interval_min: min("delivery_interval_ratio_min"),
        rx_jain_min: min("delivery_jain"),
        restream_delivered_min: count("restream_delivery_delivered")
            .into_iter()
            .min()
            .unwrap_or(0),
        restream_ratio_min: min("restream_delivery_ratio_min"),
        cpu_avg_median: median(column("restream_cpu_avg_pct")),
        cpu_peak_max: max("restream_cpu_peak_pct"),
        rss_peak_mb_max: max("rss_peak_kb") / 1024.0,
        threads_peak_max: max("thread_count_peak"),
    }
}

/// Writes `<root>/summary.csv` and `<root>/summary.md` and prints the latter.
pub(super) fn summarize(root: &Path) -> StepResult {
    let mut rungs: BTreeMap<(String, u64), Vec<Row>> = BTreeMap::new();
    for protocol_dir in subdirs(root) {
        for outputs_dir in subdirs(&protocol_dir) {
            let (Some(protocol), Some(outputs)) = (
                protocol_dir.file_name().and_then(|name| name.to_str()),
                outputs_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.parse().ok()),
            ) else {
                continue;
            };
            for rep_dir in subdirs(&outputs_dir) {
                if !rep_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("rep"))
                {
                    continue;
                }
                if let Ok(text) = fs::read_to_string(rep_dir.join("resource-sweep-results.csv")) {
                    rungs
                        .entry((protocol.to_owned(), outputs))
                        .or_default()
                        .extend(csv_rows(&text));
                }
            }
        }
    }
    let rows: Vec<Rung> = rungs
        .iter()
        .map(|((protocol, outputs), runs)| rung_summary(protocol, *outputs, runs))
        .collect();

    let mut csv = String::from(
        "protocol,outputs,repeats,passed,rx_delivered_min,rx_ratio_min,rx_ratio_median_med,rx_interval_min,\
         rx_jain_min,restream_delivered_min,restream_ratio_min,cpu_avg_median,cpu_peak_max,rss_peak_mb_max,threads_peak_max\n",
    );
    for r in &rows {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            r.protocol,
            r.outputs,
            r.repeats,
            r.passed,
            r.rx_delivered_min,
            r.rx_ratio_min,
            r.rx_ratio_median_med,
            r.rx_interval_min,
            r.rx_jain_min,
            r.restream_delivered_min,
            r.restream_ratio_min,
            r.cpu_avg_median,
            r.cpu_peak_max,
            r.rss_peak_mb_max,
            r.threads_peak_max,
        ));
    }
    fs::write(root.join("summary.csv"), csv)
        .map_err(|error| format!("cannot write summary.csv: {error}"))?;

    let provenance: Value = fs::read_to_string(root.join("provenance.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    let markdown = summary_markdown(&provenance, &rows);
    fs::write(root.join("summary.md"), &markdown)
        .map_err(|error| format!("cannot write summary.md: {error}"))?;
    println!("{markdown}");
    Ok(())
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn summary_markdown(provenance: &Value, rows: &[Rung]) -> String {
    // Strings print bare; anything else prints as JSON (numbers, null).
    let field = |key: &str, default: &str| match provenance.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => default.to_owned(),
    };
    let mut lines = vec!["# Capacity ramp".to_owned(), String::new()];
    if provenance.as_object().is_some_and(|map| !map.is_empty()) {
        let dirty = if provenance["dirty"].as_bool() == Some(true) {
            " (dirty)"
        } else {
            ""
        };
        let jitter = provenance["host_jitter"]
            .as_object()
            .map(|cpus| {
                cpus.iter()
                    .map(|(cpu, result)| {
                        format!(
                            "cpu{cpu} {}% lost, worst {} ms",
                            result["lost_pct"], result["worst_ms"]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "not measured".into());
        lines.extend([
            format!("- commit `{}`{dirty}", field("commit", "?")),
            format!("- build profile: {}", field("build_profile", "bench")),
            format!(
                "- CPU: {}, {} online, NUMA nodes {}, hypervisor {}",
                field("cpu_model", "?"),
                field("online_cpus", "?"),
                field("numa_nodes", "?"),
                field("hypervisor", "?")
            ),
            format!("- kernel {}; {}", field("kernel", "?"), field("memory", "")),
            format!(
                "- Restream CPUs `{}`, harness/sink CPUs `{}`, egress shards {}, SRT sink ports {}, Owners per port {}, malloc arenas {}",
                field("restream_cpus", "None"),
                field("harness_cpus", "None"),
                field("egress_shards", "None"),
                field("peer_count", "None"),
                field("sink_owners_per_port", "None"),
                field("malloc_arena_max", "restream provisional default")
            ),
            format!("- host jitter (gaps > 5 ms on each Restream CPU, nothing else scheduled): {jitter}"),
            format!(
                "- one ingest at {}, window {} s, {} repeats per rung; pass = every destination >= 0.95",
                field("bitrate", "None"),
                field("window_secs", "None"),
                field("repeats", "None")
            ),
            String::new(),
        ]);
    }
    let mut protocols: Vec<&str> = rows.iter().map(|row| row.protocol.as_str()).collect();
    protocols.dedup();
    for protocol in protocols {
        let protocol_rows: Vec<&Rung> =
            rows.iter().filter(|row| row.protocol == protocol).collect();
        let capacity = protocol_rows
            .iter()
            .filter(|row| row.passed == row.repeats)
            .map(|row| row.outputs)
            .max()
            .unwrap_or(0);
        // HLS PUT is graded per segment (ratio = due segments received); the
        // byte-interval ratio and Restream's fabric-leaf view do not apply.
        let hls = protocol == "hls";
        lines.push(format!(
            "## {}: capacity {capacity} outputs (all repeats passed)",
            protocol.to_uppercase()
        ));
        lines.push(String::new());
        if hls {
            lines.push(
                "HLS PUT: rx ratio is the share of due segments received; an output passes with every \
                 due segment, each within 3 s of the first output."
                    .into(),
            );
            lines.push(String::new());
        }
        lines.push(
            "| outputs | passed | rx delivered min | rx ratio min | rx interval min | rx Jain min \
             | Restream delivered min | CPU avg (median) | CPU peak | RSS MB |"
                .into(),
        );
        lines.push("|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|".into());
        for row in protocol_rows {
            let interval = if hls {
                "—".to_owned()
            } else {
                format!("{:.3}", row.rx_interval_min)
            };
            let restream = if hls {
                "—".to_owned()
            } else {
                row.restream_delivered_min.to_string()
            };
            lines.push(format!(
                "| {} | {}/{} | {} | {:.3} | {interval} | {:.5} | {restream} | {:.1}% | {:.1}% | {:.0} |",
                row.outputs,
                row.passed,
                row.repeats,
                row.rx_delivered_min,
                row.rx_ratio_min,
                row.rx_jain_min,
                row.cpu_avg_median,
                row.cpu_peak_max,
                row.rss_peak_mb_max,
            ));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_fields_honor_quotes_and_escaped_quotes() {
        assert_eq!(
            csv_fields(r#"a,"b,c","say ""hi""",,d"#),
            vec!["a", "b,c", r#"say "hi""#, "", "d"]
        );
    }

    #[test]
    fn rung_summary_counts_passes_and_takes_worst_case() {
        let csv = "delivery_delivered,delivery_ratio_min,delivery_ratio_median,restream_cpu_avg_pct,rss_peak_kb,egress_mix\n\
                   100,0.97,0.99,40,2048,\"rtmp:50,srt:50\"\n\
                   99,0.91,0.98,50,4096,x\n\
                   ,0.99,0.97,60,1024,x\n";
        let rows = csv_rows(csv);
        assert_eq!(rows[0]["egress_mix"], "rtmp:50,srt:50");
        let rung = rung_summary("rtmp", 100, &rows);
        assert_eq!(
            (rung.repeats, rung.passed, rung.rx_delivered_min),
            (3, 1, 0)
        );
        assert_eq!(rung.rx_ratio_min, 0.91);
        assert_eq!(rung.rx_ratio_median_med, 0.98);
        assert_eq!(rung.cpu_avg_median, 50.0);
        assert_eq!(rung.rss_peak_mb_max, 4.0);
        assert!(rung.rx_jain_min.is_nan(), "missing column stays NaN");
    }

    #[test]
    fn capacity_is_the_highest_rung_where_every_repeat_passed() {
        let row = |outputs, repeats, passed| Rung {
            outputs,
            repeats,
            passed,
            ..rung_summary("srt", 0, &[])
        };
        let rows = [
            row(50, 3, 3),
            row(100, 3, 3),
            row(150, 2, 1),
            row(200, 3, 3),
        ];
        let markdown = summary_markdown(&Value::Null, &rows);
        assert!(
            markdown.contains("## SRT: capacity 200 outputs (all repeats passed)"),
            "{markdown}"
        );
        assert!(markdown.contains("| 150 | 1/2 |"), "{markdown}");
        let failing = [row(50, 3, 2)];
        assert!(summary_markdown(&Value::Null, &failing).contains("capacity 0 outputs"));
    }
}
