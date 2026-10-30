use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use super::env_default;
use crate::workflow::{self, Error, OwnedChild, WorkflowResult};

const GROUPS: [(&str, u16); 3] = [
    ("live-rtmp", 43000),
    ("live-srt", 48000),
    ("file-ingest", 53000),
];
const PORTS: [(&str, u16); 11] = [
    ("RESTREAM_HTTP", 30),
    ("RESTREAM_RTMP", 35),
    ("RESTREAM_SRT", 80),
    ("MTX_RTMP", 135),
    ("MTX_SRT", 180),
    ("MTX_HLS", 190),
    ("MTX_API", 197),
    ("SINK_PORT", 300),
    ("HLS_PUT_PORT", 600),
    ("FFMPEG_SRT_SINK_BASE", 800),
    ("FFMPEG_SIGNAL_SINK_BASE", 2000),
];

struct GroupRun {
    name: &'static str,
    dir: PathBuf,
    child: Option<OwnedChild>,
    success: bool,
}

const USAGE: &str = "usage: cargo xtask parallel-fast-breadth
Runs mixed.fast-breadth --no-netns as three concurrent groups (live-rtmp,
live-srt, file-ingest) on disjoint port ranges and merges their reports.
Environment: WORK_DIR, BENCH_BUILD, N_PER_GROUP (1), SKIP_LOAD (1),
COLLECT_FAILURES (1).";

pub(crate) fn parallel_fast_breadth(args: &[&str]) -> WorkflowResult {
    match args {
        [] => {}
        ["-h" | "--help"] => {
            println!("{USAGE}");
            return Ok(());
        }
        _ => return Err(Error::usage(USAGE)),
    }
    workflow::checked(&mut harness(&["--prepare"])?)?;
    let root = match env::var_os("WORK_DIR").filter(|value| !value.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => {
            let stamp = crate::bins::utc_timestamp(SystemTime::now()).replace(['-', ':'], "");
            PathBuf::from(format!(
                ".local/artifacts/mixed/fast-breadth-parallel-{stamp}"
            ))
        }
    };
    fs::create_dir_all(&root)?;
    let mut runs = Vec::with_capacity(GROUPS.len());
    for (name, base) in GROUPS {
        let dir = root.join(name);
        fs::create_dir_all(&dir)?;
        let stdout = File::create(dir.join("result.json"))?;
        let mut stderr = File::create(dir.join("stderr.log"))?;
        let mut command = harness(&["mixed.fast-breadth", "--", "--no-netns"])?;
        command
            .env("BENCH_BUILD", "never")
            .envs(group_environment(name, base, &dir))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr.try_clone()?));
        match OwnedChild::spawn(&mut command) {
            Ok(child) => runs.push(GroupRun {
                name,
                dir,
                child: Some(child),
                success: false,
            }),
            Err(error) if workflow::interrupted().is_some() => return Err(error),
            Err(error) => {
                writeln!(stderr, "{error}")?;
                runs.push(GroupRun {
                    name,
                    dir,
                    child: None,
                    success: false,
                });
            }
        }
    }
    wait_groups(&mut runs)?;
    merge_reports(&root, &runs)?;
    println!("fast-breadth parallel artifacts: {}", root.display());
    println!("batch\tstatus\tresult\tstderr");
    io::stdout().write_all(&fs::read(root.join("report-index.txt"))?)?;
    if runs.iter().all(|run| run.success) {
        Ok(())
    } else {
        Err(Error::exit(1, "one or more fast-breadth groups failed"))
    }
}

/// This xtask binary's own `harness` entrypoint.
fn harness(args: &[&str]) -> Result<Command, Error> {
    let mut command = Command::new(env::current_exe()?);
    command.arg("harness").args(args);
    Ok(command)
}

fn wait_groups(runs: &mut [GroupRun]) -> WorkflowResult {
    // Do not short circuit on a failed group: every sibling must finish and
    // contribute its evidence. OwnedChild drops terminate/reap on interruption.
    for run in runs {
        if let Some(child) = &mut run.child {
            run.success = child.wait()?.success();
        }
    }
    Ok(())
}

fn group_environment(name: &str, base: u16, dir: &Path) -> Vec<(&'static str, OsString)> {
    let mut values = vec![
        ("WORK_DIR", dir.as_os_str().to_owned()),
        (
            "ASSERTION_LOG",
            dir.join("assertions.jsonl").into_os_string(),
        ),
        ("TIMING_LOG", dir.join("timing.jsonl").into_os_string()),
        ("SUMMARY_LOG", dir.join("summary.txt").into_os_string()),
        ("RSS_SUMMARY", dir.join("rss-summary.csv").into_os_string()),
        ("MIXED_FAST_BREADTH_GROUPS", name.into()),
        ("N_PER_GROUP", env_default("N_PER_GROUP", "1")),
        ("SKIP_LOAD", env_default("SKIP_LOAD", "1")),
        ("COLLECT_FAILURES", env_default("COLLECT_FAILURES", "1")),
    ];
    values.extend(
        PORTS
            .iter()
            .map(|&(name, offset)| (name, (base + offset).to_string().into())),
    );
    values
}

fn merge_reports(root: &Path, groups: &[GroupRun]) -> WorkflowResult {
    let mut assertions = File::create(root.join("assertions.jsonl"))?;
    let mut timing = File::create(root.join("timing.jsonl"))?;
    let mut index = File::create(root.join("report-index.txt"))?;
    for group in groups {
        append_if_file(&group.dir.join("assertions.jsonl"), &mut assertions)?;
        append_if_file(&group.dir.join("timing.jsonl"), &mut timing)?;
        writeln!(
            index,
            "{}\t{}\t{}\t{}",
            group.name,
            if group.success { "ok" } else { "fail" },
            group.dir.join("result.json").display(),
            group.dir.join("stderr.log").display()
        )?;
    }
    Ok(())
}

fn append_if_file(path: &Path, output: &mut File) -> io::Result<()> {
    if path.is_file() {
        io::copy(&mut File::open(path)?, output)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_ports_are_disjoint_and_artifacts_are_group_local() {
        let mut seen = std::collections::HashSet::new();
        for (name, base) in GROUPS {
            let dir = PathBuf::from("evidence with spaces").join(name);
            let values = group_environment(name, base, &dir);
            for (key, value) in &values {
                if PORTS.iter().any(|(port, _)| port == key) {
                    assert!(seen.insert(value.clone()));
                }
                if matches!(
                    *key,
                    "ASSERTION_LOG" | "TIMING_LOG" | "SUMMARY_LOG" | "RSS_SUMMARY"
                ) {
                    assert!(Path::new(value).starts_with(&dir));
                }
            }
            assert!(values.contains(&("MIXED_FAST_BREADTH_GROUPS", name.into())));
        }
    }

    #[test]
    fn report_merge_includes_failed_group_evidence_and_missing_logs() {
        let root = crate::workflow::TempDir::new("restream-harness-test").unwrap();
        let mut groups = Vec::new();
        for (index, (name, _)) in GROUPS.into_iter().enumerate() {
            let dir = root.path().join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("assertions.jsonl"),
                format!("{{\"group\":{index}}}\n"),
            )
            .unwrap();
            if index != 1 {
                fs::write(dir.join("timing.jsonl"), format!("{index}\n")).unwrap();
            }
            groups.push(GroupRun {
                name,
                dir,
                child: None,
                success: index != 1,
            });
        }
        merge_reports(root.path(), &groups).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("assertions.jsonl")).unwrap(),
            "{\"group\":0}\n{\"group\":1}\n{\"group\":2}\n"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("timing.jsonl")).unwrap(),
            "0\n2\n"
        );
        let index = fs::read_to_string(root.path().join("report-index.txt")).unwrap();
        assert!(index.contains("live-srt\tfail\t"));
        assert_eq!(index.lines().count(), 3);
    }

    #[test]
    fn failed_group_does_not_skip_waiting_for_successful_siblings() {
        let mut groups = Vec::new();
        let mut pids = Vec::new();
        for ((name, _), executable) in
            GROUPS
                .into_iter()
                .zip(["/bin/false", "/bin/true", "/bin/true"])
        {
            let mut child = OwnedChild::spawn(&mut Command::new(executable)).unwrap();
            pids.push(child.child().id() as libc::pid_t);
            groups.push(GroupRun {
                name,
                dir: PathBuf::new(),
                child: Some(child),
                success: false,
            });
        }
        wait_groups(&mut groups).unwrap();
        assert_eq!(
            groups.iter().map(|group| group.success).collect::<Vec<_>>(),
            [false, true, true]
        );
        for pid in pids {
            let mut status = 0;
            // SAFETY: WNOHANG only queries this process's own (already reaped) child.
            let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            assert_eq!(reaped, -1);
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }
    }
}
