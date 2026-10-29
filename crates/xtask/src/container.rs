//! Docker-compatible shipped-image proof and compiler-free CI runtime wrappers.
mod process;
mod shim;
mod smoke;
#[cfg(test)]
mod tests;

use crate::workflow::{Error, WorkflowResult, env_or};
pub(crate) use process::install_executable;
use process::{Signals, execute};
use std::ffi::OsString;
use std::fs;
use std::process::{Command, Stdio};

pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    match args {
        ["smoke", rest @ ..] => smoke::run(rest),
        ["restream-shim", rest @ ..] => shim(rest),
        ["run-in-harness-runtime", rest @ ..] => runtime(rest, false),
        ["run-harness-shard-in-runtime", rest @ ..] => runtime(rest, true),
        _ => Err(Error::usage(
            "usage: cargo xtask container <smoke|restream-shim|run-in-harness-runtime|run-harness-shard-in-runtime> [args...]",
        )),
    }
}
/// Called by argv0 before main changes directory; needs no checkout or Cargo.
pub(crate) fn shim(args: &[&str]) -> WorkflowResult {
    shim::run(args)
}

/// The original's intentionally unquoted `$CONTAINER_EXTRA_ARGS` word split.
fn split_env(key: &str) -> Vec<OsString> {
    std::env::var(key)
        .unwrap_or_default()
        .split_ascii_whitespace()
        .map(OsString::from)
        .collect()
}

fn runtime(args: &[&str], shard: bool) -> WorkflowResult {
    let usage = if shard {
        "usage: cargo xtask container run-harness-shard-in-runtime <release-shard> [harness args...]"
    } else {
        "usage: cargo xtask container run-in-harness-runtime <command> [args...]"
    };
    match args {
        [] => return Err(Error::usage(usage)),
        ["-h" | "--help"] => {
            println!("{usage}");
            return Ok(());
        }
        _ => {}
    }
    let root = crate::repo_root().canonicalize()?;
    let home = root.join(".local/ci-container-home");
    fs::create_dir_all(&home)?;
    let repository = env_or("GITHUB_REPOSITORY", "krsna1729/restream");
    let image = env_or(
        "RESTREAM_CI_HARNESS_IMAGE",
        &format!("ghcr.io/{repository}-ci-harness-runtime:ubuntu24"),
    );
    let signals = Signals;
    if env_or("RESTREAM_CI_HARNESS_PULL", "1") == "1" {
        let token = env_or("GH_TOKEN", &env_or("GITHUB_TOKEN", ""));
        if image.starts_with("ghcr.io/") && !token.is_empty() {
            println!("ci-harness-runtime: logging in to ghcr.io for runtime image pull");
            let mut login = Command::new("docker");
            login
                .args([
                    "login",
                    "ghcr.io",
                    "--username",
                    &env_or("GITHUB_ACTOR", "oauth2"),
                    "--password-stdin",
                ])
                .stdin(Stdio::piped());
            let mut child = process::OwnedChild::spawn(&mut login)?;
            // Credentials only reach stdin, never argv or diagnostics.
            child.write_stdin(format!("{token}\n").as_bytes())?;
            child.wait(&signals)?;
        }
        println!("ci-harness-runtime: pulling {image}");
        execute(
            Command::new("docker").args(["pull", &image]),
            &signals,
            None,
        )?;
    }
    let name = format!("restream-ci-harness-{}", std::process::id());
    let _container = RuntimeContainer(&name);
    let mut command = runtime_command(&root, &home, &image, shard, &name);
    if shard {
        // The image has no Cargo: run this prebuilt workflow binary, which
        // reaches the bench binaries with BENCH_BUILD=never.
        let executable = install_executable(&root.join(".local/bin"), "restream-xtask-runtime")?;
        println!("ci-harness-runtime: run shard={} image={image}", args[0]);
        command
            .arg(executable)
            .args(["qualify", "harness-shard"])
            .args(args);
    } else {
        println!(
            "ci-harness-runtime: run command={} image={image}",
            args.join(" ")
        );
        command.args(args);
    }
    execute(&mut command, &signals, None)
}

struct RuntimeContainer<'a>(&'a str);
impl Drop for RuntimeContainer<'_> {
    fn drop(&mut self) {
        process::quiet(Command::new("docker").args(["stop", "--time", "5", self.0]));
        process::quiet(Command::new("docker").args(["rm", "-f", self.0]));
    }
}

fn runtime_command(
    root: &std::path::Path,
    home: &std::path::Path,
    image: &str,
    shard: bool,
    name: &str,
) -> Command {
    // SAFETY: geteuid/getegid only read this process's credentials.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let mut command = Command::new("docker");
    command
        .args([
            "run",
            "--rm",
            "--name",
            name,
            "--network",
            "host",
            "--security-opt",
        ])
        .arg(format!(
            "seccomp={}",
            root.join("distribution/docker/restream-seccomp.json")
                .display()
        ))
        .arg("--user")
        .arg(format!("{uid}:{gid}"))
        .arg("-e")
        .arg(format!("HOME={}", home.display()))
        .arg("-e")
        .arg(format!("BENCH_BUILD={}", env_or("BENCH_BUILD", "never")))
        .arg("-e")
        .arg(format!("GITHUB_SHA={}", env_or("GITHUB_SHA", "")))
        .arg("-e")
        .arg(format!("RESTREAM_REPO_ROOT={}", root.display()));
    let keys: &[&str] = if shard {
        &[
            "RELEASE_VERSION",
            "RELEASE_HARNESS_SHARD_TIMEOUT",
            "HARNESS_BIN",
            "RESTREAM_BIN",
        ]
    } else {
        &[
            "TEST_HARNESS_USE_HOST_NET",
            "INTERNAL_BACKEND_SMOKE_CONTINUE_ON_FAIL",
            "INTERNAL_BACKEND_SMOKE_ARTIFACT_DIR",
        ]
    };
    for key in keys {
        command.arg("-e").arg(format!("{key}={}", env_or(key, "")));
    }
    command
        .arg("-v")
        .arg(format!("{}:{}", root.display(), root.display()))
        .arg("-w")
        .arg(root)
        .arg(image);
    command
}
