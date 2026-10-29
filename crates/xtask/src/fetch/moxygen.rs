use crate::workflow::{Error, WorkflowResult, checked};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const COMMIT: &str = "c392ebfc4d1dc39220fac3087b503416d06c47b9";
const LIBURING: &str = "f33bc2f24cf3b38df8553aab5f9714fc5cf0aff4";

fn checkout(url: &str, commit: &str, directory: &Path) -> WorkflowResult {
    let head = crate::setup::probe_output(
        Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(["rev-parse", "HEAD"])
            .stderr(Stdio::null()),
    )?;
    if head.is_some_and(|result| {
        String::from_utf8_lossy(&result.stdout).trim_end_matches('\n') == commit
    }) {
        return Ok(());
    }
    if directory.exists() {
        fs::remove_dir_all(directory)?;
    }
    checked(Command::new("git").args(["init", "-q"]).arg(directory))?;
    checked(
        Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(["fetch", "-q", "--depth", "1", url, commit]),
    )?;
    checked(
        Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(["checkout", "-q", "FETCH_HEAD"]),
    )
}

fn stamp_matches(path: &Path, commit: &str) -> bool {
    fs::read_to_string(path).is_ok_and(|text| text.trim_end_matches('\n') == commit)
}

pub(super) fn run(args: &[&str]) -> WorkflowResult {
    match args.first().copied() {
        None | Some("") => {}
        Some("-h" | "--help") => {
            println!(
                "Usage: xtask fetch moxygen\nBuild moxygen {COMMIT} and liburing {LIBURING} via pinned upstream getdeps.\nInstalls .local/bin/moxygen-c392ebfc/moqrelayserver and a .commit stamp.\nMOXYGEN_SCRATCH defaults to $HOME/.cache/restream-moxygen; MOXYGEN_BUILD_JOBS defaults to 3.\nNeeds roughly 6 GB scratch space, Python3, CMake and getdeps system packages.\nSystem packages may be installed explicitly with sudo python3 <checkout>/build/fbcode_builder/getdeps.py install-system-deps --recursive moxygen.\nUses CMAKE_POLICY_VERSION_MINIMUM=3.5, CFLAGS=-std=gnu17 and pinned liburing include/library paths; does not change protocol sources."
            );
            return Ok(());
        }
        Some(other) => {
            return Err(Error::usage(format!(
                "build-moxygen: unknown argument: {other}"
            )));
        }
    }
    let target_dir = crate::repo_root().join(format!(".local/bin/moxygen-{}", &COMMIT[..8]));
    let target = target_dir.join("moqrelayserver");
    let stamp = target_dir.join("moqrelayserver.commit");
    if crate::workflow::executable(&target) && stamp_matches(&stamp, COMMIT) {
        println!(
            "build-moxygen: {} already built from {COMMIT}",
            target.display()
        );
        return Ok(());
    }
    let scratch = match std::env::var_os("MOXYGEN_SCRATCH").filter(|value| !value.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(std::env::var_os("HOME").ok_or("build-moxygen: HOME is unset")?)
            .join(".cache/restream-moxygen"),
    };
    // The default is absolute, and an explicit relative scratch must remain valid
    // after changing into the upstream source directory.
    let scratch = if scratch.is_absolute() {
        scratch
    } else {
        std::env::current_dir()?.join(scratch)
    };
    let jobs = std::env::var_os("MOXYGEN_BUILD_JOBS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "3".into());
    fs::create_dir_all(&scratch)?;
    let uring_src = scratch.join("liburing-src");
    let uring = scratch.join("liburing");
    checkout(
        "https://github.com/axboe/liburing.git",
        LIBURING,
        &uring_src,
    )?;
    if !stamp_matches(&uring.join(".commit"), LIBURING) {
        if uring.exists() {
            fs::remove_dir_all(&uring)?;
        }
        let mut prefix = std::ffi::OsString::from("--prefix=");
        prefix.push(&uring);
        checked(
            Command::new("./configure")
                .arg(prefix)
                .current_dir(&uring_src)
                .stdout(Stdio::null()),
        )?;
        let mut parallel = std::ffi::OsString::from("-j");
        parallel.push(&jobs);
        checked(
            Command::new("make")
                .arg(parallel)
                .args(["-C", "src"])
                .current_dir(&uring_src)
                .stdout(Stdio::null()),
        )?;
        checked(
            Command::new("make")
                .args(["-C", "src", "install"])
                .current_dir(&uring_src)
                .stdout(Stdio::null()),
        )?;
        fs::write(uring.join(".commit"), format!("{LIBURING}\n"))?;
    }
    let source = scratch.join("moxygen-src");
    checkout(
        "https://github.com/facebookexperimental/moxygen.git",
        COMMIT,
        &source,
    )?;
    checked(
        Command::new("python3")
            .args([
                "build/fbcode_builder/getdeps.py",
                "build",
                "--allow-system-packages",
                "--src-dir=.",
                "--scratch-path",
            ])
            .arg(scratch.join("scratch"))
            .arg("--num-jobs")
            .arg(jobs)
            .args([
                "--no-tests",
                "--free-up-disk",
                "--build-type",
                "Release",
                "--cmake-target",
                "moqrelayserver",
                "moxygen",
            ])
            .env("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
            .env("CFLAGS", "-std=gnu17")
            .env("CPATH", uring.join("include"))
            .env("LIBRARY_PATH", uring.join("lib"))
            .current_dir(&source),
    )?;
    let built = [
        scratch.join("scratch/build/moxygen/moxygen/relay/moqrelayserver"),
        scratch.join("scratch/installed/moxygen/bin/moqrelayserver"),
    ]
    .into_iter()
    .find(|path| crate::workflow::executable(path))
    .ok_or_else(|| {
        format!(
            "build-moxygen: no moqrelayserver under {}/scratch",
            scratch.display()
        )
    })?;
    fs::create_dir_all(target_dir)?;
    crate::setup::install_atomic(&built, &target)?;
    fs::write(stamp, format!("{COMMIT}\n"))?;
    println!("build-moxygen: installed {} ({COMMIT})", target.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_complete_pinned_stamp_authorizes_reuse() {
        let temp = crate::workflow::TempDir::new("restream-xtask").unwrap();
        let stamp = temp.path().join(".commit");
        assert!(!stamp_matches(&stamp, COMMIT));
        fs::write(&stamp, &COMMIT[..8]).unwrap();
        assert!(!stamp_matches(&stamp, COMMIT));
        fs::write(&stamp, format!("{LIBURING}\n")).unwrap();
        assert!(!stamp_matches(&stamp, COMMIT));
        fs::write(&stamp, format!("{COMMIT}\n")).unwrap();
        assert!(stamp_matches(&stamp, COMMIT));
        fs::write(&stamp, format!("{COMMIT}\ninterrupted")).unwrap();
        assert!(!stamp_matches(&stamp, COMMIT));
    }
}
