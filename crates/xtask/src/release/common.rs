use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use crate::workflow::{Error, WorkflowResult, checked, output};

pub(crate) fn require_version(caller: &str, version: &str) -> WorkflowResult {
    let digits = version.strip_prefix('v').unwrap_or(version);
    if version.is_empty() {
        return Err(Error::usage(format!("{caller}: version is required")));
    }
    if !digits.as_bytes().first().is_some_and(u8::is_ascii_digit)
        || !digits
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
    {
        return Err(Error::usage(format!(
            "{caller}: version must be filename-safe: {version}"
        )));
    }
    Ok(())
}

pub(crate) fn require_tag(caller: &str, tag: &str) -> WorkflowResult {
    if tag.is_empty() {
        return Err(Error::usage(format!("{caller}: tag is required")));
    }
    let valid = tag.strip_prefix('v').is_some_and(|s| {
        s.as_bytes().first().is_some_and(u8::is_ascii_digit)
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    });
    if !valid {
        return Err(Error::usage(format!(
            "{caller}: tag must start with v and be release-safe, got: {tag}"
        )));
    }
    Ok(())
}

pub(crate) fn require_clean_checkout(caller: &str) -> WorkflowResult {
    if !output(Command::new("git").args(["status", "--porcelain"]))?
        .stdout
        .is_empty()
    {
        checked(Command::new("git").args(["status", "--short"]))?;
        return Err(format!("{caller}: checkout must be clean before tagging").into());
    }
    Ok(())
}

pub(super) fn git(args: &[&str]) -> Result<String, Error> {
    Ok(
        String::from_utf8_lossy(&output(Command::new("git").args(args))?.stdout)
            .trim_end_matches('\n')
            .to_owned(),
    )
}

pub(super) fn require_commands(commands: &[&str]) -> WorkflowResult {
    match commands
        .iter()
        .find(|command| !crate::workflow::on_path(command))
    {
        Some(command) => Err(format!("required command not found: {command}").into()),
        None => Ok(()),
    }
}

pub(super) fn install(source: &Path, destination: &Path, mode: u32) -> WorkflowResult {
    fs::copy(source, destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_allow_build_metadata_but_never_paths() {
        for value in ["1", "v1.2.3-rc.1", "1.2.3+linux_x86-64"] {
            assert!(require_version("test", value).is_ok(), "{value}");
        }
        for value in ["", "v", "vv1", "-1", "../v1", "v1/a", "1\n2", "١"] {
            assert!(require_version("test", value).is_err(), "{value:?}");
        }
    }

    #[test]
    fn release_tags_are_stricter_than_archive_versions() {
        for value in ["v1", "v1.2.3-rc_1"] {
            assert!(require_tag("test", value).is_ok());
        }
        for value in ["1.2.3", "v1+metadata", "v1/path", "v", "v1\n"] {
            assert!(require_tag("test", value).is_err());
        }
    }
}
