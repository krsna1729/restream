use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use super::env_default;
use crate::workflow::{Error, WorkflowResult};

fn real_restream() -> Result<std::ffi::OsString, Error> {
    env::var_os("REAL_RESTREAM")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::from("REAL_RESTREAM is required"))
}

/// These entrypoints are also argv0 aliases. Exec, rather than spawn/wait, is
/// essential: the harness samples and signals the PID returned when it starts
/// RESTREAM_BIN, which must become Restream itself.
pub(crate) fn pinned(args: &[&str]) -> WorkflowResult {
    let real = real_restream()?;
    exec_taskset(
        Command::new("taskset")
            .arg("-c")
            .arg(env_default("PIN_CPUS", "5"))
            .arg(real)
            .args(args),
    )
}

pub(crate) fn segregated(args: &[&str]) -> WorkflowResult {
    let real = real_restream()?;
    let sets = CpuSets {
        control: env_default("SEG_CONTROL_CPUS", "0"),
        media: env_default("SEG_MEDIA_CPUS", "1"),
        hot: env_default("SEG_HOT_CPUS", "2-7"),
    };
    let mut union = sets.control.clone();
    union.push(",");
    union.push(&sets.media);
    union.push(",");
    union.push(&sets.hot);
    if env_default("SEG_SHARED", "0") != "1" {
        start_monitor(Path::new(&real), sets)?;
    }
    exec_taskset(
        Command::new("taskset")
            .arg("-c")
            .arg(union)
            .arg(real)
            .args(args),
    )
}

fn exec_taskset(command: &mut Command) -> WorkflowResult {
    let error = command.exec();
    let code = if error.kind() == io::ErrorKind::NotFound {
        127
    } else {
        126
    };
    Err(Error::exit(code, format!("cannot exec taskset: {error}")))
}

struct CpuSets {
    control: std::ffi::OsString,
    media: std::ffi::OsString,
    hot: std::ffi::OsString,
}

impl CpuSets {
    fn for_thread(&self, name: &str) -> &std::ffi::OsStr {
        if name.starts_with("restream-media") {
            &self.media
        } else if ["egress-shard-", "srt-in-", "restream-rtmp-c"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            &self.hot
        } else {
            &self.control
        }
    }
}

fn start_monitor(binary: &Path, sets: CpuSets) -> WorkflowResult {
    let target = std::process::id();
    let process = PathBuf::from(format!("/proc/{target}"));
    let start = fs::read_to_string(process.join("stat"))
        .ok()
        .and_then(|stat| start_time(&stat));
    // A failed readlink in the old background monitor never prevented exec.
    // Still keep the monitor alive until target exit if the path cannot resolve.
    let binary = fs::canonicalize(binary).ok();
    // SAFETY: this launcher route runs before any worker thread exists, so the
    // child may run ordinary Rust code until its explicit `_exit`.
    let child = unsafe { libc::fork() };
    if child < 0 {
        return Err(io::Error::last_os_error().into());
    }
    if child == 0 {
        // The forked monitor never returns to xtask. There are no worker threads
        // before this exec-only CLI route, and it owns no parent cleanup guards.
        // SAFETY: plain syscalls on this freshly forked, single-threaded child;
        // the C string literal is NUL-terminated.
        unsafe {
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
                libc::signal(signal, libc::SIG_DFL);
            }
            // Parent death and the race before installing PDEATHSIG are both
            // covered; exec preserves the parent's PID and does not trigger it.
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0
                || libc::getppid() != target as libc::pid_t
            {
                libc::_exit(0);
            }
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            if null >= 0 {
                for descriptor in [0, 1, 2] {
                    libc::dup2(null, descriptor);
                }
                if null > 2 {
                    libc::close(null);
                }
            }
        }
        monitor(&process, start, binary.as_deref(), &sets);
        // SAFETY: terminate the monitor without running parent-owned destructors.
        unsafe { libc::_exit(0) }
    }
    Ok(())
}

fn alive(process: &Path, expected_start: Option<u64>) -> bool {
    match fs::read_to_string(process.join("stat")) {
        Ok(stat) => expected_start.is_none_or(|expected| start_time(&stat) == Some(expected)),
        Err(_) => false,
    }
}

fn monitor(process: &Path, start: Option<u64>, binary: Option<&Path>, sets: &CpuSets) {
    while alive(process, start) {
        if binary
            .is_some_and(|binary| fs::read_link(process.join("exe")).is_ok_and(|exe| exe == binary))
        {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !alive(process, start) {
        return;
    }
    // Every pool must size itself from the union before its thread class is
    // segregated. HostCapacity is read during boot, not by the monitor.
    thread::sleep(Duration::from_secs(2));
    let mut pinned: HashMap<u32, ThreadIdentity> = HashMap::new();
    while alive(process, start) {
        if let Ok(tasks) = fs::read_dir(process.join("task")) {
            for task in tasks.filter_map(Result::ok) {
                let Some(tid) = task
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<u32>().ok())
                else {
                    continue;
                };
                let Some(identity) = thread_identity(&task.path()) else {
                    continue;
                };
                if pinned.get(&tid) == Some(&identity) {
                    continue;
                }
                // This is a best-effort background operation. Exiting/reused
                // threads are retried next scan, and only success is cached.
                let status = Command::new("taskset")
                    .arg("-pc")
                    .arg(sets.for_thread(&identity.name))
                    .arg(tid.to_string())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                if status.is_ok_and(|status| status.success()) {
                    pinned.insert(tid, identity);
                }
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ThreadIdentity {
    start: u64,
    name: String,
}

fn thread_identity(task: &Path) -> Option<ThreadIdentity> {
    let name = fs::read_to_string(task.join("comm"))
        .ok()?
        .trim_end_matches('\n')
        .to_owned();
    let start = start_time(&fs::read_to_string(task.join("stat")).ok()?)?;
    Some(ThreadIdentity { start, name })
}

fn start_time(stat: &str) -> Option<u64> {
    // comm is parenthesized and may itself contain spaces or ')'. Field 3
    // starts after the last ') '; field 22 is consequently suffix index 19.
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sets() -> CpuSets {
        CpuSets {
            control: "0".into(),
            media: "1".into(),
            hot: "2-7".into(),
        }
    }

    fn stat(start: u64) -> String {
        let mut fields = vec!["S".to_owned(); 20];
        fields[19] = start.to_string();
        format!("123 (name with ) and spaces) {}", fields.join(" "))
    }

    #[test]
    fn thread_classes_keep_control_as_the_fallback() {
        let sets = sets();
        for name in ["restream-media", "restream-media-0"] {
            assert_eq!(sets.for_thread(name), "1");
        }
        for name in ["egress-shard-0", "srt-in-123", "restream-rtmp-c"] {
            assert_eq!(sets.for_thread(name), "2-7");
        }
        for name in ["restream-tokio", "restream", "ffmpeg", "egress-shard"] {
            assert_eq!(sets.for_thread(name), "0");
        }
    }

    #[test]
    fn stat_parser_tolerates_parenthesized_thread_names_and_rejects_truncation() {
        assert_eq!(start_time(&stat(98765)), Some(98765));
        assert_eq!(start_time("123 (broken) S 1 2"), None);
        assert_eq!(start_time("not a stat"), None);
    }

    #[test]
    fn renamed_and_reused_threads_are_not_mistaken_for_already_pinned_threads() {
        let task = crate::workflow::TempDir::new("restream-harness-test").unwrap();
        fs::write(task.path().join("comm"), "restream-tokio\n").unwrap();
        fs::write(task.path().join("stat"), stat(10)).unwrap();
        let first = thread_identity(task.path()).unwrap();
        fs::write(task.path().join("comm"), "egress-shard-1\n").unwrap();
        let renamed = thread_identity(task.path()).unwrap();
        assert_ne!(first, renamed);
        assert_eq!(sets().for_thread(&renamed.name), "2-7");
        fs::write(task.path().join("stat"), stat(11)).unwrap();
        assert_ne!(thread_identity(task.path()).unwrap(), renamed);
        fs::remove_file(task.path().join("comm")).unwrap();
        assert!(thread_identity(task.path()).is_none());
    }

    #[test]
    fn process_identity_rejects_pid_reuse() {
        let process = crate::workflow::TempDir::new("restream-harness-test").unwrap();
        fs::write(process.path().join("stat"), stat(20)).unwrap();
        assert!(alive(process.path(), Some(20)));
        assert!(!alive(process.path(), Some(19)));
        fs::remove_file(process.path().join("stat")).unwrap();
        assert!(!alive(process.path(), Some(20)));
    }

    fn current_cpus() -> Vec<u32> {
        // SAFETY: an all-zero cpu_set_t is a valid empty set; the kernel fills
        // it for this thread and it is only read through the libc accessor.
        unsafe {
            let mut cpus: libc::cpu_set_t = std::mem::zeroed();
            assert_eq!(
                libc::sched_getaffinity(0, std::mem::size_of_val(&cpus), &mut cpus),
                0
            );
            (0..libc::CPU_SETSIZE)
                .filter(|&cpu| libc::CPU_ISSET(cpu as usize, &cpus))
                .map(|cpu| cpu as u32)
                .collect()
        }
    }

    #[test]
    fn native_launchers_preserve_pid_boot_union_and_thread_masks() {
        let allowed = current_cpus();
        let control = allowed[0];
        let media = *allowed.get(1).unwrap_or(&control);
        let hot = *allowed.get(2).unwrap_or(&media);
        let mut union = vec![control, media, hot];
        union.sort_unstable();
        union.dedup();
        for mode in ["pinned", "segregated", "shared"] {
            let dir = crate::workflow::TempDir::new("restream-harness-test").unwrap();
            let evidence = dir.path().join("process.json");
            let log = fs::File::create(dir.path().join("child.log")).unwrap();
            let exe = env::current_exe().unwrap();
            let mut command = Command::new(&exe);
            command
                .args([
                    "--exact",
                    "harness::affinity::tests::native_launch_fixture",
                    "--ignored",
                    "--nocapture",
                ])
                .env("Q105_AFFINITY_FIXTURE", mode)
                .env("Q105_AFFINITY_EVIDENCE", &evidence)
                .env("REAL_RESTREAM", &exe)
                .env("PIN_CPUS", control.to_string())
                .env("SEG_CONTROL_CPUS", control.to_string())
                .env("SEG_MEDIA_CPUS", media.to_string())
                .env("SEG_HOT_CPUS", hot.to_string())
                .env("SEG_SHARED", if mode == "shared" { "1" } else { "0" })
                .stdout(log.try_clone().unwrap())
                .stderr(log);
            let mut child = crate::workflow::OwnedChild::spawn(&mut command).unwrap();
            let pid = child.child().id();
            let status = child.wait().unwrap();
            assert!(
                status.success(),
                "{mode}: {}",
                fs::read_to_string(dir.path().join("child.log")).unwrap()
            );
            let evidence: serde_json::Value =
                serde_json::from_slice(&fs::read(evidence).unwrap()).unwrap();
            assert_eq!(evidence["pid"], pid);
            let expected_boot = if mode == "pinned" {
                vec![control]
            } else {
                union.clone()
            };
            assert_eq!(evidence["boot"], serde_json::json!(expected_boot));
            let expected = match mode {
                "pinned" => vec![vec![control], vec![control], vec![control]],
                "shared" => vec![union.clone(), union.clone(), union.clone()],
                _ => vec![vec![control], vec![media], vec![hot]],
            };
            assert_eq!(evidence["final"], serde_json::json!(expected));
        }
    }

    #[test]
    #[ignore = "subprocess fixture for native_launchers_preserve_pid_boot_union_and_thread_masks"]
    fn native_launch_fixture() {
        let mode = env::var("Q105_AFFINITY_FIXTURE").expect("subprocess fixture only");
        let args = [
            "--exact",
            "harness::affinity::tests::native_target_fixture",
            "--ignored",
            "--nocapture",
        ];
        let result = if mode == "pinned" {
            pinned(&args)
        } else {
            segregated(&args)
        };
        if let Err(error) = result {
            panic!("native launcher did not exec: {error}");
        }
        panic!("native launcher unexpectedly returned");
    }

    #[test]
    #[ignore = "exec target for native_launchers_preserve_pid_boot_union_and_thread_masks"]
    fn native_target_fixture() {
        let path = env::var_os("Q105_AFFINITY_EVIDENCE").expect("subprocess fixture only");
        let boot = current_cpus();
        let segregated = env::var("Q105_AFFINITY_FIXTURE").unwrap() == "segregated";
        let wait = if segregated {
            Duration::from_millis(2800)
        } else {
            Duration::ZERO
        };
        let media = thread::Builder::new()
            .name("restream-media".into())
            .spawn(move || {
                thread::sleep(wait);
                current_cpus()
            })
            .unwrap();
        let hot = thread::Builder::new()
            .name("egress-shard-0".into())
            .spawn(move || {
                thread::sleep(wait);
                current_cpus()
            })
            .unwrap();
        thread::sleep(wait);
        let masks = vec![current_cpus(), media.join().unwrap(), hot.join().unwrap()];
        fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({
                "pid": std::process::id(), "boot": boot, "final": masks,
            }))
            .unwrap(),
        )
        .unwrap();
    }
}
