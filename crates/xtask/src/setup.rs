use crate::workflow::{Error, WorkflowResult, checked, output};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

mod seccomp;

pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    match args {
        ["refresh-docker-seccomp", rest @ ..] => seccomp::run(rest),
        [other, ..] => Err(Error::usage(format!("setup: unknown command: {other}"))),
        [] => Err(Error::usage("usage: xtask setup refresh-docker-seccomp")),
    }
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
