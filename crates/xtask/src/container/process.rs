//! Container process ownership (off the media hot path), built on the
//! process-group primitives in `workflow`. Unlike `workflow::spawn`, these
//! guards still start children after an interruption, so cleanup runs, and
//! they kill descendants that outlive the reaped leader. Cancellation
//! forwards the received TERM/INT; a bounded grace period ends in KILL and
//! wait. Container ownership is separate: named/label cleanup survives a
//! killed engine client.
use crate::workflow::{self, Error, TempDir, WorkflowResult};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub(super) struct Signals;
impl Signals {
    pub(super) fn received(&self) -> i32 {
        workflow::interrupted().unwrap_or(0)
    }
    pub(super) fn check(&self) -> WorkflowResult {
        let signal = self.received();
        if signal == 0 {
            Ok(())
        } else {
            Err(Error::exit(
                (128 + signal) as u8,
                "container workflow interrupted",
            ))
        }
    }
    pub(super) fn sleep(&self, duration: Duration) -> WorkflowResult {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            self.check()?;
            std::thread::sleep(
                Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        self.check()
    }
}

pub(super) struct OwnedChild {
    child: Child,
    reaped: bool,
}
impl OwnedChild {
    pub(super) fn spawn(command: &mut Command) -> Result<Self, Error> {
        Ok(Self {
            child: workflow::spawn_group(command)?,
            reaped: false,
        })
    }
    pub(super) fn take_stdout(&mut self) -> Result<std::process::ChildStdout, Error> {
        self.child
            .stdout
            .take()
            .ok_or_else(|| "child stdout is not piped".into())
    }
    pub(super) fn write_stdin(&mut self, bytes: &[u8]) -> WorkflowResult {
        use std::io::Write;
        let mut input = self.child.stdin.take().ok_or("child stdin is not piped")?;
        input.write_all(bytes)?;
        Ok(())
    }
    /// Waits for the leader; an interruption stops the group first.
    pub(super) fn wait(&mut self) -> WorkflowResult {
        // `workflow::wait` reaps the leader on every outcome.
        let status = workflow::wait(&mut self.child);
        self.reaped = true;
        let status = status?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::status(status, "container subprocess failed"))
        }
    }
    pub(super) fn poll(&mut self) -> Result<Option<ExitStatus>, Error> {
        let status = self.child.try_wait()?;
        self.reaped |= status.is_some();
        Ok(status)
    }
    pub(super) fn signal(&mut self, signal: i32) {
        workflow::signal_group(&self.child, signal);
    }
    pub(super) fn terminate(&mut self, signal: i32, grace: Duration) {
        workflow::stop_group(&mut self.child, signal, grace);
        self.reaped = true;
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // The child group is owned until this guard is dropped, including on
        // spawn-following errors. Never detach a client or harness descendant.
        self.signal(libc::SIGKILL);
        if !self.reaped {
            let _ = self.child.wait();
        }
    }
}

pub(super) fn execute(
    command: &mut Command,
    signals: &Signals,
    limit: Option<Duration>,
) -> WorkflowResult {
    signals.check()?;
    eprintln!("+ {command:?}");
    let mut child = OwnedChild::spawn(command)?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.poll()? {
            return if status.success() {
                Ok(())
            } else {
                Err(Error::status(status, "container subprocess failed"))
            };
        }
        let signal = signals.received();
        if signal != 0 {
            child.terminate(signal, Duration::from_secs(5));
            return Err(Error::exit(
                (128 + signal) as u8,
                "container subprocess interrupted",
            ));
        }
        if limit.is_some_and(|limit| start.elapsed() >= limit) {
            child.terminate(libc::SIGTERM, Duration::from_secs(5));
            return Err(Error::exit(124, "container subprocess timed out"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Runs engine cleanup or a probe for at most 12 s, even when the engine
/// daemon is unavailable; dropping the guard then kills the group.
fn bounded(command: &mut Command) -> WorkflowResult {
    let mut child = OwnedChild::spawn(command)?;
    let until = Instant::now() + Duration::from_secs(12);
    while child.poll()?.is_none() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}
pub(super) fn quiet(command: &mut Command) {
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let _ = bounded(command);
}
pub(super) fn quiet_capture(command: &mut Command) -> String {
    let result = (|| -> Result<String, Error> {
        let directory = TempDir::new("restream-engine-output")?;
        let log = directory.path().join("output");
        let output = fs::File::create(&log)?;
        command.stdout(output.try_clone()?).stderr(output);
        bounded(command)?;
        Ok(fs::read_to_string(&log)?)
    })();
    result.unwrap_or_default()
}
pub(crate) fn install_executable(directory: &Path, name: &str) -> Result<PathBuf, Error> {
    fs::create_dir_all(directory)?;
    let source = std::env::current_exe()?;
    let destination = directory.join(name);
    if fs::canonicalize(&destination).ok().as_ref() == Some(&source) {
        return Ok(destination);
    }
    let stage = workflow::unique_dir(directory, ".xtask-install")?;
    let staged = stage.join(name);
    let result = (|| {
        if fs::hard_link(&source, &staged).is_err() {
            fs::copy(&source, &staged)?;
        }
        fs::rename(&staged, &destination)?;
        Ok(destination)
    })();
    let _ = fs::remove_dir_all(stage);
    result
}
