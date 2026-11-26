//! Pull task control shared with the push registry.
//!
//! The registry owns the task handles; `hls_pull` owns the fetch loop. These
//! types carry the source row and the task control surface, so both modules
//! depend on this file instead of on each other.

use std::sync::{Arc, Mutex};

use restream_domain::domain::pipeline_input::{PullAuth, VariantRule};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// The input this pull task serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullIdentity {
    pub pipeline_id: String,
    pub input_id: String,
    pub stream_key: String,
    pub selected: bool,
}

/// What to pull: the source URL, which rendition of a master playlist to
/// follow, and how to authenticate to the origin (write-only; never logged).
/// Cookies ride the client's jar and need no configuration here.
#[derive(Clone, PartialEq, Eq)]
pub struct PullSource {
    pub url: String,
    pub variant: VariantRule,
    pub auth: PullAuth,
}

/// Why a pull task stopped, once it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullEnd {
    /// The playlist reached `EXT-X-ENDLIST` and every listed segment was
    /// delivered. The source is finished: restarting would replay it.
    Ended,
    /// Every other stop: a fatal error (a bad URL, refused credentials, a
    /// master that points at another master), another publisher holding the
    /// input, a cancellation, or a run that gave up. The source may recover,
    /// so a caller may restart the task behind a backoff. A cancelled task
    /// reports this too; the registry drops a cancelled task's handle and
    /// never reads its end.
    Fatal,
}

/// A running pull. `cancel` stops it at its next await; `join` waits for it
/// to release every resource; `end` says how it stopped once it has.
pub struct HlsPullTask {
    cancel: CancellationToken,
    join: JoinHandle<()>,
    /// Why the task stopped, or `None` while it runs. The task's last act is
    /// to set it, and a caller may keep a clone ([`Self::end_state`]) so it
    /// can still tell how the task ended after dropping the handle.
    end: Arc<Mutex<Option<PullEnd>>>,
}

impl HlsPullTask {
    /// Assemble a handle around the task the puller spawned.
    pub(crate) fn new(
        cancel: CancellationToken,
        join: JoinHandle<()>,
        end: Arc<Mutex<Option<PullEnd>>>,
    ) -> Self {
        Self { cancel, join, end }
    }

    /// Stop the task. Idempotent; the session it opened ends on its own idle
    /// timeout or when the pipeline input is cancelled.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub async fn join(self) {
        match self.join.await {
            Ok(()) => {}
            Err(error) => warn!(%error, "HLS pull task did not finish cleanly"),
        }
    }

    /// Whether the task has stopped. A stopped task is still the row's
    /// running task: only a changed source replaces it, so a caller that
    /// wants a stopped task restarted drops it first.
    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }

    /// Why the task stopped, or `None` while it runs. `EXT-X-ENDLIST` is the
    /// one end a restart would replay, so it is the one a caller must not
    /// restart.
    pub fn end(&self) -> Option<PullEnd> {
        *restream_sync::lock::lock(&self.end)
    }

    /// The state [`Self::end`] reads, shared with the task.
    pub fn end_state(&self) -> Arc<Mutex<Option<PullEnd>>> {
        self.end.clone()
    }
}
