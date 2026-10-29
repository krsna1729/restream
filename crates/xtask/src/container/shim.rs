use super::process::{OwnedChild, Signals, quiet};
use crate::workflow::{Error, WorkflowResult};
use std::ffi::{OsStr, OsString};
use std::process::Command;
use std::time::{Duration, Instant};

pub(super) struct Settings {
    pub engine: OsString,
    pub image: OsString,
    pub name: String,
    pub label: Option<OsString>,
    pub seccomp: Option<OsString>,
    pub extra: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
}
impl Settings {
    fn environment() -> Result<Self, Error> {
        let image = std::env::var_os("CONTAINER_IMAGE")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::from("CONTAINER_IMAGE is required"))?;
        let mut extra = super::split_env("CONTAINER_EXTRA_ARGS");
        // Internal structured arguments avoid corrupting trust/media paths that
        // contain whitespace. User CONTAINER_EXTRA_ARGS retains word splitting.
        if let Some(json) = std::env::var_os("RESTREAM_CONTAINER_MOUNTS_JSON") {
            let parsed: Vec<String> = serde_json::from_str(&json.to_string_lossy())
                .map_err(|e| Error::from(format!("invalid container mount arguments: {e}")))?;
            extra.extend(parsed.into_iter().map(OsString::from));
        }
        Ok(Self {
            engine: std::env::var_os("CONTAINER_ENGINE")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "docker".into()),
            image,
            name: format!("restream-shim-{}", std::process::id()),
            label: std::env::var_os("CONTAINER_LABEL").filter(|s| !s.is_empty()),
            seccomp: std::env::var_os("CONTAINER_SECCOMP").filter(|s| !s.is_empty()),
            extra,
            environment: std::env::vars_os().collect(),
        })
    }
    pub(super) fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.engine);
        command.args(["run", "--rm", "--name", &self.name, "--network", "host"]);
        if let Some(label) = &self.label {
            command.arg("--label").arg(label);
        }
        if let Some(profile) = &self.seccomp {
            let mut option = OsString::from("seccomp=");
            option.push(profile);
            command.arg("--security-opt").arg(option);
        }
        command.args(&self.extra);
        for (key, value) in &self.environment {
            if forward_environment(key) {
                // Pass names, rather than expanding values into command strings.
                command.arg("-e").arg(key).env(key, value);
            }
        }
        command.arg(&self.image).args(args);
        command
    }
}
fn forward_environment(key: &OsStr) -> bool {
    let Some(key) = key.to_str() else {
        return false;
    };
    key.starts_with("RESTREAM_")
        && !matches!(
            key,
            "RESTREAM_DB_PATH"
                | "RESTREAM_LOG_DIR"
                | "RESTREAM_MEDIA_DIR"
                | "RESTREAM_BIN"
                | "RESTREAM_CONTAINER_MOUNTS_JSON"
        )
}

struct Container<'a>(&'a Settings);
impl Container<'_> {
    fn stop(&self) {
        quiet(Command::new(&self.0.engine).args(["stop", "--time", "5", &self.0.name]));
    }
}
impl Drop for Container<'_> {
    fn drop(&mut self) {
        self.stop();
        quiet(Command::new(&self.0.engine).args(["rm", "-f", &self.0.name]));
    }
}

pub(super) fn run_with(
    settings: &Settings,
    args: &[&str],
    signal: impl Fn() -> i32,
) -> WorkflowResult {
    // Establish container ownership before spawning: daemon-side creation may
    // succeed even when the client cannot subsequently return a status.
    let container = Container(settings);
    let mut client = OwnedChild::spawn(&mut settings.command(args))?;
    let mut stopping = None;
    loop {
        if let Some(status) = client.poll()? {
            if let Some((signal, _)) = stopping {
                return Err(Error::exit(
                    (128 + signal) as u8,
                    "container shim interrupted",
                ));
            }
            return if status.success() {
                Ok(())
            } else {
                Err(Error::status(status, "container runtime exited"))
            };
        }
        let received = signal();
        if received != 0 && stopping.is_none() {
            // Stop the daemon-owned container before signalling its local CLI;
            // killing the CLI alone leaves a live container behind.
            container.stop();
            client.signal(received);
            stopping = Some((received, Instant::now()));
        }
        if stopping
            .as_ref()
            .is_some_and(|(_, start)| start.elapsed() >= Duration::from_secs(5))
        {
            client.terminate(libc::SIGKILL, Duration::ZERO)?;
            return Err(Error::exit(
                (128 + stopping.unwrap().0) as u8,
                "container shim interrupted",
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
pub(super) fn run(args: &[&str]) -> WorkflowResult {
    let signals = Signals;
    let settings = Settings::environment()?;
    run_with(&settings, args, || signals.received())
}
