use super::{Actions, info, warn};
use crate::workflow::{Error, WorkflowResult, spawn, wait};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const LOCAL: &[&str] = &["librestream", "restream", "restream_mcp", "test_harness"];
const INCREMENTAL: &[&str] = &[
    "build_script_build",
    "restream",
    "restream_mcp",
    "test_harness",
];
const ROOT_FILES: &[&str] = &[
    "librestream.d",
    "librestream.rlib",
    "restream",
    "restream.d",
    "restream-mcp",
    "restream-mcp.d",
    "test_harness",
    "test_harness.d",
];

fn same_file(source: &Path, target: &Path) -> bool {
    match (fs::metadata(source), fs::metadata(target)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

fn owned_directory(actions: &Actions, target: &Path) -> WorkflowResult {
    if target.is_symlink() {
        info(format!(
            "replace symlinked cache with a copied snapshot: {}",
            target.display()
        ));
        actions.remove(target)?;
    }
    actions.mkdir(target)
}

pub(super) fn sync_tree(
    actions: &Actions,
    source: &Path,
    target: &Path,
    label: &str,
) -> WorkflowResult {
    if !source.exists() {
        info(format!(
            "skip {label}: source missing at {}",
            source.display()
        ));
        return Ok(());
    }
    let target_was_symlink = target.is_symlink();
    if target_was_symlink {
        info(format!(
            "replace symlinked {label} with a copied snapshot: {}",
            target.display()
        ));
        actions.remove(target)?;
    }
    if !target_was_symlink && same_file(source, target) {
        info(format!("skip {label}: source and target are the same path"));
        return Ok(());
    }
    info(format!(
        "seed {label}: {} -> {}",
        source.display(),
        target.display()
    ));
    if crate::workflow::on_path("rsync") {
        actions.mkdir(target)?;
        let mut command = Command::new("rsync");
        command
            .args(["-a", "--delete", "--"])
            .arg(with_slash(source))
            .arg(with_slash(target));
        rsync(actions, &mut command, label)
    } else {
        warn(format!(
            "rsync not found; falling back to cp -a for {label}"
        ));
        actions.remove(target)?;
        if let Some(parent) = target.parent() {
            actions.mkdir(parent)?;
        }
        copy(actions, source, target)
    }
}

// Explicit argv preserves archive permissions, timestamps and package symlinks.
// cp does not hard-link the new cache to the source snapshot.
fn copy(actions: &Actions, source: &Path, target: &Path) -> WorkflowResult {
    actions.command(
        Command::new("cp")
            .args(["-a", "--"])
            .arg(source)
            .arg(target),
    )
}

fn with_slash(path: &Path) -> std::ffi::OsString {
    let mut value = path.as_os_str().to_os_string();
    value.push("/");
    value
}

fn rsync(actions: &Actions, command: &mut Command, label: &str) -> WorkflowResult {
    if actions.dry {
        return actions.command(command);
    }
    let mut child = spawn(command)?;
    let status = wait(&mut child)?;
    if status.success() {
        return Ok(());
    }
    if status.code() == Some(24) {
        warn(format!(
            "rsync reported vanished source files while seeding {label}; keeping the copied cache snapshot"
        ));
        return Ok(());
    }
    // The shell wrapper turns all non-vanished rsync failures into exit 1.
    Err(format!("rsync failed while seeding {label} (exit {status})").into())
}

fn entries(path: &Path) -> Result<Vec<fs::DirEntry>, Error> {
    Ok(fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?)
}

fn modified(entry: &fs::DirEntry) -> SystemTime {
    entry
        .metadata()
        .and_then(|meta| meta.modified())
        .unwrap_or(UNIX_EPOCH)
}

pub(super) enum Keep {
    Numeric(f64),
    Text(String),
}

impl Keep {
    fn allows(&self, rank: usize) -> bool {
        match self {
            Self::Numeric(count) => rank as f64 <= *count,
            // awk compares a nonnumeric -v value lexically with NR.
            Self::Text(value) => rank.to_string().as_str() <= value.as_str(),
        }
    }
}

fn keep_from_env(name: &str, default: usize) -> Keep {
    let Some(value) = env::var(name).ok().filter(|value| !value.is_empty()) else {
        return Keep::Numeric(default as f64);
    };
    let trimmed = value.trim();
    let numeric_start = trimmed.starts_with(|character: char| {
        character.is_ascii_digit() || matches!(character, '+' | '-' | '.')
    });
    if numeric_start && let Ok(count) = trimmed.parse::<f64>() {
        return Keep::Numeric(count);
    }
    Keep::Text(value)
}

pub(super) fn latest_hashes(
    source: &Path,
    prefix: &str,
    keep: &Keep,
) -> Result<Vec<String>, Error> {
    let mut groups: HashMap<String, SystemTime> = HashMap::new();
    let prefix = format!("{prefix}-");
    for entry in entries(source)? {
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(tail) = name.to_str().and_then(|name| name.strip_prefix(&prefix)) else {
            continue;
        };
        let hash = tail.split('.').next().unwrap_or_default().to_owned();
        let newest = groups.entry(hash).or_insert(UNIX_EPOCH);
        *newest = (*newest).max(modified(&entry));
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    // sort -nr breaks timestamp ties using descending hash text.
    groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    Ok(groups
        .into_iter()
        .enumerate()
        .filter(|(index, _)| keep.allows(*index + 1))
        .map(|(_, (hash, _))| hash)
        .collect())
}

fn copy_local_groups(
    actions: &Actions,
    source: &Path,
    target: &Path,
    prefix: &str,
    keep: &Keep,
) -> WorkflowResult {
    let hashes = latest_hashes(source, prefix, keep)?;
    if hashes.is_empty() {
        info(format!(
            "skip debug/deps {prefix}: no matching local artifact groups"
        ));
        return Ok(());
    }
    info(format!(
        "seed debug/deps {prefix}: keeping latest {} artifact group(s)",
        hashes.len()
    ));
    actions.mkdir(target)?;
    for hash in hashes {
        let pattern = format!("{prefix}-{hash}");
        if actions.dry {
            println!(
                "dry-run: copy local dep group {pattern} from {} to {}",
                source.display(),
                target.display()
            );
            continue;
        }
        for entry in entries(source)? {
            if entry.file_type()?.is_file()
                && entry.file_name().to_string_lossy().starts_with(&pattern)
            {
                copy(actions, &entry.path(), &target.join(entry.file_name()))?;
            }
        }
    }
    Ok(())
}

pub(super) fn debug_deps(
    actions: &Actions,
    source: &Path,
    target: &Path,
    keep: &Keep,
) -> WorkflowResult {
    if !source.is_dir() {
        info(format!(
            "skip debug/deps subset: source missing at {}",
            source.display()
        ));
        return Ok(());
    }
    info("seed debug/deps subset: copy third-party artifacts and latest local crate groups");
    owned_directory(actions, target)?;
    if crate::workflow::on_path("rsync") {
        let mut command = Command::new("rsync");
        command.args(["-a", "--delete"]);
        for prefix in LOCAL {
            command.arg("--exclude").arg(format!("{prefix}-*"));
        }
        command
            .arg("--")
            .arg(with_slash(source))
            .arg(with_slash(target));
        rsync(actions, &mut command, "debug/deps subset")?;
    } else {
        warn("rsync not found; falling back to cp -a for debug/deps subset");
        copy(actions, &source.join("."), target)?;
        if !actions.dry {
            for entry in entries(target)? {
                if entry.file_type()?.is_file()
                    && LOCAL.iter().any(|prefix| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(&format!("{prefix}-"))
                    })
                {
                    actions.remove(&entry.path())?;
                }
            }
        } else {
            println!(
                "dry-run: remove local artifact groups from {}",
                target.display()
            );
        }
    }
    for prefix in LOCAL {
        copy_local_groups(actions, source, target, prefix, keep)?;
    }
    Ok(())
}

pub(super) fn latest_incremental(
    source: &Path,
    prefix: &str,
    keep: &Keep,
) -> Result<Vec<PathBuf>, Error> {
    let prefix = format!("{prefix}-");
    let mut dirs = Vec::new();
    for entry in entries(source)? {
        if entry.file_type()?.is_dir() && entry.file_name().to_string_lossy().starts_with(&prefix) {
            dirs.push((modified(&entry), entry.path()));
        }
    }
    dirs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    Ok(dirs
        .into_iter()
        .enumerate()
        .filter(|(index, _)| keep.allows(*index + 1))
        .map(|(_, (_, path))| path)
        .collect())
}

pub(super) fn incremental(
    actions: &Actions,
    source: &Path,
    target: &Path,
    keep: &Keep,
) -> WorkflowResult {
    if !source.is_dir() {
        info(format!(
            "skip debug/incremental subset: source missing at {}",
            source.display()
        ));
        return Ok(());
    }
    for prefix in INCREMENTAL {
        let dirs = latest_incremental(source, prefix, keep)?;
        if dirs.is_empty() {
            info(format!(
                "skip debug/incremental {prefix}: no matching directories"
            ));
            continue;
        }
        info(format!(
            "seed debug/incremental {prefix}: keeping latest {} directories",
            dirs.len()
        ));
        owned_directory(actions, target)?;
        for dir in dirs {
            copy(actions, &dir, target)?;
        }
    }
    Ok(())
}

pub(super) fn seed_subset(
    actions: &Actions,
    source: &Path,
    target: &Path,
    with_incremental: bool,
) -> WorkflowResult {
    let debug_source = source.join("debug");
    if !debug_source.is_dir() {
        info(format!(
            "skip target subset: source debug dir missing at {}",
            debug_source.display()
        ));
        return Ok(());
    }
    if same_file(source, target) && !target.is_symlink() {
        info("skip target subset: source and target are the same path");
        return Ok(());
    }
    // A prior setup may have shared the entire target or a subdirectory.
    // Detach directory aliases before populating the destination-owned cache.
    owned_directory(actions, target)?;
    let debug_target = target.join("debug");
    owned_directory(actions, &debug_target)?;
    info("seed target subset: pruned high-value debug cache");
    debug_deps(
        actions,
        &debug_source.join("deps"),
        &debug_target.join("deps"),
        &keep_from_env("RESTREAM_AGENT_WORKTREE_LOCAL_ARTIFACT_KEEP", 3),
    )?;
    sync_tree(
        actions,
        &debug_source.join("build"),
        &debug_target.join("build"),
        "target debug/build",
    )?;
    sync_tree(
        actions,
        &debug_source.join(".fingerprint"),
        &debug_target.join(".fingerprint"),
        "target debug/.fingerprint",
    )?;
    for name in ROOT_FILES {
        let source_file = debug_source.join(name);
        if source_file.exists() {
            info(format!(
                "seed target debug/{name}: {} -> {}",
                source_file.display(),
                debug_target.join(name).display()
            ));
            actions.mkdir(&debug_target)?;
            copy(actions, &source_file, &debug_target.join(name))?;
        } else {
            info(format!(
                "skip target debug/{name}: source missing at {}",
                source_file.display()
            ));
        }
    }
    if with_incremental {
        incremental(
            actions,
            &debug_source.join("incremental"),
            &debug_target.join("incremental"),
            &keep_from_env("RESTREAM_AGENT_WORKTREE_INCREMENTAL_KEEP", 2),
        )?;
    } else {
        info("skip target debug/incremental: default warm subset leaves incremental state out");
    }
    Ok(())
}
