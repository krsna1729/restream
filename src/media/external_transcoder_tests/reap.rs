/// A cancelled stage whose child has stopped reading stdin must not wait out
/// the 30 s end-of-input grace: the stdin writer is blocked in `write_all`
/// on a full pipe, so only killing the child releases it. The reap kills
/// after the short cancel grace and joins the writer.
#[tokio::test]
async fn cancelled_reap_releases_a_writer_blocked_on_a_stalled_child() {
    use std::io::Write;

    let mut child = tokio::process::Command::new("sleep")
        .arg("60")
        .stdin(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn a child that never reads stdin");
    let stdin = child.stdin.take().expect("piped stdin");
    let mut pipe =
        ffmpeg_process::blocking_pipe(stdin.into_owned_fd().expect("stdin fd")).expect("blocking");
    let (blocked_tx, blocked_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let chunk = vec![0u8; 64 * 1024];
        let _ = blocked_tx.send(());
        // Fills the pipe, then blocks until the child is gone (EPIPE).
        while pipe.write_all(&chunk).is_ok() {}
    });
    blocked_rx.recv().expect("writer started");
    std::thread::sleep(std::time::Duration::from_millis(50));

    let started = std::time::Instant::now();
    reap_external_child(&mut child, true, vec![writer]).await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < ffmpeg_process::EXTERNAL_CANCEL_GRACE + std::time::Duration::from_secs(3),
        "cancelled reap took {elapsed:?}"
    );
    assert!(child.try_wait().expect("child status").is_some());
}
