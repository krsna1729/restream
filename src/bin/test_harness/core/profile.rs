use std::path::{Path, PathBuf};

use crate::{
    command_requires_port_namespace, measurement_mode_requires_bench_profile,
    suite_modes_require_bench_profile,
};

fn path_profile(path: &Path) -> Option<&'static str> {
    let mut components = path.components();
    while let Some(component) = components.next() {
        if component.as_os_str() == "target" {
            return components
                .next()
                .and_then(|value| value.as_os_str().to_str())
                .and_then(|value| match value {
                    "debug" => Some("debug"),
                    "release" => Some("release"),
                    "bench" => Some("bench"),
                    _ => None,
                });
        }
    }
    None
}

pub(crate) fn is_optimized_profile(path: &Path) -> bool {
    matches!(path_profile(path), Some("bench" | "release"))
}

pub(crate) fn restream_bin_is_explicit() -> bool {
    std::env::var_os("RESTREAM_BIN").is_some()
}

pub(crate) fn measurement_profile_ok_with_explicit(
    harness_path: &Path,
    restream_path: &Path,
    explicit_restream_bin: bool,
) -> bool {
    is_optimized_profile(harness_path)
        && (explicit_restream_bin || is_optimized_profile(restream_path))
}

pub(crate) fn measurement_profile_ok(harness_path: &Path, restream_path: &Path) -> bool {
    measurement_profile_ok_with_explicit(harness_path, restream_path, restream_bin_is_explicit())
}

pub(crate) fn default_work_db_path(work_dir: &Path, file_name: &str) -> PathBuf {
    // Keep mutable harness state scoped to each WORK_DIR so long suites do not
    // contend through a shared repo-root SQLite database.
    work_dir.join(file_name)
}

fn command_uses_host_net(raw: &[String]) -> bool {
    raw.iter().any(|arg| arg == "--no-netns")
        || std::env::var("TEST_HARNESS_USE_HOST_NET")
            .ok()
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

pub(crate) fn strip_netns_opt(raw: &[String]) -> Vec<String> {
    raw.iter()
        .filter(|arg| arg.as_str() != "--no-netns")
        .cloned()
        .collect()
}

pub(crate) fn netns_available() -> bool {
    std::process::Command::new("unshare")
        .args(["--net", "--user", "--map-root-user", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub(crate) fn maybe_reexec_in_port_namespace() -> Result<(), String> {
    if std::env::var_os("RESTREAM_HARNESS_IN_NETNS").is_some() {
        return Ok(());
    }

    let raw: Vec<String> = std::env::args().skip(1).collect();
    let command = raw.first().map(String::as_str).unwrap_or("suite");
    if command == "suite"
        || command == "preflight"
        || !command_requires_port_namespace(command)
        || command_uses_host_net(&raw)
    {
        return Ok(());
    }

    if !netns_available() {
        return Err(format!(
            "{command} requires a network namespace by default; install `unshare` support or rerun with --no-netns"
        ));
    }

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let status = std::process::Command::new("unshare")
        .args(["--net", "--user", "--map-root-user"])
        .arg(&exe)
        .args(strip_netns_opt(&raw))
        .env("RESTREAM_HARNESS_IN_NETNS", "1")
        .status()
        .map_err(|e| format!("failed to re-exec {command} inside a network namespace: {e}"))?;

    if !status.success() {
        return Err(format!(
            "{command} could not enter the default private network namespace; rerun with --no-netns to use the host-network fallback"
        ));
    }

    let code = status.code().unwrap_or(1);
    unsafe { libc::_exit(code) };
}

pub(crate) fn ensure_measurement_profile(command: &str, raw: &[String]) -> Result<(), String> {
    let needs_bench = if command == "suite" {
        suite_modes_require_bench_profile(raw)?
    } else {
        command == "preflight" || measurement_mode_requires_bench_profile(command)
    };
    if !needs_bench {
        return Ok(());
    }

    let harness_path = std::env::current_exe().map_err(|e| e.to_string())?;
    let restream_path = default_restream_bin();
    if measurement_profile_ok(&harness_path, &restream_path) {
        return Ok(());
    }

    Err(format!(
        "{command} requires optimized measurement binaries; use `target/release/test_harness` in release CI, or build local measurement binaries with `scripts/build/bench-harness.sh`"
    ))
}

/// The MSR harness can own one sink socket per RTMP output. Raise its own
/// descriptor ceiling before the in-process peer starts accepting so a normal
/// shell default (often 1024) cannot become the scale-test bottleneck.
pub(crate) fn ensure_msr_nofile_limit(command: &str) -> Result<(), String> {
    if command != "msr" && command != "msr.dashboard" {
        return Ok(());
    }

    let target = msr_nofile_target(msr_requested_output_count());
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `current` is initialized storage valid for the duration of the
    // call, and `getrlimit` only writes the provided structure.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut current) } != 0 {
        return Err(format!(
            "failed to inspect the harness file-descriptor limit: {}",
            std::io::Error::last_os_error()
        ));
    }
    if current.rlim_cur >= target {
        return Ok(());
    }
    if current.rlim_max < target {
        return Err(format!(
            "MSR needs at least {target} open files for {} requested outputs, but the harness hard limit is {}; raise the host/container RLIMIT_NOFILE",
            msr_requested_output_count(),
            current.rlim_max
        ));
    }

    let raised = libc::rlimit {
        rlim_cur: target,
        rlim_max: current.rlim_max,
    };
    // SAFETY: `raised` is initialized storage valid for the duration of the
    // call, and `setrlimit` does not retain the pointer.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } != 0 {
        return Err(format!(
            "failed to raise the harness file-descriptor limit to {target}: {}",
            std::io::Error::last_os_error()
        ));
    }
    println!("[harness] raised RLIMIT_NOFILE soft limit to {target} for MSR");
    Ok(())
}

fn msr_requested_output_count() -> usize {
    let default = if std::env::var("MSR_FULL").ok().as_deref() == Some("1") {
        1_200
    } else {
        30
    };
    std::env::var("MSR_OUTPUT_COUNTS")
        .ok()
        .and_then(|value| {
            value
                .split(',')
                .filter_map(|entry| entry.trim().parse::<usize>().ok())
                .max()
        })
        .unwrap_or(default)
}

fn msr_nofile_target(outputs: usize) -> libc::rlim_t {
    // One server-side RTMP descriptor per output, plus a full additional
    // connection's worth of headroom for the harness, publishers, logs, and
    // retries. Keep ordinary MSR invocations safely above typical 1024-FD
    // shells too.
    outputs.saturating_mul(2).saturating_add(1_024).max(4_096) as libc::rlim_t
}

pub(crate) fn harness_runtime_worker_threads() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    std::env::var("HARNESS_TOKIO_WORKER_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(cpus.clamp(2, 16))
        .max(1)
}

pub(crate) fn harness_runtime_max_blocking_threads() -> usize {
    std::env::var("HARNESS_TOKIO_MAX_BLOCKING_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(256)
        .max(1)
}

pub(crate) fn default_restream_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("RESTREAM_BIN").map(PathBuf::from) {
        return path;
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(bin_dir) = exe.parent()
    {
        let sibling = bin_dir.join("restream");
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from("target/release/restream")
}

#[cfg(test)]
mod tests {
    use super::msr_nofile_target;

    #[test]
    fn msr_nofile_target_covers_full_scale_with_headroom() {
        assert_eq!(msr_nofile_target(30), 4_096);
        assert_eq!(msr_nofile_target(1_200), 4_096);
        assert_eq!(msr_nofile_target(2_000), 5_024);
    }
}
