//! Consumer leases on shared stages (Q-041).
//!
//! Every consumer of a shared stage (an egress, an HLS preview, an SRT
//! player) holds a [`StageLease`] on each stage it reads. The registry entry
//! keeps only a `Weak` to the lease cell, so the stage lives exactly as long
//! as its consumers: dropping the last lease cancels the stage and removes
//! its entry. No sweep re-derives use from the outputs, and no grace period
//! covers consumers that have not announced themselves yet.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use tokio_util::sync::CancellationToken;

use crate::domain::stage::StageKey;
use crate::media::engine::MediaEngine;

/// One consumer's hold on a shared stage. Clones share the hold.
#[derive(Clone)]
pub struct StageLease(#[allow(dead_code)] Arc<LeaseCell>);

pub struct LeaseCell {
    /// Unique for the process: the registry entry records it, so a release
    /// removes only the entry this cell created (an address can be reused).
    id: u64,
    engine: Weak<MediaEngine>,
    /// Captured at creation (always on Tokio): the last lease can drop on a
    /// Compio Owner thread (an SRT player), and the release runs on Tokio.
    runtime: Option<tokio::runtime::Handle>,
    stage: LeasedStage,
    cancel: CancellationToken,
}

enum LeasedStage {
    Runtime(StageKey),
    TsMuxer(String),
}

impl StageLease {
    pub(crate) fn runtime(
        engine: &Arc<MediaEngine>,
        key: StageKey,
        cancel: CancellationToken,
    ) -> Self {
        Self::new(engine, LeasedStage::Runtime(key), cancel)
    }

    pub(crate) fn ts_muxer(
        engine: &Arc<MediaEngine>,
        key: String,
        cancel: CancellationToken,
    ) -> Self {
        Self::new(engine, LeasedStage::TsMuxer(key), cancel)
    }

    fn new(engine: &Arc<MediaEngine>, stage: LeasedStage, cancel: CancellationToken) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self(Arc::new(LeaseCell {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            engine: Arc::downgrade(engine),
            runtime: tokio::runtime::Handle::try_current().ok(),
            stage,
            cancel,
        }))
    }

    /// The registry's reference: upgrades while any consumer holds the lease.
    pub(crate) fn downgrade(&self) -> Weak<LeaseCell> {
        Arc::downgrade(&self.0)
    }

    /// The id the registry entry records for this lease (never 0).
    pub(crate) fn id(&self) -> u64 {
        self.0.id
    }

    /// Another hold on a live stage, if any consumer still holds one.
    pub(crate) fn upgrade(weak: &Weak<LeaseCell>) -> Option<Self> {
        weak.upgrade().map(Self)
    }
}

impl Drop for LeaseCell {
    fn drop(&mut self) {
        self.cancel.cancel();
        let (Some(engine), Some(runtime)) = (self.engine.upgrade(), self.runtime.as_ref()) else {
            // A cancelled entry is never reused; the next consumer replaces it.
            return;
        };
        // A newer stage under the same key records another id and is left
        // alone, even if its cell reuses this allocation.
        let id = self.id;
        let stage = match &self.stage {
            LeasedStage::Runtime(key) => LeasedStage::Runtime(key.clone()),
            LeasedStage::TsMuxer(key) => LeasedStage::TsMuxer(key.clone()),
        };
        runtime.spawn(async move {
            match stage {
                LeasedStage::Runtime(key) => engine.release_stage_runtime(&key, id).await,
                LeasedStage::TsMuxer(key) => engine.release_ts_muxer_stage(&key, id).await,
            }
        });
    }
}
