//! Process ownership for CLI workflows (off the media hot path).
//! A spawned process group has exactly one reaper. Cancellation first reaches
//! the group as TERM/INT; a bounded grace period ends in KILL and wait. Container
//! ownership is separate: named/label cleanup survives a killed engine client.
use crate::workflow::{Error, WorkflowResult};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static SERIAL: AtomicU64 = AtomicU64::new(0);

pub(super) struct Signals;
impl Signals {
    pub(super) fn received(&self) -> i32 {
        crate::workflow::interrupted().unwrap_or(0)
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
        command.process_group(0);
        Ok(Self {
            child: command.spawn()?,
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
    pub(super) fn wait(&mut self, signals: &Signals) -> WorkflowResult {
        loop {
            signals.check().or_else(|error| {
                self.terminate(libc::SIGTERM, Duration::from_secs(5))?;
                Err(error)
            })?;
            if let Some(status) = self.poll()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(Error::status(status, "container subprocess failed"))
                };
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    pub(super) fn poll(&mut self) -> Result<Option<ExitStatus>, Error> {
        let status = self.child.try_wait()?;
        self.reaped |= status.is_some();
        Ok(status)
    }
    pub(super) fn signal(&mut self, signal: i32) {
        // SAFETY: the child was spawned as its own process-group leader; the
        // negative PID addresses only that owned group.
        unsafe {
            libc::kill(-(self.child.id() as i32), signal);
        }
    }
    pub(super) fn terminate(&mut self, signal: i32, grace: Duration) -> Result<(), Error> {
        self.signal(signal);
        let until = Instant::now() + grace;
        while self.poll()?.is_none() {
            if Instant::now() >= until {
                self.signal(libc::SIGKILL);
                self.child.wait()?;
                self.reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        // A descendant may outlive its direct parent after TERM.
        self.signal(libc::SIGKILL);
        Ok(())
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
            child.terminate(signal, Duration::from_secs(5))?;
            return Err(Error::exit(
                (128 + signal) as u8,
                "container subprocess interrupted",
            ));
        }
        if limit.is_some_and(|limit| start.elapsed() >= limit) {
            child.terminate(libc::SIGTERM, Duration::from_secs(5))?;
            return Err(Error::exit(124, "container subprocess timed out"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn quiet(command: &mut Command) {
    // Cleanup must be bounded even when the engine daemon is unavailable.
    command.stdout(Stdio::null()).stderr(Stdio::null());
    if let Ok(mut child) = OwnedChild::spawn(command) {
        let until = Instant::now() + Duration::from_secs(12);
        while matches!(child.poll(), Ok(None)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
pub(super) fn quiet_capture(command: &mut Command) -> String {
    let Ok(directory) = unique_dir(&std::env::temp_dir(), "restream-engine-output") else {
        return String::new();
    };
    let log = directory.join("output");
    let result = (|| -> Result<String, Error> {
        let output = fs::File::create(&log)?;
        command.stdout(output.try_clone()?).stderr(output);
        let mut child = OwnedChild::spawn(command)?;
        let until = Instant::now() + Duration::from_secs(12);
        while child.poll()?.is_none() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(25));
        }
        drop(child);
        Ok(fs::read_to_string(&log)?)
    })();
    let _ = fs::remove_dir_all(directory);
    result.unwrap_or_default()
}
pub(super) fn unique_dir(parent: &Path, prefix: &str) -> Result<PathBuf, Error> {
    fs::create_dir_all(parent)?;
    loop {
        let path = parent.join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
}
pub(crate) fn install_executable(directory: &Path, name: &str) -> Result<PathBuf, Error> {
    fs::create_dir_all(directory)?;
    let source = std::env::current_exe()?;
    let destination = directory.join(name);
    if fs::canonicalize(&destination).ok().as_ref() == Some(&source) {
        return Ok(destination);
    }
    let stage = unique_dir(directory, ".xtask-install")?;
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
