use super::*;
use proptest::prelude::*;
use std::sync::Arc;
use std::sync::Mutex;

static EXPECTED_PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>;

struct ScopedSilentPanicHook(Option<PanicHook>);

impl ScopedSilentPanicHook {
    fn new() -> Self {
        Self(Some(std::panic::take_hook()))
    }

    fn silence(&mut self) {
        std::panic::set_hook(Box::new(|_| {}));
    }
}

impl Drop for ScopedSilentPanicHook {
    fn drop(&mut self) {
        if let Some(hook) = self.0.take() {
            std::panic::set_hook(hook);
        }
    }
}

#[tokio::test]
async fn write_batch_preserves_chunk_order() {
    let queue = MemoryQueue::new();
    assert_eq!(
        queue
            .write_batch([b"abc".as_slice(), b"def".as_slice()])
            .await,
        6
    );

    let mut output = [0u8; 6];
    assert_eq!(queue.read(&mut output), output.len());
    assert_eq!(&output, b"abcdef");
}

#[test]
fn explicit_capacity_is_reported_in_stats() {
    let queue = MemoryQueue::new_with_capacity(12345);
    assert_eq!(queue.stats().capacity, 12345);
}

#[tokio::test]
async fn empty_write_batch_does_not_add_data() {
    let queue = MemoryQueue::new();
    assert_eq!(queue.write_batch(std::iter::empty()).await, 0);
    let mut output = [0u8; 1];
    assert_eq!(queue.read_nonblocking(&mut output), 0);
}

#[test]
fn custom_output_drop_does_not_double_close_avio() {
    ffmpeg::init().expect("FFmpeg init");
    let queue = MemoryQueue::new();
    let output = CustomOutput::new(&queue, "mpegts").expect("custom output");
    drop(output);
}

// --- Regression: issue #2 (Round 3) — MemoryQueue::read must not panic
// if the Mutex is poisoned by a panicking writer thread.
// Before the fix, `cvar.wait(inner).unwrap()` would propagate the poison
// and panic in the AVIO read callback, corrupting the FFmpeg output.
// After the fix the lock is recovered and reading resumes normally.
#[tokio::test]
async fn read_recovers_from_poisoned_mutex() {
    // Poison the MemoryQueue's internal mutex from a separate thread,
    // then verify that write() and read_nonblocking() do not panic.
    // We use Arc<MemoryQueue> so the poisoning thread can share the object.
    let queue = Arc::new(MemoryQueue::new());
    {
        let _panic_hook_lock = EXPECTED_PANIC_HOOK_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut panic_hook = ScopedSilentPanicHook::new();
        panic_hook.silence();
        let q = queue.clone();
        // unwrap() inside a thread that panics → the Mutex becomes poisoned
        let _ = std::thread::spawn(move || {
            let _guard = q.inner.lock().unwrap();
            panic!("deliberate poison");
        })
        .join(); // returns Err(payload) — that's expected, we just consume it
    }
    // The mutex is now poisoned. write() and read_nonblocking() must
    // recover via `unwrap_or_else(|e| e.into_inner())` and not panic.
    queue.write(b"hello").await;
    let mut buf = [0u8; 5];
    let n = queue.read_nonblocking(&mut buf);
    assert_eq!(n, 5);
    assert_eq!(&buf, b"hello");
}

#[tokio::test]
async fn write_after_close_is_noop() {
    let queue = MemoryQueue::new();
    queue.close();
    queue.write(b"should not appear").await;
    let mut buf = [0u8; 16];
    assert_eq!(queue.read_nonblocking(&mut buf), 0);
}

#[test]
fn write_sync_after_close_is_noop() {
    let queue = MemoryQueue::new();
    queue.close();
    queue.write_sync(b"should not appear");
    let mut buf = [0u8; 32];
    assert_eq!(queue.read_nonblocking(&mut buf), 0);
}

#[tokio::test]
async fn cancellable_write_succeeds_immediately_when_capacity_available() {
    let queue = MemoryQueue::new_with_capacity(64);
    let cancel = CancellationToken::new();
    assert!(queue.write_cancellable(b"hello", &cancel).await);
    let mut buf = [0u8; 5];
    assert_eq!(queue.read_nonblocking(&mut buf), 5);
    assert_eq!(&buf, b"hello");
}

#[tokio::test]
async fn zero_capacity_queue_blocks_write_forever_until_cancelled() {
    // buf.len() < capacity is `0 < 0` on an empty zero-capacity queue —
    // always false, so write()/write_cancellable() can never observe
    // room and must block until cancelled or closed rather than looping
    // forever on a false "space available" wakeup.
    let queue = Arc::new(MemoryQueue::new_with_capacity(0));
    let cancel = CancellationToken::new();
    let q = queue.clone();
    let c = cancel.clone();
    let handle = tokio::spawn(async move { q.write_cancellable(b"x", &c).await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !handle.is_finished(),
        "a zero-capacity queue must never accept a normal write"
    );

    cancel.cancel();
    assert!(
        !handle.await.unwrap(),
        "cancellation must still unblock a permanently-full write"
    );
}

#[tokio::test]
async fn write_batch_can_exceed_capacity_in_a_single_atomic_call() {
    // write_batch only checks that the buffer currently has *some* room
    // before writing the whole batch — it does not split a batch across
    // the capacity boundary. This locks in that intentional atomicity so
    // a future refactor doesn't silently start truncating or blocking
    // mid-batch instead.
    let queue = MemoryQueue::new_with_capacity(4);
    let written = queue.write_batch([b"12345678".as_slice()]).await;
    assert_eq!(written, 8);
    assert_eq!(
        queue.len(),
        8,
        "a single batch write is not capped at capacity"
    );
}

#[tokio::test]
async fn write_batch_after_close_returns_zero() {
    let queue = MemoryQueue::new();
    queue.close();
    assert_eq!(queue.write_batch([b"data" as &[u8]]).await, 0);
}

#[test]
fn read_nonblocking_empty_returns_zero() {
    let queue = MemoryQueue::new();
    let mut buf = [0u8; 16];
    assert_eq!(queue.read_nonblocking(&mut buf), 0);
}

#[test]
fn read_returns_zero_on_closed_empty() {
    let queue = MemoryQueue::new();
    queue.close();
    let mut buf = [0u8; 16];
    assert_eq!(queue.read(&mut buf), 0);
}

/// Builds a `VecDeque` that is deliberately wrapped (occupies both ends of
/// its physical allocation) so `as_slices()` returns two non-empty parts.
/// `read`/`read_nonblocking` stitch front+back manually; a bug in that
/// stitching only shows up once the ring has actually wrapped, which a
/// queue built purely through the public API isn't guaranteed to trigger.
fn wrapped_ring_of(head: &[u8], tail: &[u8]) -> VecDeque<u8> {
    let mut ring = VecDeque::with_capacity(head.len() + tail.len());
    for _ in 0..tail.len() {
        ring.push_back(0u8);
    }
    for _ in 0..tail.len() {
        ring.pop_front();
    }
    ring.extend(head.iter().copied());
    ring.extend(tail.iter().copied());
    let (front, back) = ring.as_slices();
    assert!(
        !front.is_empty() && !back.is_empty(),
        "test setup must actually wrap the ring, or the split-slice path goes unexercised"
    );
    ring
}

#[test]
fn read_stitches_front_and_back_slices_across_a_wrapped_ring() {
    let queue = MemoryQueue::new_with_capacity(64);
    {
        let mut inner = queue.inner.lock().unwrap();
        inner.buf = wrapped_ring_of(&[0u8; 4], b"abcd");
    }
    let mut out = [0u8; 8];
    let n = queue.read(&mut out);
    assert_eq!(n, 8);
    assert_eq!(&out[..4], &[0u8; 4]);
    assert_eq!(&out[4..], b"abcd");
}

#[test]
fn read_nonblocking_stitches_front_and_back_slices_across_a_wrapped_ring() {
    let queue = MemoryQueue::new_with_capacity(64);
    {
        let mut inner = queue.inner.lock().unwrap();
        inner.buf = wrapped_ring_of(&[0u8; 4], b"wxyz");
    }
    let mut out = [0u8; 8];
    let n = queue.read_nonblocking(&mut out);
    assert_eq!(n, 8);
    assert_eq!(&out[..4], &[0u8; 4]);
    assert_eq!(&out[4..], b"wxyz");
}

#[test]
fn len_and_is_empty_reflect_buffered_bytes() {
    let queue = MemoryQueue::new();
    assert!(queue.is_empty());
    assert_eq!(queue.len(), 0);
    queue.write_sync(b"hello");
    assert!(!queue.is_empty());
    assert_eq!(queue.len(), 5);
    let mut buf = [0u8; 3];
    queue.read_nonblocking(&mut buf);
    assert_eq!(queue.len(), 2);
}

#[test]
fn stats_report_depth_capacity_high_water_and_closed_state() {
    let queue = MemoryQueue::new_with_capacity(8);
    assert_eq!(
        queue.stats(),
        MemoryQueueStats {
            len: 0,
            capacity: 8,
            high_water_bytes: 0,
            blocked_writes: 0,
            blocked_write_us: 0,
            closed: false,
        }
    );

    queue.write_sync(b"hello");
    let stats = queue.stats();
    assert_eq!(stats.len, 5);
    assert_eq!(stats.capacity, 8);
    assert_eq!(stats.high_water_bytes, 5);
    assert!(!stats.closed);

    let mut buf = [0u8; 3];
    queue.read_nonblocking(&mut buf);
    let stats = queue.stats();
    assert_eq!(stats.len, 2);
    assert_eq!(stats.high_water_bytes, 5);

    queue.close();
    assert!(queue.stats().closed);
}

#[test]
fn is_closed_reflects_state() {
    let queue = MemoryQueue::new();
    assert!(!queue.is_closed());
    queue.close();
    assert!(queue.is_closed());
}

#[tokio::test]
async fn write_respects_capacity() {
    let queue = Arc::new(MemoryQueue::new_with_capacity(5));
    queue.write(b"hello").await; // 5 bytes — exactly at capacity
    let q = queue.clone();
    let handle = tokio::spawn(async move {
        q.write(b"blocked").await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !handle.is_finished(),
        "write should still be blocked at capacity"
    );
    let mut buf = [0u8; 5];
    queue.read_nonblocking(&mut buf);
    handle.await.unwrap();
}

#[tokio::test]
async fn blocked_write_updates_backpressure_stats() {
    let queue = Arc::new(MemoryQueue::new_with_capacity(5));
    queue.write(b"hello").await;
    let q = queue.clone();
    let handle = tokio::spawn(async move {
        q.write(b"blocked").await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!handle.is_finished());
    assert_eq!(queue.stats().blocked_writes, 1);

    let mut buf = [0u8; 5];
    queue.read_nonblocking(&mut buf);
    handle.await.unwrap();

    let stats = queue.stats();
    assert_eq!(stats.blocked_writes, 1);
    assert!(stats.blocked_write_us > 0);
    assert!(stats.high_water_bytes >= 7);
}

#[tokio::test]
async fn blocked_write_unblocks_when_queue_closes() {
    let queue = Arc::new(MemoryQueue::new_with_capacity(5));
    queue.write(b"hello").await;

    let writer_queue = queue.clone();
    let blocked_write = tokio::spawn(async move {
        writer_queue.write(b"blocked").await;
        writer_queue.is_closed()
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!blocked_write.is_finished());

    queue.close();

    assert!(
        blocked_write.await.unwrap(),
        "blocked writer should observe queue closure and return"
    );
}

#[tokio::test]
async fn cancellable_write_unblocks_when_token_cancels() {
    let queue = Arc::new(MemoryQueue::new_with_capacity(5));
    queue.write(b"hello").await;

    let writer_queue = queue.clone();
    let cancel = CancellationToken::new();
    let writer_cancel = cancel.clone();
    let blocked_write = tokio::spawn(async move {
        writer_queue
            .write_cancellable(b"blocked", &writer_cancel)
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!blocked_write.is_finished());

    cancel.cancel();

    assert!(
        !blocked_write.await.unwrap(),
        "blocked writer should stop once cancellation is requested"
    );
}

#[tokio::test]
async fn read_wakes_on_write() {
    let queue = Arc::new(MemoryQueue::new());
    let q = queue.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        q.write_sync(b"wakeup");
    });
    let mut buf = [0u8; 6];
    let n = queue.read(&mut buf);
    assert_eq!(n, 6);
    assert_eq!(&buf, b"wakeup");
    handle.join().unwrap();
}

// Regression: `write()` used to call `space_available.notified()` *after*
// releasing the capacity-check lock. A reader on another OS thread could
// drain the buffer and call `notify_waiters()` in that gap — before the
// writer's future existed to observe it — losing the wakeup and hanging
// the writer forever. Reproduced by benches/avio_throughput.rs, which hung
// indefinitely at 0% CPU on its first warmup iteration. This test mirrors
// that shape (multi-thread runtime writer + tight-loop reader on a real
// OS thread, capacity crossed on nearly every write) so the race window
// is exercised on essentially every iteration; a timeout catches a
// reintroduced lost wakeup instead of hanging the test suite.
#[test]
fn write_wakeup_survives_lock_release_race() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let chunk_size = 64;
    let iterations = 500;
    let queue = Arc::new(MemoryQueue::new_with_capacity(chunk_size));
    let chunk = vec![0xABu8; chunk_size];

    let w = queue.clone();
    let c = chunk.clone();
    let writer = runtime.spawn(async move {
        for _ in 0..iterations {
            w.write(&c).await;
        }
        w.close();
    });

    let reader_queue = queue.clone();
    let reader = std::thread::spawn(move || {
        let mut buf = vec![0u8; chunk_size];
        let mut total = 0usize;
        loop {
            let n = reader_queue.read(&mut buf);
            if n == 0 {
                break;
            }
            total += n;
        }
        total
    });

    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), writer)
            .await
            .expect("writer must not hang on a lost wakeup")
            .expect("writer task must not panic");
    });

    let total = reader.join().unwrap();
    assert_eq!(total, iterations * chunk_size);
}

proptest! {
    #[test]
    fn write_batch_round_trips_random_chunks(
        chunks in proptest::collection::vec(
            proptest::collection::vec(any::<u8>(), 0..64),
            0..16
        )
    ) {
        let total_bytes: usize = chunks.iter().map(Vec::len).sum();
        let queue = MemoryQueue::new_with_capacity(total_bytes.max(1));
        let expected: Vec<u8> = chunks.iter().flatten().copied().collect();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let slices: Vec<&[u8]> = chunks.iter().map(Vec::as_slice).collect();
        let written = runtime.block_on(queue.write_batch(slices.iter().copied()));

        prop_assert_eq!(written, total_bytes);

        let mut actual = vec![0u8; total_bytes];
        let read = queue.read_nonblocking(&mut actual);
        prop_assert_eq!(read, total_bytes);
        prop_assert_eq!(actual, expected);
    }
}
