use crate::workflow::{Error, WorkflowResult, on_path};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn xtask() -> Result<Command, Error> {
    Ok(Command::new(std::env::current_exe()?))
}

pub(super) fn require_command(name: &str, caller: &str) -> WorkflowResult {
    if on_path(name) {
        Ok(())
    } else {
        Err(format!("{caller}: required command not found: {name}").into())
    }
}

/// Match find's default non-symlink-following traversal and sorted file results.
pub(super) fn files(root: &Path, recursive: bool) -> Result<Vec<PathBuf>, Error> {
    let mut result = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_file() {
            result.push(entry.path());
        } else if recursive && kind.is_dir() {
            result.extend(files(&entry.path(), true)?);
        }
    }
    result.sort();
    Ok(result)
}

pub(super) fn sboms(root: &Path, recursive: bool) -> Result<Vec<PathBuf>, Error> {
    Ok(files(root, recursive)?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".sbom.cdx.json"))
        })
        .collect())
}
