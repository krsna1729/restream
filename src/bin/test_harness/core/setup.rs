use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::{
    MIXED_FAST_BREADTH_MODE, MIXED_MATRIX_MODE, MIXED_SIGNAL_MODE,
    mixed_fast_breadth_default_work_dir, mixed_input_case_for_command,
    mixed_input_default_work_dir, mixed_matrix_default_work_dir, mixed_signal_default_work_dir,
};

pub(crate) fn mixed_command_artifact_path(command: &str) -> Option<PathBuf> {
    if command == MIXED_MATRIX_MODE
        || command == MIXED_SIGNAL_MODE
        || command == MIXED_FAST_BREADTH_MODE
    {
        let work_dir = std::env::var_os("WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if command == MIXED_FAST_BREADTH_MODE {
                    mixed_fast_breadth_default_work_dir()
                } else if command == MIXED_SIGNAL_MODE {
                    mixed_signal_default_work_dir()
                } else {
                    mixed_matrix_default_work_dir()
                }
            });
        return Some(work_dir.join("scenario.json"));
    }
    let case = mixed_input_case_for_command(command)?;
    let work_dir = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| mixed_input_default_work_dir(case));
    Some(work_dir.join("scenario.json"))
}

pub(crate) fn artifact_path(name: &str) -> PathBuf {
    std::env::var_os("TEST_HARNESS_ARTIFACT_DIR")
        .or_else(|| std::env::var_os("WORK_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".local/artifacts/latest"))
        .join(name)
}

pub(crate) fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

pub(crate) fn maybe_global_process_cleanup() {
    if !env_flag("ALLOW_GLOBAL_PROCESS_CLEANUP") {
        return;
    }
    for program in ["restream", "mediamtx", "ffmpeg"] {
        let _ = std::process::Command::new("pkill")
            .args(["-x", program])
            .status();
    }
}

pub(crate) fn maybe_prune_old_artifacts() -> Result<(), String> {
    if env_flag("KEEP_ARTIFACTS") {
        return Ok(());
    }
    let artifact_root = PathBuf::from(".local/artifacts");
    let Ok(entries) = std::fs::read_dir(&artifact_root) else {
        return Ok(());
    };
    let mut run_dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_timestamp_run_dir)
        })
        .collect();
    if run_dirs.len() <= 3 {
        return Ok(());
    }
    run_dirs.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    let remove_count = run_dirs.len().saturating_sub(3);
    for path in run_dirs.into_iter().take(remove_count) {
        std::fs::remove_dir_all(&path).map_err(|error| {
            format!(
                "failed to prune old artifact directory {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

pub(crate) fn command_with_optional_cgroup(program: impl AsRef<OsStr>, scope: &str) -> Command {
    if !env_flag("HARNESS_USE_CGROUP_WRAPPER") {
        return Command::new(program);
    }
    let mut command = Command::new("scripts/native/cgroup-wrap.sh");
    command.arg("--scope").arg(scope).arg("--").arg(program);
    command
}

pub(crate) const MEDIAMTX_CONFIG_ENV_NAMES: [&str; 4] =
    ["MTX_RTMP", "MTX_SRT", "MTX_HLS", "MTX_API"];

pub(crate) fn remove_mediamtx_config_env(command: &mut Command) -> &mut Command {
    for name in MEDIAMTX_CONFIG_ENV_NAMES {
        command.env_remove(name);
    }
    command
}

fn is_timestamp_run_dir(name: &str) -> bool {
    if name.len() != 16 {
        return false;
    }
    let bytes = name.as_bytes();
    bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'Z'
}

pub(crate) fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path))
    }
}

pub(crate) fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

pub(crate) fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
