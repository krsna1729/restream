//! Non-blocking line writers for the console and file log sinks.
//!
//! A thread that logs never waits for a log collector. Each formatted line
//! goes into a bounded queue that one writer thread per sink drains; a full
//! queue drops the line and counts it. A stalled stdout pipe, slow terminal,
//! journald backpressure or full disk stalls only that sink's writer thread,
//! never an Owner thread, the SRT loop, the reconciler or a Tokio worker.
//! Shutdown flushes under a deadline, so a stalled collector cannot hang exit.

use std::cell::Cell;
use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use tracing_subscriber::fmt::MakeWriter;

/// How often a flush retries a full queue before its deadline.
const FLUSH_RETRY: Duration = Duration::from_millis(5);

enum Msg {
    Line(Vec<u8>),
    Flush(SyncSender<()>),
}

thread_local! {
    static ON_WRITER_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// True on a log writer thread. The panic hook reports nothing there: a sink
/// that panics on every write would otherwise feed its own queue forever.
pub(super) fn on_writer_thread() -> bool {
    ON_WRITER_THREAD.with(Cell::get)
}

/// The logging side of one sink: hands each line to the writer thread
/// without waiting, counting every line it cannot hand over.
#[derive(Clone)]
pub(super) struct NonBlocking {
    tx: SyncSender<Msg>,
    lost: &'static AtomicU64,
}

impl NonBlocking {
    pub(super) fn push(&self, line: &[u8]) {
        if self.tx.try_send(Msg::Line(line.to_vec())).is_err() {
            self.lost.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Never reports an error: `tracing-subscriber` prints a failed write to
/// stderr synchronously, which is the wait this writer exists to remove.
impl Write for &NonBlocking {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.push(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for NonBlocking {
    type Writer = &'a NonBlocking;

    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

/// Flushes one sink's queue on demand; held by `LoggingHandles`.
pub(super) struct FlushGuard {
    tx: SyncSender<Msg>,
}

impl FlushGuard {
    /// Waits until the writer has written every line queued before this call,
    /// or until `deadline`. Returns whether the flush completed.
    pub(super) fn flush_until(&self, deadline: Instant) -> bool {
        let (done_tx, done_rx) = sync_channel(1);
        let mut msg = Msg::Flush(done_tx);
        loop {
            match self.tx.try_send(msg) {
                Ok(()) => break,
                Err(TrySendError::Full(back)) if Instant::now() < deadline => {
                    msg = back;
                    std::thread::sleep(FLUSH_RETRY);
                }
                Err(_) => return false,
            }
        }
        done_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
    }
}

/// Starts the writer thread for `sink` with a queue of `capacity` lines.
/// `lost` counts lines dropped on a full queue or lost to a failed write.
pub(super) fn spawn<W: Write + Send + 'static>(
    name: &str,
    sink: W,
    capacity: usize,
    lost: &'static AtomicU64,
) -> io::Result<(NonBlocking, FlushGuard)> {
    let (tx, rx) = sync_channel(capacity);
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || run_writer(rx, sink, lost))?;
    Ok((
        NonBlocking {
            tx: tx.clone(),
            lost,
        },
        FlushGuard { tx },
    ))
}

/// Runs until every sender is gone. A panicking sink loses the line it was
/// writing; the loop resumes with the next one.
fn run_writer<W: Write>(rx: Receiver<Msg>, mut sink: W, lost: &AtomicU64) {
    ON_WRITER_THREAD.with(|flag| flag.set(true));
    while catch_unwind(AssertUnwindSafe(|| drain(&rx, &mut sink, lost))).is_err() {
        lost.fetch_add(1, Ordering::Relaxed);
    }
}

fn drain<W: Write>(rx: &Receiver<Msg>, sink: &mut W, lost: &AtomicU64) {
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Line(line) => {
                if sink.write_all(&line).is_err() {
                    lost.fetch_add(1, Ordering::Relaxed);
                }
            }
            Msg::Flush(done) => {
                let _ = sink.flush();
                let _ = done.try_send(());
            }
        }
    }
}

#[cfg(test)]
mod tests;
