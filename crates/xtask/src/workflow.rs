//! Process execution for repository workflows, never the media data path.

use std::env;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) type WorkflowResult = Result<(), Error>;

#[derive(Debug)]
pub(crate) struct Error {
    pub(crate) code: u8,
    message: String,
}

impl Error {
    pub(crate) fn usage(message: impl Into<String>) -> Self {
        Self::exit(2, message)
    }

    pub(crate) fn exit(code: u8, message: impl Into<String>) -> Self {
        Self {
            code: code.max(1),
            message: message.into(),
        }
    }

    pub(crate) fn status(status: ExitStatus, message: impl Into<String>) -> Self {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        let code = status.code().unwrap_or_else(|| {
            #[cfg(unix)]
            {
                status.signal().map_or(1, |signal| 128 + signal)
            }
            #[cfg(not(unix))]
            {
                1
            }
        });
        Self::exit(code.clamp(1, 255) as u8, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::exit(1, message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self::exit(1, message)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        error.to_string().into()
    }
}

static INTERRUPTED: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn on_signal(signal: libc::c_int) {
    // A lock-free scalar store is the only operation in the signal handler.
    INTERRUPTED.store(signal, Ordering::Relaxed);
}

pub(crate) fn install_signals() -> WorkflowResult {
    #[cfg(unix)]
    {
        // SAFETY: sigaction is initialized before use; the handler has the C
        // signal ABI and performs no allocation, locking or I/O.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            for signal in [libc::SIGINT, libc::SIGTERM] {
                if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                    return Err(format!(
                        "cannot install signal handler: {}",
                        io::Error::last_os_error()
                    )
                    .into());
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn interrupted() -> Option<i32> {
    match INTERRUPTED.load(Ordering::Relaxed) {
        0 => None,
        signal => Some(signal),
    }
}

fn interruption(signal: i32) -> Error {
    Error::exit(
        (128 + signal).clamp(1, 255) as u8,
        format!("interrupted by signal {signal}"),
    )
}

pub(crate) fn spawn(command: &mut Command) -> Result<Child, Error> {
    if let Some(signal) = interrupted() {
        return Err(interruption(signal));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // The child and its ordinary descendants form an owned group. Never
        // signal the calling terminal's group or find processes by name.
        command.process_group(0);
    }
    command.spawn().map_err(|error| {
        format!(
            "cannot start {}: {error}",
            command.get_program().to_string_lossy()
        )
        .into()
    })
}

#[cfg(unix)]
fn signal_group(child: &Child, signal: i32) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: this group was created by spawn for this recorded child;
        // negative PID addresses that group, not unrelated named processes.
        unsafe {
            libc::kill(-pid, signal);
        }
    }
}

#[cfg(unix)]
fn group_exists(child: &Child) -> bool {
    let Ok(pid) = i32::try_from(child.id()) else {
        return false;
    };
    // SAFETY: signal zero only checks the recorded group; it sends no signal.
    unsafe { libc::kill(-pid, 0) == 0 }
}

/// Best-effort bounded cleanup, including descendants after the leader exits.
/// Call only for a child created by spawn, and only while its owner is live.
pub(crate) fn terminate(child: &mut Child) {
    #[cfg(unix)]
    signal_group(child, libc::SIGTERM);
    #[cfg(not(unix))]
    let _ = child.kill();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let reaped = matches!(child.try_wait(), Ok(Some(_)));
        #[cfg(unix)]
        let descendants = group_exists(child);
        #[cfg(not(unix))]
        let descendants = false;
        if reaped && !descendants {
            return;
        }
        if Instant::now() >= until {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    #[cfg(unix)]
    signal_group(child, libc::SIGKILL);
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn wait(child: &mut Child) -> Result<ExitStatus, Error> {
    loop {
        if let Some(signal) = interrupted() {
            terminate(child);
            return Err(interruption(signal));
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                terminate(child);
                return Err(error.into());
            }
        }
    }
}

/// Owns a child spawned into its own group: dropping it before it is reaped
/// terminates and reaps the whole group, so a started child is never
/// abandoned on an error, a failed sibling or an interruption.
pub(crate) struct OwnedChild(Option<Child>);

impl OwnedChild {
    pub(crate) fn spawn(command: &mut Command) -> Result<Self, Error> {
        Ok(Self(Some(spawn(command)?)))
    }

    pub(crate) fn child(&mut self) -> &mut Child {
        self.0.as_mut().expect("owned child has not been reaped")
    }

    /// `wait` reaps the child on every outcome (on an error, after
    /// terminating it), so the guard never signals a reused PID afterwards.
    pub(crate) fn wait(&mut self) -> Result<ExitStatus, Error> {
        let mut child = self.0.take().expect("owned child has not been reaped");
        wait(&mut child)
    }

    /// Records that the caller reaped the child itself (`try_wait`).
    pub(crate) fn reaped(&mut self) {
        self.0.take();
    }

    pub(crate) fn terminate(&mut self) {
        if let Some(mut child) = self.0.take() {
            terminate(&mut child);
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Runs a child to completion; its stdio is the caller's choice (inherited by
/// default), except inside `capture_into`, which owns both output streams.
pub(crate) fn checked(command: &mut Command) -> WorkflowResult {
    // Never log Command's Debug form: it includes environment values, which
    // may contain credentials. stdout can be a machine-readable result.
    eprintln!("==> {}", command.get_program().to_string_lossy());
    if let Some((stdout, stderr)) = crate::captured_output()? {
        command.stdout(stdout).stderr(stderr);
    }
    #[cfg(unix)]
    let terminal = Terminal::handoff(command);
    let mut child = spawn(command)?;
    #[cfg(unix)]
    let _foreground = terminal.map(|terminal| terminal.give(&child));
    let status = wait(&mut child)?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::status(
            status,
            format!(
                "{} failed ({status})",
                command.get_program().to_string_lossy()
            ),
        ))
    }
}

/// A sequential child owns the controlling terminal while it runs, as under
/// a job-control shell: it can prompt (sudo, credentials) instead of being
/// stopped by SIGTTIN, and a terminal ^C reaches its whole group. Concurrent
/// children (`spawn`) stay in the background and are stopped by `terminate`.
#[cfg(unix)]
struct Terminal {
    previous: libc::pid_t,
}

#[cfg(unix)]
impl Terminal {
    fn handoff(command: &mut Command) -> Option<Self> {
        use std::os::unix::process::CommandExt;
        // SAFETY: isatty/tcgetpgrp/getpgrp only inspect this process's stdin
        // and process group.
        let previous = unsafe {
            if libc::isatty(libc::STDIN_FILENO) != 1 {
                return None;
            }
            let foreground = libc::tcgetpgrp(libc::STDIN_FILENO);
            if foreground != libc::getpgrp() {
                return None;
            }
            foreground
        };
        // SAFETY: runs in the forked child after its own process group is
        // set; isatty, getpid, sigprocmask and tcsetpgrp are async-signal-safe.
        // Claiming the terminal in the child too closes the race with an early
        // terminal read. A redirected stdin is left alone.
        unsafe {
            command.pre_exec(|| {
                if libc::isatty(libc::STDIN_FILENO) == 1 {
                    set_foreground(libc::getpid());
                }
                Ok(())
            });
        }
        Some(Self { previous })
    }

    fn give(self, child: &Child) -> Self {
        if let Ok(group) = libc::pid_t::try_from(child.id()) {
            set_foreground(group);
        }
        self
    }
}

#[cfg(unix)]
impl Drop for Terminal {
    fn drop(&mut self) {
        set_foreground(self.previous);
    }
}

/// Makes `group` the terminal's foreground group; SIGTTOU is blocked so a
/// background caller is not stopped by the change itself.
#[cfg(unix)]
fn set_foreground(group: libc::pid_t) {
    // SAFETY: the masks are initialized by sigemptyset/sigprocmask before
    // use, and the previous mask is restored on the same thread.
    unsafe {
        let mut block: libc::sigset_t = std::mem::zeroed();
        let mut previous: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut block);
        libc::sigaddset(&mut block, libc::SIGTTOU);
        libc::sigprocmask(libc::SIG_BLOCK, &block, &mut previous);
        libc::tcsetpgrp(libc::STDIN_FILENO, group);
        libc::sigprocmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
    }
}

/// Capture both streams without rejecting a nonzero exit (for probes like ldd).
pub(crate) fn capture(command: &mut Command) -> Result<Output, Error> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = spawn(command)?;
    let stdout = child.stdout.take().ok_or("missing child stdout pipe")?;
    let stderr = child.stderr.take().ok_or("missing child stderr pipe")?;
    let read = |mut stream: Box<dyn Read + Send>| -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let stdout_reader = thread::spawn(move || read(Box::new(stdout)));
    let stderr_reader = thread::spawn(move || read(Box::new(stderr)));
    let status = wait(&mut child);
    let stdout = stdout_reader
        .join()
        .map_err(|_| Error::from("child stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| Error::from("child stderr reader panicked"))??;
    Ok(Output {
        status: status?,
        stdout,
        stderr,
    })
}

pub(crate) fn output(command: &mut Command) -> Result<Output, Error> {
    let output = capture(command)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(Error::status(
        output.status,
        format!(
            "{} failed ({}): {}",
            command.get_program().to_string_lossy(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    ))
}

/// The shell's `${name:-default}`: a nonempty environment value, else `default`.
pub(crate) fn env_or(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// The shell's `[[ -s path ]]`.
pub(crate) fn nonempty(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0)
}

/// The shell's `[[ -f path && -x path ]]`: a file executable by the effective
/// identity, including ACLs and mount policy.
pub(crate) fn executable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `name` is a NUL-terminated path that outlives the call.
    path.is_file()
        && unsafe {
            libc::faccessat(libc::AT_FDCWD, name.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0
        }
}

/// The shell's `command -v name` for an external program.
pub(crate) fn on_path(name: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|dir| executable(&dir.join(name))))
}

/// A private (0700) directory below the system temporary directory, removed
/// with its contents on drop (the shell's `mktemp -d` plus an EXIT trap).
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(prefix: &str) -> Result<Self, Error> {
        use std::os::unix::fs::DirBuilderExt;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let parent = fs::canonicalize(env::temp_dir())?;
        loop {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!("{prefix}-{}-{serial}", std::process::id()));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_an_owned_group_terminates_and_reaps_it() {
        let mut child = OwnedChild::spawn(Command::new("sleep").arg("60")).unwrap();
        let pid = child.child().id() as libc::pid_t;
        drop(child);
        let mut status = 0;
        // SAFETY: WNOHANG only queries this process's own (already reaped) child.
        let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        assert_eq!(reaped, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn failed_child_status_maps_to_its_exit_code_and_signal() {
        let error = checked(&mut Command::new("false")).unwrap_err();
        assert_eq!(error.code, 1);
        let mut child = spawn(Command::new("sleep").arg("60")).unwrap();
        // SAFETY: signals only the group this test just spawned.
        unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGTERM) };
        let status = wait(&mut child).unwrap();
        assert_eq!(
            Error::status(status, "terminated").code,
            128 + libc::SIGTERM as u8
        );
    }
}
