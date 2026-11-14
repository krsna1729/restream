//! Recording catalog updates on their way to the writer that persists them.
//!
//! A recorder reports from its own task and never waits on the database, so
//! [`RecordingMetadataReporter::report`] only merges the update into the
//! queue: at most one entry per recording, holding its start (the row to
//! create) and its latest end. Memory is bounded by the recordings with an
//! update still unwritten, never by how far behind the writer is, and no
//! update is lost: a later one supersedes only an earlier one of the same
//! kind (a `Finalized` replaces a pending `Failed` and the reverse; a start
//! that lands beside a pending end keeps that end).
//!
//! The entry being written stays with the queue until the writer takes the
//! next one, so a writer that panics mid-write and is restarted gets it back
//! ([`RecordingMetadataQueue::restore`]), merged under anything reported
//! for the same recording since.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordingMetadataEvent {
    Started {
        recording_id: String,
        pipeline_id: String,
        started_at: String,
        temp_path: String,
    },
    Finalized {
        recording_id: String,
        ended_at: String,
        final_path: String,
    },
    Failed {
        recording_id: String,
        error: String,
    },
}

/// A recording's start: the catalog row to create.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingStarted {
    pub pipeline_id: String,
    pub started_at: String,
    pub temp_path: String,
}

/// How a recording ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordingEnd {
    Finalized {
        ended_at: String,
        final_path: String,
    },
    Failed {
        error: String,
    },
}

/// Everything still to be written for one recording: the start first, then
/// the end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingMetadataUpdate {
    pub recording_id: String,
    pub started: Option<RecordingStarted>,
    pub end: Option<RecordingEnd>,
}

impl RecordingMetadataUpdate {
    fn from_event(event: RecordingMetadataEvent) -> Self {
        match event {
            RecordingMetadataEvent::Started {
                recording_id,
                pipeline_id,
                started_at,
                temp_path,
            } => Self {
                recording_id,
                started: Some(RecordingStarted {
                    pipeline_id,
                    started_at,
                    temp_path,
                }),
                end: None,
            },
            RecordingMetadataEvent::Finalized {
                recording_id,
                ended_at,
                final_path,
            } => Self {
                recording_id,
                started: None,
                end: Some(RecordingEnd::Finalized {
                    ended_at,
                    final_path,
                }),
            },
            RecordingMetadataEvent::Failed {
                recording_id,
                error,
            } => Self {
                recording_id,
                started: None,
                end: Some(RecordingEnd::Failed { error }),
            },
        }
    }

    /// Folds `newer` into this older update: each part takes the newer
    /// value where it has one.
    fn absorb(&mut self, newer: Self) {
        if newer.started.is_some() {
            self.started = newer.started;
        }
        if newer.end.is_some() {
            self.end = newer.end;
        }
    }
}

#[derive(Default)]
struct Pending {
    /// Recording ids in the order their first unwritten update arrived;
    /// exactly the keys of `updates`.
    order: VecDeque<String>,
    updates: HashMap<String, RecordingMetadataUpdate>,
    /// The update the writer took last, until it takes the next one.
    writing: Option<RecordingMetadataUpdate>,
    /// Every reporter is gone: once drained, the writer ends.
    closed: bool,
}

#[derive(Default)]
struct Shared {
    pending: Mutex<Pending>,
    ready: Notify,
}

/// A new reporter and the queue its writer drains.
pub fn recording_metadata_channel() -> (RecordingMetadataReporter, RecordingMetadataQueue) {
    let shared = Arc::new(Shared::default());
    (
        RecordingMetadataReporter {
            handle: Arc::new(ReporterHandle {
                shared: shared.clone(),
            }),
        },
        RecordingMetadataQueue { shared },
    )
}

/// The recorders' side: reports never block and are never dropped.
#[derive(Clone)]
pub struct RecordingMetadataReporter {
    handle: Arc<ReporterHandle>,
}

/// Closes the queue when the last reporter clone goes.
struct ReporterHandle {
    shared: Arc<Shared>,
}

impl Drop for ReporterHandle {
    fn drop(&mut self) {
        restream_sync::lock::lock(&self.shared.pending).closed = true;
        self.shared.ready.notify_one();
    }
}

impl RecordingMetadataReporter {
    pub fn report(&self, event: RecordingMetadataEvent) {
        let update = RecordingMetadataUpdate::from_event(event);
        let shared = &self.handle.shared;
        {
            let mut pending = restream_sync::lock::lock(&shared.pending);
            if let Some(queued) = pending.updates.get_mut(&update.recording_id) {
                queued.absorb(update);
            } else {
                pending.order.push_back(update.recording_id.clone());
                pending.updates.insert(update.recording_id.clone(), update);
            }
        }
        shared.ready.notify_one();
    }
}

/// The writer's side. One writer drains it at a time.
#[derive(Clone)]
pub struct RecordingMetadataQueue {
    shared: Arc<Shared>,
}

impl RecordingMetadataQueue {
    /// The next recording's update, oldest first; taking it acknowledges
    /// the one taken before. `None` once every reporter is gone and nothing
    /// is left to write.
    pub async fn next(&self) -> Option<RecordingMetadataUpdate> {
        loop {
            {
                let mut pending = restream_sync::lock::lock(&self.shared.pending);
                pending.writing = None;
                while let Some(id) = pending.order.pop_front() {
                    if let Some(update) = pending.updates.remove(&id) {
                        pending.writing = Some(update.clone());
                        return Some(update);
                    }
                }
                if pending.closed {
                    return None;
                }
            }
            // A report between the check and here leaves a permit.
            self.shared.ready.notified().await;
        }
    }

    /// Puts back the update taken last and not acknowledged (its writer
    /// panicked), under whatever was reported for that recording since; it
    /// is written next unless that recording was already queued again.
    pub fn restore(&self) {
        let mut pending = restream_sync::lock::lock(&self.shared.pending);
        let Some(mut update) = pending.writing.take() else {
            return;
        };
        let id = update.recording_id.clone();
        if let Some(newer) = pending.updates.remove(&id) {
            update.absorb(newer);
            pending.updates.insert(id, update);
        } else {
            pending.order.push_front(id.clone());
            pending.updates.insert(id, update);
        }
    }

    /// Recordings with an update waiting (the one being written excluded).
    pub fn pending_len(&self) -> usize {
        restream_sync::lock::lock(&self.shared.pending).order.len()
    }
}

#[cfg(test)]
mod tests;
