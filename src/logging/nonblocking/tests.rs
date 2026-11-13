use super::*;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};

const LINES: usize = 10_000;
const CAPACITY: usize = 64;
/// Ample for 10k formatted lines; a logging thread stuck on the sink never makes it.
const PROMPT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Gate {
    open: bool,
    parked: bool,
    written: Vec<u8>,
}

/// A sink whose writes park until the gate opens, like stdout behind a pipe
/// nobody reads.
#[derive(Clone, Default)]
struct GatedSink(Arc<(Mutex<Gate>, Condvar)>);

impl GatedSink {
    fn open(&self) {
        self.0.0.lock().unwrap().open = true;
        self.0.1.notify_all();
    }

    fn wait_until_parked(&self) {
        let (gate, cvar) = &*self.0;
        let guard = gate.lock().unwrap();
        let (_guard, timeout) = cvar
            .wait_timeout_while(guard, PROMPT, |gate| !gate.parked)
            .unwrap();
        assert!(!timeout.timed_out(), "the writer never reached the sink");
    }

    fn lines_containing(&self, needle: &str) -> usize {
        let written = self.0.0.lock().unwrap().written.clone();
        String::from_utf8(written)
            .unwrap()
            .lines()
            .filter(|line| line.contains(needle))
            .count()
    }
}

impl Write for GatedSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (gate, cvar) = &*self.0;
        let mut guard = gate.lock().unwrap();
        guard.parked = true;
        cvar.notify_all();
        let mut guard = cvar.wait_while(guard, |gate| !gate.open).unwrap();
        guard.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Runs `f` on its own thread; `None` if it is still running after `limit`.
fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(limit).ok()
}

/// Logs `lines` events through a `fmt` subscriber writing to `writer`, on a
/// thread of its own; returns whether that thread finished in time.
fn log_lines(writer: &NonBlocking, message: &'static str, lines: usize) -> bool {
    let writer = writer.clone();
    within(PROMPT, move || {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            for line in 0..lines {
                tracing::info!(line, "{message}");
            }
        });
    })
    .is_some()
}

#[test]
fn a_thread_that_logs_never_waits_for_a_blocked_sink_and_drops_are_counted() {
    static LOST: AtomicU64 = AtomicU64::new(0);
    let sink = GatedSink::default();
    let writer = spawn("test-log-blocked", sink.clone(), CAPACITY, &LOST).unwrap();

    assert!(
        log_lines(&writer, "while blocked", LINES),
        "the logging thread waited on a blocked sink"
    );
    let lost = LOST.load(Ordering::Relaxed) as usize;
    assert!(
        lost >= LINES - CAPACITY - 1,
        "{lost} of {LINES} lines counted lost; the queue holds {CAPACITY}"
    );

    sink.open();
    assert!(writer.flush_until(Instant::now() + PROMPT));
    assert_eq!(
        sink.lines_containing("while blocked") + LOST.load(Ordering::Relaxed) as usize,
        LINES,
        "every line is either written or counted lost"
    );
}

#[test]
fn a_writer_recovers_once_its_sink_unblocks() {
    static LOST: AtomicU64 = AtomicU64::new(0);
    let sink = GatedSink::default();
    let writer = spawn("test-log-recover", sink.clone(), CAPACITY, &LOST).unwrap();
    assert!(log_lines(&writer, "while blocked", LINES));
    let lost_while_blocked = LOST.load(Ordering::Relaxed);
    assert!(lost_while_blocked > 0);

    sink.open();
    assert!(writer.flush_until(Instant::now() + PROMPT));
    assert!(log_lines(&writer, "after recovery", CAPACITY));
    assert!(writer.flush_until(Instant::now() + PROMPT));

    assert_eq!(sink.lines_containing("after recovery"), CAPACITY);
    assert_eq!(LOST.load(Ordering::Relaxed), lost_while_blocked);
}

#[test]
fn a_shutdown_flush_returns_by_its_deadline_while_the_sink_is_blocked() {
    static LOST: AtomicU64 = AtomicU64::new(0);
    const DEADLINE: Duration = Duration::from_millis(200);
    const MARGIN: Duration = Duration::from_secs(1);
    let sink = GatedSink::default();
    let writer = spawn("test-log-flush", sink.clone(), CAPACITY, &LOST).unwrap();
    writer.push(b"held by the sink\n");
    sink.wait_until_parked();

    // The flush request fits the queue, and the writer never answers it.
    let flushed = within(DEADLINE + MARGIN, {
        let writer = writer.clone();
        move || writer.flush_until(Instant::now() + DEADLINE)
    });
    assert_eq!(flushed, Some(false), "a flush with room in the queue");

    // The queue is full, so the flush request itself cannot be queued.
    assert!(log_lines(&writer, "queued", CAPACITY * 2));
    let flushed = within(DEADLINE + MARGIN, {
        let writer = writer.clone();
        move || writer.flush_until(Instant::now() + DEADLINE)
    });
    assert_eq!(flushed, Some(false), "a flush behind a full queue");
    sink.open();
}

/// The writer thread's entry catches a panicking sink: that line is counted
/// lost and the next one is written.
#[test]
fn a_panicking_sink_loses_one_line_and_the_writer_continues() {
    struct PanicsOnce(bool, Arc<Mutex<Vec<u8>>>);
    impl Write for PanicsOnce {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if !std::mem::replace(&mut self.0, true) {
                // `resume_unwind` skips the panic hook: quiet test logs.
                std::panic::resume_unwind(Box::new("injected sink panic"));
            }
            self.1.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    static LOST: AtomicU64 = AtomicU64::new(0);
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = PanicsOnce(false, written.clone());
    let writer = spawn("test-log-panic", sink, CAPACITY, &LOST).unwrap();
    writer.push(b"lost\n");
    writer.push(b"kept\n");
    assert!(writer.flush_until(Instant::now() + PROMPT));
    assert_eq!(written.lock().unwrap().as_slice(), b"kept\n");
    assert_eq!(LOST.load(Ordering::Relaxed), 1);
}
