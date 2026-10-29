use std::env;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

struct Runner(Option<PathBuf>);
impl Runner {
    fn selected() -> Result<Self, Error> {
        let selected = env::var_os("HARNESS_RUNNER")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        if let Some(path) = &selected
            && !crate::workflow::executable(path)
        {
            return Err(format!("missing harness runner at {}", path.display()).into());
        }
        Ok(Self(selected))
    }

    fn command(&self, args: &[&str]) -> Result<Command, Error> {
        match &self.0 {
            Some(path) => {
                let mut command = Command::new(path);
                command.args(args);
                Ok(command)
            }
            None => super::self_harness(args),
        }
    }
}

struct GroupRun {
    name: &'static str,
    dir: PathBuf,
    child: Option<OwnedChild>,
    success: bool,
}

const USAGE: &str = "usage: cargo xtask parallel-fast-breadth
Runs HARNESS_MODE (mixed.fast-breadth) as three concurrent groups (live-rtmp,
live-srt, file-ingest) on disjoint port ranges and merges their reports.
Environment: WORK_DIR, HARNESS_RUNNER, HARNESS_MODE, HARNESS_ARGS (--no-netns,
word-split and globbed), N_PER_GROUP (1), SKIP_LOAD (1), COLLECT_FAILURES (1),
REQUIRE_CLEAN_RUNTIME (0).";

pub(crate) fn parallel_fast_breadth(args: &[&str]) -> WorkflowResult {
    if !super::no_arguments(args, USAGE)? {
        return Ok(());
    }
    let runner = Runner::selected()?;
    workflow::checked(
        runner
            .command(&["--prepare"])?
            .env("BENCH_BUILD", "if-needed"),
    )?;
    let root = match env::var_os("WORK_DIR").filter(|value| !value.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => {
            let stamp = workflow::output(Command::new("date").arg("+%Y%m%dT%H%M%S"))?;
            PathBuf::from(format!(
                ".local/artifacts/mixed/fast-breadth-parallel-{}",
                String::from_utf8_lossy(&stamp.stdout).trim()
            ))
        }
    };
    fs::create_dir_all(&root)?;
    let processes = workflow::output(Command::new("ps").args(["-eo", "pid=,comm=,args="]))?;
    let baseline = runtime_rows(&String::from_utf8_lossy(&processes.stdout));
    if !baseline.is_empty() {
        let path = root.join("preexisting-runtime-processes.txt");
        fs::write(&path, &baseline)?;
        if env_default("REQUIRE_CLEAN_RUNTIME", "0") == "1" {
            eprintln!(
                "refusing to start fast-breadth parallel run while runtime processes are already running"
            );
            eprint!("{baseline}");
            return Err(Error::exit(
                1,
                "pre-existing runtime processes prevent a clean run",
            ));
        }
        eprintln!(
            "warning: continuing beside pre-existing runtime processes; baseline saved to {}",
            path.display()
        );
    }
    let mode = env_default("HARNESS_MODE", "mixed.fast-breadth");
    let args = split_arguments(&env_default("HARNESS_ARGS", "--no-netns"))?;
    let mut runs = Vec::with_capacity(GROUPS.len());
    for (name, base) in GROUPS {
        let dir = root.join(name);
        fs::create_dir_all(&dir)?;
        let stdout = File::create(dir.join("result.json"))?;
        let mut stderr = File::create(dir.join("stderr.log"))?;
        let mut command = runner.command(&[])?;
        command.arg(&mode);
        if !args.is_empty() {
            command.arg("--").args(&args);
        }
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

fn runtime_rows(output: &str) -> String {
    let mut result = String::new();
    for row in output.lines() {
        if matches!(
            row.split_whitespace().nth(1),
            Some("restream" | "mediamtx" | "ffmpeg")
        ) {
            result.push_str(row.trim_start());
            result.push('\n');
        }
    }
    result
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

/// Bash intentionally split HARNESS_ARGS on IFS whitespace and then expanded
/// globs. Quotes/backslashes originating in the variable were not shell syntax.
fn split_arguments(value: &OsStr) -> Result<Vec<OsString>, Error> {
    let mut result = Vec::new();
    for word in value
        .as_bytes()
        .split(|byte| matches!(byte, b' ' | b'\t' | b'\n'))
        .filter(|word| !word.is_empty())
    {
        let pattern = CString::new(word).map_err(|_| Error::from("HARNESS_ARGS contains NUL"))?;
        // SAFETY: an all-zero glob_t is glob's documented initial state; the
        // pattern is NUL-terminated, gl_pathv holds gl_pathc valid C strings
        // until globfree, and every path is copied before globfree.
        unsafe {
            let mut matches: libc::glob_t = std::mem::zeroed();
            let status = libc::glob(
                pattern.as_ptr(),
                libc::GLOB_NOCHECK | libc::GLOB_NOESCAPE,
                None,
                &mut matches,
            );
            if status != 0 {
                libc::globfree(&mut matches);
                return Err(format!(
                    "cannot expand HARNESS_ARGS pattern {} (glob status {status})",
                    pattern.to_string_lossy()
                )
                .into());
            }
            for index in 0..matches.gl_pathc {
                let bytes = CStr::from_ptr(*matches.gl_pathv.add(index)).to_bytes();
                result.push(OsString::from_vec(bytes.to_vec()));
            }
            libc::globfree(&mut matches);
        }
    }
    Ok(result)
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
    fn word_splitting_preserves_literal_quotes_and_expands_paths() {
        assert_eq!(
            split_arguments(OsStr::new("--label 'space value'\t--no-netns\r")).unwrap(),
            ["--label", "'space", "value'", "--no-netns\r"]
        );
        let dir = crate::workflow::TempDir::new("restream-harness-test").unwrap();
        fs::write(dir.path().join("one file.json"), b"").unwrap();
        let pattern = dir.path().join("*.json");
        assert_eq!(
            split_arguments(pattern.as_os_str()).unwrap(),
            [dir.path().join("one file.json").into_os_string()]
        );
    }

    #[test]
    fn runtime_baseline_matches_comm_not_argument_substrings() {
        assert_eq!(
            runtime_rows(
                "  1 restream target/bench/restream\n 2 ffmpeg ffmpeg -i x\n 3 bash echo mediamtx\n 4 mediamtx /opt/mediamtx\n 5 restream-extra x\n"
            ),
            "1 restream target/bench/restream\n2 ffmpeg ffmpeg -i x\n4 mediamtx /opt/mediamtx\n"
        );
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
