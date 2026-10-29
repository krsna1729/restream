//! The single release-shard catalog, execution plan, and timeout policy.
use super::support;
use crate::workflow::{self, Error, WorkflowResult, checked};
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::time::Instant;

pub(crate) fn list() -> &'static [&'static str] {
    &[
        "mixed.file.h265.a2.bf2",
        "mixed.file.h265.a2.bf0",
        "mixed.live.srt.h265.a2.bf0",
        "mixed.live.srt.h265.a2.bf2",
        "mixed.live.srt.h264.a2.bf2",
        "mixed.live.srt.h264.a2.bf0",
        "mixed.file.h264.a2.bf0",
        "mixed.file.h264.a2.bf2",
        "srt-crypto-matrix",
        "ramp-family",
        "mixed.file.h265.a1.bf0",
        "mixed.file.h265.a1.bf2",
        "mixed.live.srt.h265.a1.bf0",
        "mixed.live.srt.h265.a1.bf2",
        "mixed.file.h264.a1.bf2",
        "mixed.file.h264.a1.bf0",
        "mixed.live.rtmp.h264.a1.bf0",
        "mixed.live.rtmp.h264.a1.bf2",
        "bitrate-sweep.h264-rtmp",
        "bitrate-sweep.h265-srt",
        "bitrate-sweep.h264-srt",
        "bitrate-sweep.mixed-h264-a2",
        "bitrate-sweep.mixed-h265-a2",
        "branch-matrix",
        "fault.resilience",
        "fault.egress-retry",
        "fault.output-stall",
        "fault.srt-output-stall",
        "msr-smoke",
        "resource-sweep.source",
        "resource-sweep.transcode",
        "resource-sweep.hevc",
        "smoke",
        "direct-play",
        "mixed.live.srt.h264.a1.bf0",
        "mixed.live.srt.h264.a1.bf2",
    ]
}

fn exists(shard: &str) -> bool {
    list().contains(&shard)
}

pub(crate) fn timeout(shard: &str) -> &'static str {
    if matches!(
        shard,
        "smoke" | "direct-play" | "branch-matrix" | "msr-smoke"
    ) {
        "10m"
    } else if (shard.starts_with("mixed.") && (shard.ends_with(".bf0") || shard.ends_with(".bf2")))
        || matches!(
            shard,
            "fault.resilience"
                | "fault.egress-retry"
                | "fault.output-stall"
                | "fault.srt-output-stall"
                | "ramp-family"
        )
    {
        "30m"
    } else if shard == "srt-crypto-matrix" || shard.starts_with("resource-sweep.") {
        "50m"
    } else if shard.starts_with("bitrate-sweep.") {
        "60m"
    } else {
        "40m"
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) kind: &'static str,
    pub(crate) value: String,
}

pub(crate) fn plan(shard: &str) -> Result<Vec<Step>, Error> {
    if !exists(shard) {
        return Err(Error::usage(format!("unknown release shard: {shard}")));
    }
    let row = |kind, value: &str| Step {
        kind,
        value: value.to_owned(),
    };
    Ok(match shard {
        "smoke" => ["api-smoke", "file.live-edge", "srt.policy"]
            .map(|mode| row("mode", mode))
            .into(),
        "direct-play" => ["srt.policy", "timestamp.bframe"]
            .map(|mode| row("mode", mode))
            .into(),
        "bitrate-sweep.h264-rtmp" => vec![row("bitrate", "h264-rtmp")],
        "bitrate-sweep.h264-srt" => vec![row("bitrate", "h264-srt")],
        "bitrate-sweep.h265-srt" => vec![row("bitrate", "h265-srt")],
        "bitrate-sweep.mixed-h264-a2" => vec![row("bitrate", "mixed.live.srt.h264.a2.bf2")],
        "bitrate-sweep.mixed-h265-a2" => vec![row("bitrate", "mixed.live.srt.h265.a2.bf2")],
        "resource-sweep.source" => vec![row(
            "resource",
            "resource.egress-growth-source-same,resource.egress-growth-source-mixed",
        )],
        "resource-sweep.transcode" => vec![row(
            "resource",
            "resource.egress-growth-transcode-same,resource.egress-growth-transcode-mixed,resource.egress-growth-source-plus-transcode-mixed,resource.egress-growth-transcode-dual-mixed,resource.egress-growth-source-plus-transcode-dual-mixed",
        )],
        "resource-sweep.hevc" => vec![row("resource", "resource.egress-growth-hevc-bridge")],
        "msr-smoke" => vec![row("msr-smoke", "msr")],
        name if name.starts_with("mixed.file.") => vec![row(
            "mode",
            &format!("mixed.asset.file.{}", &name["mixed.file.".len()..]),
        )],
        name => vec![row("mode", name)],
    })
}

const USAGE: &str = "usage: cargo xtask qualify harness-shard <shard> | run <shard> | list | timeout <shard> | explain <shard>";

/// Shared catalog CLI, also exposed as `release shards`.
pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    match args {
        ["--json", ..] | ["list", "--json", ..] => {
            let json = serde_json::to_string(list())
                .map_err(|error| format!("release shards: {error}"))?;
            println!("{json}");
            Ok(())
        }
        [] | ["list", ..] => {
            for shard in list() {
                println!("{shard}");
            }
            Ok(())
        }
        ["-h" | "--help", ..] => {
            println!("{USAGE}");
            Ok(())
        }
        [verb @ ("timeout" | "explain" | "plan"), shard, ..] if exists(shard) => {
            if *verb == "timeout" {
                println!("{}", timeout(shard));
            } else {
                if *verb == "explain" {
                    println!("shard={shard}\ntimeout={}", timeout(shard));
                }
                for step in plan(shard)? {
                    println!("{}\t{}", step.kind, step.value);
                }
            }
            Ok(())
        }
        ["timeout" | "explain" | "plan", shard, ..] => {
            Err(Error::usage(format!("unknown release shard: {shard}")))
        }
        _ => Err(Error::usage(USAGE)),
    }
}

pub(super) fn harness(args: &[&str]) -> WorkflowResult {
    let shard = match args {
        [] => return Err(Error::usage(USAGE)),
        ["-h" | "--help", ..] => {
            eprintln!("{USAGE}");
            return Ok(());
        }
        ["list" | "timeout" | "explain", ..] => return run(args),
        ["run", shard, ..] if !shard.is_empty() => *shard,
        ["run", ..] => return Err(Error::usage(USAGE)),
        [shard, ..] => *shard,
    };
    if !exists(shard) {
        return Err(Error::usage(format!(
            "harness-shard: unknown shard '{shard}'\n{USAGE}"
        )));
    }
    let started = Instant::now();
    if std::env::var("RELEASE_HARNESS_SHARD_TIMEOUT_ACTIVE").as_deref() != Ok("1") {
        support::require_command("timeout", "harness-shard")?;
        let limit = crate::workflow::env_or("RELEASE_HARNESS_SHARD_TIMEOUT", timeout(shard));
        println!("[release-shard] timeout {shard}: {limit}");
        let mut child = workflow::spawn(
            Command::new("timeout")
                .args(["--kill-after=30s", &limit])
                .arg(std::env::current_exe()?)
                .args(["qualify", "harness-shard", "run", shard])
                .env("RELEASE_HARNESS_SHARD_TIMEOUT_ACTIVE", "1"),
        )?;
        let status = workflow::wait(&mut child)?;
        let code = status.code().or_else(|| status.signal().map(|s| 128 + s));
        if matches!(code, Some(124 | 137 | 143)) {
            eprintln!(
                "[release-shard] TIMEOUT {shard} after {} (limit {limit})",
                elapsed(started)
            );
        }
        return if status.success() {
            Ok(())
        } else {
            Err(Error::status(
                status,
                format!("harness-shard: {shard} failed"),
            ))
        };
    }
    let safe_shard: String = shard
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let default_id = format!(
        "release-{}-{safe_shard}",
        crate::workflow::env_or("GITHUB_SHA", "local")
    );
    let run_id = crate::workflow::env_or("RELEASE_HARNESS_RUN_ID", &default_id);
    for step in plan(shard)? {
        let mode = match step.kind {
            "mode" | "msr-smoke" => step.value.as_str(),
            "bitrate" => "bitrate-sweep",
            "resource" => "resource-sweep",
            kind => {
                return Err(format!(
                    "harness-shard: invalid plan row for {shard}: {kind} {}",
                    step.value
                )
                .into());
            }
        };
        println!("[release-shard] run {shard}: {mode}");
        let mut command = support::xtask()?;
        command
            .args(["harness", mode, "--", "--no-netns", "--run-id", &run_id])
            .env("TEST_HARNESS_SUPPRESS_SUCCESS_JSON", "1");
        match step.kind {
            "bitrate" => {
                command.env("BITRATE_SWEEP_CONFIGS", &step.value);
            }
            "resource" => {
                command.env("RESOURCE_SWEEP_SCENARIOS", &step.value);
            }
            "msr-smoke" => {
                command
                    .env("MSR_OUTPUT_COUNTS", "1")
                    .env("MSR_FFPROBE_SAMPLE_COUNT", "1")
                    .env("MSR_SINK_SAMPLE_SECS", "1");
            }
            _ => {}
        }
        checked(&mut command)?;
    }
    println!("[release-shard] PASS {shard} ({})", elapsed(started));
    Ok(())
}

fn elapsed(started: Instant) -> String {
    let seconds = started.elapsed().as_secs();
    format!("{}m{:02}s", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_published_shard_has_a_nonempty_executable_plan() {
        let unique: HashSet<_> = list().iter().collect();
        assert_eq!(unique.len(), list().len());
        for shard in list() {
            let steps = plan(shard).unwrap();
            assert!(!steps.is_empty());
            assert!(steps.iter().all(|s| !s.value.is_empty()
                && ["mode", "bitrate", "resource", "msr-smoke"].contains(&s.kind)));
        }
        assert!(plan("mixed.file.h264.a9.bf0").is_err());
        assert!(plan("").is_err());
    }

    #[test]
    fn file_aliases_and_measurement_selectors_are_not_harness_mode_names() {
        assert_eq!(
            plan("mixed.file.h264.a2.bf2").unwrap()[0].value,
            "mixed.asset.file.h264.a2.bf2"
        );
        let bitrate = plan("bitrate-sweep.mixed-h265-a2").unwrap();
        assert_eq!(bitrate[0].kind, "bitrate");
        assert_eq!(bitrate[0].value, "mixed.live.srt.h265.a2.bf2");
        assert_eq!(
            plan("resource-sweep.source").unwrap()[0]
                .value
                .split(',')
                .count(),
            2
        );
        assert_eq!(
            plan("resource-sweep.transcode").unwrap()[0]
                .value
                .split(',')
                .count(),
            5
        );
        assert_eq!(plan("msr-smoke").unwrap()[0].kind, "msr-smoke");
    }

    #[test]
    fn long_measurements_do_not_inherit_the_smoke_timeout() {
        assert_eq!(timeout("smoke"), "10m");
        assert_eq!(timeout("mixed.file.h264.a2.bf0"), "30m");
        assert_eq!(timeout("srt-crypto-matrix"), "50m");
        assert_eq!(timeout("resource-sweep.hevc"), "50m");
        assert_eq!(timeout("bitrate-sweep.h264-srt"), "60m");
    }
}
