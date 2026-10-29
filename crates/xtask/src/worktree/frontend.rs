use super::{Actions, info};
use crate::workflow::{WorkflowResult, spawn, wait};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

const BINS: &[&str] = &["vite", "tsc", "tailwindcss"];
const FILES: &[&str] = &[
    "hls.js/dist/hls.min.js",
    "react/jsx-runtime.js",
    "react-dom/client.js",
    "@types/react/index.d.ts",
    "@types/react-dom/index.d.ts",
];

pub(super) fn ready(root: &Path) -> bool {
    if !crate::workflow::on_path("npm") {
        return false;
    }
    let modules = root.join("node_modules");
    if !BINS.iter().all(|name| {
        fs::metadata(modules.join(".bin").join(name))
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }) {
        return false;
    }
    if !FILES.iter().all(|name| modules.join(name).is_file()) {
        return false;
    }
    let mut command = Command::new("npm");
    command
        .arg("--prefix")
        .arg(root)
        .args(["ls", "--all", "--include=dev", "--omit=optional"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    spawn(&mut command)
        .and_then(|mut child| wait(&mut child))
        .is_ok_and(|status| status.success())
}

pub(super) fn ensure(actions: &Actions, root: &Path) -> WorkflowResult {
    if !root.join("package-lock.json").is_file() {
        info("skip frontend dependency check: package-lock.json missing in worktree");
        return Ok(());
    }
    if ready(root) {
        info("frontend dependencies ready in worktree");
        return Ok(());
    }
    if !crate::workflow::on_path("npm") {
        return Err("npm is required to hydrate frontend dependencies for the worktree".into());
    }
    info("install locked frontend dependencies in worktree");
    actions.command(
        Command::new("npm")
            .args(["ci", "--include=optional", "--prefix"])
            .arg(root),
    )?;
    if !ready(root) {
        return Err(format!(
            "frontend dependency check failed after npm ci in {}",
            root.display()
        )
        .into());
    }
    Ok(())
}
