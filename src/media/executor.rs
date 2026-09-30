//! Fixed process-wide execution class for continuous container work.
//!
//! Tokio supplies timers, ring/queue waits and child-pipe readiness here, but
//! these workers never share the application/control scheduler. A service owns
//! its ring cursor and reusable packaging state; there is no per-packet handoff
//! and no thread per feed. Services must check cancellation and yield between
//! bounded bursts. Blocking codecs and disk writes remain on their own threads.

use std::sync::OnceLock;

use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::error;

fn runtime() -> Result<&'static Runtime, String> {
    static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            Builder::new_multi_thread()
                .worker_threads(crate::system_sampling::effective_cpu_count().clamp(1, 4))
                .thread_name("restream-media")
                .enable_all()
                .build()
                .map_err(|error| format!("media executor initialization failed: {error}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

struct CancelOnDrop(Option<CancellationToken>);

impl CancelOnDrop {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel.cancel();
            if std::thread::panicking() {
                error!("media service panicked; cancelling its lifecycle");
            }
        }
    }
}

/// Start an owned media service. Tokio contains task panics; the guard also
/// cancels its lifecycle on panic or abort, even before its first poll.
/// Successful completion does not cancel: file-input EOF may start a new pass.
pub(crate) fn spawn<F>(
    cancel: CancellationToken,
    future: F,
) -> Result<JoinHandle<F::Output>, String>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut guard = CancelOnDrop(Some(cancel));
    let runtime = runtime().inspect_err(|error| error!(%error))?;
    Ok(runtime.spawn(async move {
        let output = future.await;
        guard.disarm();
        output
    }))
}

/// Await a media service from the control runtime. Dropping the caller requests
/// graceful cancellation, not task abortion: the media service still flushes
/// final segments, closes queues and removes its stage. The service must bound
/// every work turn and honor `cancel` at waits and burst boundaries.
pub(crate) async fn run<F>(cancel: CancellationToken, future: F) -> Result<F::Output, String>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut guard = CancelOnDrop(Some(cancel.clone()));
    let task = spawn(cancel, future)?;
    match task.await {
        Ok(output) => {
            guard.disarm();
            Ok(output)
        }
        Err(error) => {
            error!(%error, "media service failed");
            Err(error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test(flavor = "current_thread")]
    async fn media_service_progresses_while_control_thread_is_blocked() {
        let ring = std::sync::Arc::new(crate::media::ring_buffer::RingBuffer::new(8));
        let mut reader =
            crate::media::ring_buffer::Reader::new("media-isolation".into(), ring.clone());
        let cancel = CancellationToken::new();
        let (done, observed) = std::sync::mpsc::channel();
        let task = spawn(cancel.clone(), async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            ring.push(crate::media::packet::MediaPacket {
                media_type: crate::media::packet::MediaType::Video,
                format: crate::media::packet::PayloadFormat::Raw,
                is_keyframe: true,
                track_index: 0,
                pts: 33,
                dts: 33,
                payload: bytes::Bytes::from_static(b"owned-packet"),
            });
            done.send(()).unwrap();
        })
        .unwrap();
        // Deliberately prevent the current-thread control runtime from polling.
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut packets = Vec::new();
        assert_eq!(reader.pull_burst(&mut packets, 8).unwrap(), 1);
        assert_eq!(packets[0].payload.as_ref(), b"owned-packet");
        assert_eq!((packets[0].pts, packets[0].dts), (33, 33));
        task.await.unwrap();
        assert!(
            !cancel.is_cancelled(),
            "successful pass must permit file-loop restart"
        );
    }

    #[tokio::test]
    async fn dropping_control_owner_allows_media_cleanup_to_finish() {
        let cancel = CancellationToken::new();
        let (ready, started) = oneshot::channel();
        let (done, finished) = oneshot::channel();
        let inner_cancel = cancel.clone();
        let owner_cancel = cancel.clone();
        let owner = tokio::spawn(async move {
            run(owner_cancel, async move {
                ready.send(()).unwrap();
                inner_cancel.cancelled().await;
                // Finalization is asynchronous; aborting the inner task would
                // drop this sender and fail the observable cleanup contract.
                tokio::task::yield_now().await;
                done.send(()).unwrap();
            })
            .await
        });
        started.await.unwrap();
        owner.abort();
        assert!(owner.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), finished)
            .await
            .unwrap()
            .unwrap();
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn aborting_media_service_cancels_its_lifecycle() {
        let cancel = CancellationToken::new();
        let (ready, started) = oneshot::channel();
        let task = spawn(cancel.clone(), async move {
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        })
        .unwrap();
        started.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(cancel.is_cancelled());
    }
}
