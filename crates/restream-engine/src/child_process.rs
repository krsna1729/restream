//! Awaiting a child process from async code. [`reap`] and
//! [`crate::ffmpeg_binary::run_tool`] are the only callers of tokio's
//! `Child::wait`, `Child::wait_with_output` and `Command::output`
//! (`clippy.toml` `disallowed-methods`): each bounds the wait, so a stalled
//! child cannot park the task that owns it.

use std::process::ExitStatus;
use std::time::Duration;

/// How long [`reap`] waits for a child after SIGKILL. Only a child stuck in
/// an uninterruptible kernel wait outlives this; it is left to tokio's
/// orphan reaper once its `Child` drops.
const KILLED_EXIT_WITHIN: Duration = Duration::from_secs(5);

/// Waits up to `grace` for `child` to exit and returns its exit status.
/// Past `grace` the child is sent SIGKILL and awaited for at most
/// [`KILLED_EXIT_WITHIN`] more; `Duration::ZERO` kills it at once. `None`
/// when the status cannot be read: the wait failed, or the killed child did
/// not exit within that bound. Cancel-safe: dropping the future leaves the
/// child as it was, or killed.
#[expect(
    clippy::disallowed_methods,
    reason = "the single bounded `Child::wait` (clippy.toml)"
)]
pub async fn reap(child: &mut tokio::process::Child, grace: Duration) -> Option<ExitStatus> {
    if !grace.is_zero()
        && let Ok(status) = tokio::time::timeout(grace, child.wait()).await
    {
        return status.ok();
    }
    // Fails only when the child was already reaped; the wait below then
    // returns its status.
    let _ = child.start_kill();
    tokio::time::timeout(KILLED_EXIT_WITHIN, child.wait())
        .await
        .ok()?
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;
    use std::time::Instant;

    fn spawn(program: &str, args: &[&str]) -> tokio::process::Child {
        tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn test child")
    }

    #[tokio::test]
    async fn reap_kills_a_child_past_its_grace_and_returns_a_real_exit_status() {
        let grace = Duration::from_millis(200);
        let mut sleeper = spawn("sleep", &["30"]);
        let started = Instant::now();
        let status = reap(&mut sleeper, grace)
            .await
            .expect("killed child reaped");
        assert!(
            started.elapsed() < grace + Duration::from_secs(2),
            "reaped {:?} after the grace",
            started.elapsed()
        );
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "killed past the grace"
        );

        let mut quick = spawn("sh", &["-c", "exit 3"]);
        let status = reap(&mut quick, Duration::from_secs(10))
            .await
            .expect("exited child reaped");
        assert_eq!(status.code(), Some(3), "its own exit status");

        let mut sleeper = spawn("sleep", &["30"]);
        let status = reap(&mut sleeper, Duration::ZERO)
            .await
            .expect("killed child reaped");
        assert_eq!(status.signal(), Some(libc::SIGKILL), "zero grace kills now");
    }
}
