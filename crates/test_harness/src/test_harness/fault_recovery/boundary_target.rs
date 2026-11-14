//! Own a target and its process group until the final group signal and reap.
use std::io;
use std::process::{Output, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

type Capture = JoinHandle<io::Result<Vec<u8>>>;

pub(super) struct BoundaryTarget {
    // Drop signals the group BEFORE Rust drops/reaps the owned child. Once the
    // final signal is sent this becomes None; no signal follows a reap.
    group: Option<libc::pid_t>,
    child: Child,
    stdout: Capture,
    stderr: Capture,
}

impl BoundaryTarget {
    pub(super) fn spawn(command: &mut Command) -> Result<Self, String> {
        let mut child = command
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| error.to_string())?;
        let group = child.id().expect("a just-spawned unreaped child has a PID") as libc::pid_t;
        let stdout = capture(child.stdout.take().expect("piped stdout"));
        let stderr = capture(child.stderr.take().expect("piped stderr"));
        Ok(Self {
            group: Some(group),
            child,
            stdout,
            stderr,
        })
    }

    pub(super) async fn run(mut self, deadline: Duration) -> Result<Output, String> {
        let pid = self.group.expect("unreaped target group");
        // WNOWAIT leaves the leader unreaped, pinning its PID/group until the
        // final killpg. Pipe collectors run independently: a descendant holding
        // a pipe open cannot prevent us observing the leader's exit.
        let observed = tokio::time::timeout(deadline, exited_unreaped(pid))
            .await
            .map_err(|_| format!("live fault target exceeded {} ms", deadline.as_millis()))
            .and_then(|result| result);
        self.signal_group();
        // The zombie leader pins the group while signalled descendants exit.
        let group_exited = async {
            while tokio::task::spawn_blocking(move || group_running(pid))
                .await
                .map_err(|error| format!("observe target group: {error}"))??
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, String>(())
        }
        .await;
        // The child stays outside the timed future. Even timeout/observer errors
        // explicitly reap it before the caller can process::exit.
        let status = self
            .child
            .wait()
            .await
            .map_err(|error| format!("reap fault target: {error}"))?;
        let stdout = collect(&mut self.stdout).await?;
        group_exited?;
        let stderr = collect(&mut self.stderr).await?;
        observed?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    fn signal_group(&mut self) {
        if let Some(group) = self.group.take() {
            // SAFETY: this PID is still our owned, unreaped group leader. It
            // cannot be reused before this final group signal.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }
}

impl Drop for BoundaryTarget {
    fn drop(&mut self) {
        self.signal_group();
        self.stdout.abort();
        self.stderr.abort();
        // `child` is dropped only after this body, so cancellation signals the
        // group before kill_on_drop can release the leader's ownership.
    }
}

fn capture(mut pipe: impl AsyncRead + Unpin + Send + 'static) -> Capture {
    tokio::spawn(async move {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).await?;
        Ok(bytes)
    })
}

async fn collect(task: &mut Capture) -> Result<Vec<u8>, String> {
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .map_err(|_| "target output pipe did not close after group cleanup".to_string())?
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

async fn exited_unreaped(pid: libc::pid_t) -> Result<(), String> {
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: info has writable siginfo_t storage; pid is our owned child.
        // WNOHANG never blocks, WNOWAIT does not reap or release its PID.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("observe fault target: {error}"));
        }
        // SAFETY: successful waitid initialized info (zero si_pid means pending).
        if unsafe { info.assume_init().si_pid() } != 0 {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn group_running(group: libc::pid_t) -> Result<bool, String> {
    let entries =
        std::fs::read_dir("/proc").map_err(|error| format!("group inventory: {error}"))?;
    Ok(entries.filter_map(Result::ok).any(|entry| {
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            return false;
        };
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            return false;
        };
        let mut fields = fields.split_whitespace();
        let state = fields.next();
        let _parent = fields.next();
        let process_group = fields
            .next()
            .and_then(|value| value.parse::<libc::pid_t>().ok());
        process_group == Some(group) && !matches!(state, Some("Z" | "X"))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_reaped(pid: libc::pid_t) {
        let mut status = 0;
        // SAFETY: WNOHANG is nonblocking; this PID belongs to the runner under test.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        assert_eq!(result, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD),
            "leader must be reaped, not merely a zombie"
        );
    }

    async fn exercise(timeout: bool) {
        let path = std::env::temp_dir().join(format!(
            "restream-target-descendant-{}-{timeout}",
            std::process::id()
        ));
        let script = if timeout {
            "sleep 60 & echo $! > \"$1\"; printf out; printf err >&2; wait"
        } else {
            "sleep 60 & echo $! > \"$1\"; printf out; printf err >&2; exit 0"
        };
        let mut command = Command::new("sh");
        command.args(["-c", script, "fault-target"]).arg(&path);
        let target = BoundaryTarget::spawn(&mut command).expect("real runner target");
        let pid = target.group.unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            target.run(Duration::from_millis(300)),
        )
        .await
        .expect("cleanup bounded for ordinary live processes");
        assert_reaped(pid);
        let descendant = std::fs::read_to_string(&path)
            .expect("target started descendant")
            .trim()
            .parse::<libc::pid_t>()
            .unwrap();
        let stat = std::fs::read_to_string(format!("/proc/{descendant}/stat"));
        match stat {
            Ok(stat) => assert!(
                stat.rsplit_once(") ").is_some_and(|(_, fields)| {
                    matches!(fields.split_whitespace().next(), Some("Z" | "X"))
                }),
                "descendant must have exited BEFORE runner returns"
            ),
            Err(error) => assert_eq!(
                error.kind(),
                io::ErrorKind::NotFound,
                "only an absent process proves exit when proc stat cannot be read"
            ),
        }
        std::fs::remove_file(path).unwrap();
        if timeout {
            assert!(result.unwrap_err().contains("exceeded"));
        } else {
            let output = result.expect("normal leader exit with inherited descendant pipes");
            assert!(output.status.success());
            assert_eq!(output.stdout, b"out");
            assert_eq!(output.stderr, b"err");
        }
    }

    #[tokio::test]
    async fn boundary_target_normal_exit_reaps_leader_and_exits_descendant() {
        exercise(false).await;
    }

    #[tokio::test]
    async fn boundary_target_timeout_reaps_leader_and_exits_descendant() {
        exercise(true).await;
    }
}
