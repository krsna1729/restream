//! The process panic hook. The default hook writes stderr synchronously, so a
//! stalled collector would park a panicking Owner or Tokio worker
//! indefinitely. This hook queues the report on the stderr sink and waits at
//! most [`PANIC_FLUSH`] for it to be written. It bypasses the subscriber: a
//! panic inside a layer that holds a lock would deadlock on a nested event.

use std::backtrace::Backtrace;
use std::fmt::{Display, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::nonblocking::{self, NonBlocking};

/// How long a panicking thread waits for its report to reach stderr. A panic
/// that aborts (one raised while unwinding) ends the process right after the
/// hook, losing whatever is still queued; only a thread that is already
/// panicking waits, and never longer than this.
const PANIC_FLUSH: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BacktraceStyle {
    Off,
    Short,
    Full,
}

impl BacktraceStyle {
    /// `RUST_BACKTRACE` as std's panic hook reads it: `0` or unset is off,
    /// `full` is full, any other value is short. `RUST_LIB_BACKTRACE` does
    /// not apply: std consults it only for `Backtrace::capture`, not panics.
    fn from_env(value: Option<&str>) -> Self {
        match value {
            None | Some("0") => Self::Off,
            Some("full") => Self::Full,
            Some(_) => Self::Short,
        }
    }
}

/// Replaces the default panic hook. `rust_backtrace` is `RUST_BACKTRACE`
/// as `AppConfig` read it at startup.
pub(super) fn install(stderr: NonBlocking, rust_backtrace: Option<&str>) {
    let style = BacktraceStyle::from_env(rust_backtrace);
    let first_panic = AtomicBool::new(true);
    std::panic::set_hook(Box::new(move |info| {
        if nonblocking::on_writer_thread() {
            // The writer counts the line it lost; reporting it would queue
            // another line for a sink that may panic again.
            return;
        }
        let thread = std::thread::current();
        let location: &dyn Display = match info.location() {
            Some(location) => location,
            None => &"<unknown>",
        };
        let report = report(
            thread.name().unwrap_or("<unnamed>"),
            location,
            info.payload_as_str(),
            style,
            &first_panic,
        );
        deliver(&stderr, report.as_bytes());
    }));
}

/// The report std's default hook writes, without its OS thread id.
fn report(
    thread: &str,
    location: &dyn Display,
    payload: Option<&str>,
    style: BacktraceStyle,
    first_panic: &AtomicBool,
) -> String {
    let message = payload.unwrap_or("Box<dyn Any>");
    let mut report = format!("\nthread '{thread}' panicked at {location}:\n{message}\n");
    match style {
        BacktraceStyle::Short => {
            let _ = write!(report, "stack backtrace:\n{}", Backtrace::force_capture());
        }
        BacktraceStyle::Full => {
            let _ = write!(report, "stack backtrace:\n{:#}", Backtrace::force_capture());
        }
        BacktraceStyle::Off => {
            if first_panic.swap(false, Ordering::Relaxed) {
                report.push_str(
                    "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n",
                );
            }
        }
    }
    report
}

/// Queues `report` on the stderr sink, then waits up to [`PANIC_FLUSH`] for
/// it to be written.
fn deliver(stderr: &NonBlocking, report: &[u8]) {
    stderr.push(report);
    stderr.flush_until(Instant::now() + PANIC_FLUSH);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex};

    /// A sink that takes `delay` per write, like a slow terminal.
    struct SlowSink(Duration, Arc<Mutex<Vec<u8>>>);

    impl Write for SlowSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            std::thread::sleep(self.0);
            self.1.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// An aborting panic ends the process when the hook returns, so the
    /// report must be written by then.
    #[test]
    fn a_panic_report_is_written_before_the_hook_returns() {
        static LOST: AtomicU64 = AtomicU64::new(0);
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = SlowSink(Duration::from_millis(30), written.clone());
        let stderr = nonblocking::spawn("test-panic-slow", sink, 64, &LOST).unwrap();

        deliver(&stderr, b"thread 'x' panicked at here:\nboom\n");

        assert_eq!(
            written.lock().unwrap().as_slice(),
            b"thread 'x' panicked at here:\nboom\n"
        );
    }

    #[test]
    fn a_panic_report_waits_at_most_its_bound_while_stderr_is_stalled() {
        static LOST: AtomicU64 = AtomicU64::new(0);
        let sink = SlowSink(Duration::from_secs(30), Arc::default());
        let stderr = nonblocking::spawn("test-panic-stall", sink, 64, &LOST).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            deliver(&stderr, b"first\n");
            deliver(&stderr, b"second, behind the stalled first\n");
            let _ = done_tx.send(());
        });
        assert!(
            done_rx
                .recv_timeout(PANIC_FLUSH * 2 + Duration::from_secs(1))
                .is_ok(),
            "a panicking thread waited on a stalled stderr past its bound"
        );
    }

    #[test]
    fn a_panic_report_follows_the_default_hooks_format() {
        let first_panic = AtomicBool::new(true);
        let off = BacktraceStyle::Off;
        assert_eq!(
            report("worker", &"src/x.rs:1:2", None, off, &first_panic),
            "\nthread 'worker' panicked at src/x.rs:1:2:\nBox<dyn Any>\n\
             note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n"
        );
        assert_eq!(
            report("worker", &"src/x.rs:1:2", Some("boom"), off, &first_panic),
            "\nthread 'worker' panicked at src/x.rs:1:2:\nboom\n",
            "the hint is given on the first panic only"
        );
        assert!(
            report("w", &"l", Some("m"), BacktraceStyle::Short, &first_panic)
                .contains("stack backtrace:\n")
        );

        let style = BacktraceStyle::from_env;
        assert_eq!(style(None), BacktraceStyle::Off);
        assert_eq!(style(Some("0")), BacktraceStyle::Off);
        assert_eq!(style(Some("1")), BacktraceStyle::Short);
        assert_eq!(style(Some("full")), BacktraceStyle::Full);
    }
}
