use crate::workflow::{Error, WorkflowResult, checked, output};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

mod host;
mod seccomp;

pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    let (command, rest) = args.split_first().ok_or_else(|| Error::usage("usage: xtask setup prepare|install-git-hooks|refresh-docker-seccomp|harness-host-prereqs"))?;
    match *command {
        "prepare" => prepare(rest),
        "install-git-hooks" => install_hooks(rest),
        "refresh-docker-seccomp" => seccomp::run(rest),
        "harness-host-prereqs" => host::run(rest),
        other => Err(Error::usage(format!("setup: unknown command: {other}"))),
    }
}

fn prepare(args: &[&str]) -> WorkflowResult {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        println!(
            "usage: xtask setup prepare\nBuild the embedded frontend assets (requires bootstrap's npm dependencies)."
        );
        return Ok(());
    }
    if !args.is_empty() {
        return Err(Error::usage("usage: xtask setup prepare"));
    }
    let root = crate::repo_root();
    if !root.join("node_modules").is_dir() {
        return Err(Error::exit(
            2,
            "prepare: frontend dependencies are missing; run scripts/dev/bootstrap.sh first",
        ));
    }
    checked(
        Command::new("npm")
            .args(["run", "build:frontend"])
            .current_dir(root),
    )?;
    println!("prepare: embedded frontend assets are ready");
    Ok(())
}

fn install_hooks(args: &[&str]) -> WorkflowResult {
    if args.iter().any(|arg| matches!(*arg, "-h" | "--help")) {
        println!(
            "usage: xtask setup install-git-hooks\nPoint core.hooksPath at the tracked .githooks entrypoints and mark them executable."
        );
        return Ok(());
    }
    if !args.is_empty() {
        return Err(Error::usage("usage: xtask setup install-git-hooks"));
    }
    let root = crate::repo_root();
    let mut child = crate::workflow::spawn(
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["rev-parse", "--is-inside-work-tree"])
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;
    let status = crate::workflow::wait(&mut child)?;
    if !status.success() {
        return Err(format!(
            "install-git-hooks: {} is not a Git worktree",
            root.display()
        )
        .into());
    }
    let hooks = root.join(".githooks");
    for entry in fs::read_dir(&hooks)? {
        let path = entry?.path();
        let mode = fs::metadata(&path)?.permissions().mode();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode | 0o111))?;
    }
    checked(
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["config", "core.hooksPath"])
            .arg(&hooks),
    )?;
    println!(
        "install-git-hooks: Git hooks installed to {}",
        hooks.display()
    );
    Ok(())
}

pub(crate) fn install_atomic(source: &Path, target: &Path) -> WorkflowResult {
    let mut partial = target.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    checked(
        Command::new("install")
            .args(["-m", "0755"])
            .arg(source)
            .arg(&partial),
    )?;
    fs::rename(&partial, target)?;
    Ok(())
}

pub(crate) fn sha256(path: &Path) -> Result<String, Error> {
    let result = output(Command::new("sha256sum").arg(path))?;
    String::from_utf8_lossy(&result.stdout)
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| "sha256sum returned no digest".into())
}

pub(crate) fn probe_output(command: &mut Command) -> Result<Option<std::process::Output>, Error> {
    match output(command) {
        Ok(output) => Ok(Some(output)),
        Err(error) if crate::workflow::interrupted().is_some() => Err(error),
        Err(_) => Ok(None),
    }
}
