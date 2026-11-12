//! Ingest admission against an in-memory index of the catalog.
//!
//! Every connect path (RTMP publish and play, SRT publish and read, HLS push,
//! MoQ publish) authenticates against an immutable [`CatalogAuthSnapshot`]
//! held in an `ArcSwap`, never against SQLite, so a stalled or failing
//! database cannot stop an input from connecting.
//!
//! The index changes only through a reload. Each catalog write that can
//! change admission (pipeline create, update and delete; input create,
//! update, delete and promote) runs through [`CatalogAuth::commit`]: the
//! write and the reload after it are one task its caller awaits but cannot
//! cancel, so an API client that rotates or deletes a key sees it enforced on
//! the next connect, and a dropped request still lands its committed write.
//! A reload rebuilds the whole index and swaps it in. Reloads run one at a
//! time; one that an already-started reload covers returns without loading
//! again. A failed reload keeps the previous index, is logged as
//! `restream.catalog_auth.reload_failed`, and leaves one background task
//! retrying with bounded backoff until a reload succeeds. A periodic reload
//! ([`CatalogAuth::reload_periodically`], every
//! [`PERIODIC_RELOAD_INTERVAL`] in production) bounds staleness from any
//! change no write reported.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use restream_domain::domain::pipeline_input::{PipelineInput, PipelineInputSourceKind, PushAuth};
use restream_engine::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessFuture,
    PipelineAccessMode,
};
use restream_engine::security::{IngestSecurityService, RateLimitScope};
use tracing::{info, warn};

use crate::application::pipeline_inputs::PipelineInputStoreError;
use crate::application::ports::PipelineStoreError;

const RETRY_INITIAL_DELAY: Duration = Duration::from_millis(100);
const RETRY_MAX_DELAY: Duration = Duration::from_secs(5);
/// How often production reloads the index whether or not a write asked.
pub const PERIODIC_RELOAD_INTERVAL: Duration = Duration::from_secs(30);

/// A catalog write task that ended without an answer (it panicked).
#[derive(Debug)]
pub struct WriteAborted(String);

impl std::fmt::Display for WriteAborted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<WriteAborted> for PipelineStoreError {
    fn from(error: WriteAborted) -> Self {
        Self::new(error.0)
    }
}

impl From<WriteAborted> for PipelineInputStoreError {
    fn from(error: WriteAborted) -> Self {
        Self::Internal(error.0)
    }
}

/// A stored input whose row does not decode (for example a malformed push
/// credential). Its key stays known, so it is refused without counting a
/// failure against the client: one broken row must not get the client's
/// address banned from its healthy keys.
#[derive(Debug)]
pub struct UnreadableInput {
    pub input_id: String,
    pub stream_key: String,
    pub enabled: bool,
    pub error: String,
}

/// One stored input, decoded or not.
pub type CatalogRow = Result<PipelineInput, UnreadableInput>;

pub type CatalogAuthLoadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<CatalogRow>, PipelineStoreError>> + Send + 'a>>;

/// Reads every pipeline input, for a full rebuild of the index.
pub trait CatalogAuthSource: Send + Sync {
    fn load_inputs(&self) -> CatalogAuthLoadFuture<'_>;
}

/// What admission needs to know about one enabled input's stream key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamKeyGrant {
    pub pipeline_id: String,
    pub input_id: String,
    pub selected: bool,
    /// Only a keyed input accepts a push. A pull or caller input has no
    /// listener of its own, so its key resolves for reads only.
    pub accepts_push: bool,
    /// The HLS push credential, checked on every pushed object.
    pub push_auth: PushAuth,
}

#[derive(Debug)]
enum Admission {
    Grant(StreamKeyGrant),
    /// An enabled input whose row does not decode.
    Unreadable,
}

/// The stream keys of every enabled input. A disabled input's key is absent,
/// exactly as if it did not exist.
#[derive(Debug, Default)]
pub struct CatalogAuthSnapshot {
    keys: HashMap<String, Admission>,
}

impl CatalogAuthSnapshot {
    fn from_rows(rows: Vec<CatalogRow>) -> Self {
        let mut keys = HashMap::with_capacity(rows.len());
        for row in rows {
            match row {
                Ok(input) if input.enabled => {
                    let grant = StreamKeyGrant {
                        pipeline_id: input.pipeline_id,
                        input_id: input.id,
                        selected: input.selected,
                        accepts_push: input.source_kind == PipelineInputSourceKind::Keyed,
                        push_auth: input.push_auth,
                    };
                    keys.insert(input.stream_key, Admission::Grant(grant));
                }
                Err(unreadable) if unreadable.enabled => {
                    warn!(
                        input_id = %unreadable.input_id,
                        error = %unreadable.error,
                        "pipeline input row does not decode; its key is refused until the row is fixed",
                    );
                    keys.insert(unreadable.stream_key, Admission::Unreadable);
                }
                Ok(_) | Err(_) => {}
            }
        }
        Self { keys }
    }

    /// The grant for an enabled input's key; `None` for an unknown key and for
    /// an input whose row does not decode.
    pub fn get(&self, stream_key: &str) -> Option<&StreamKeyGrant> {
        match self.keys.get(stream_key) {
            Some(Admission::Grant(grant)) => Some(grant),
            Some(Admission::Unreadable) | None => None,
        }
    }
}

/// The process's one admission index and its reload discipline.
pub struct CatalogAuth {
    snapshot: ArcSwap<CatalogAuthSnapshot>,
    source: Arc<dyn CatalogAuthSource>,
    /// Held across a reload: one reload in flight at a time.
    reloads: tokio::sync::Mutex<()>,
    /// Reloads asked for so far, one per committed write (and the startup load).
    requested: AtomicU64,
    /// The `requested` count read before the newest successful load started:
    /// every write counted up to it is in the current snapshot.
    covered: AtomicU64,
    /// A retry task is running.
    retrying: AtomicBool,
    /// Ends the retry task and the periodic reload at shutdown.
    shutdown: tokio_util::sync::CancellationToken,
}

impl CatalogAuth {
    /// An index with no keys, refusing every connect until [`Self::load`]
    /// or a reload fills it.
    pub fn new(source: Arc<dyn CatalogAuthSource>) -> Arc<Self> {
        Arc::new(Self {
            snapshot: ArcSwap::from_pointee(CatalogAuthSnapshot::default()),
            source,
            reloads: tokio::sync::Mutex::new(()),
            requested: AtomicU64::new(0),
            covered: AtomicU64::new(0),
            retrying: AtomicBool::new(false),
            shutdown: tokio_util::sync::CancellationToken::new(),
        })
    }

    /// Stops the periodic reload and any retry task. A write still reloads.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// The startup load, before any ingest listener starts. Its error fails
    /// startup rather than leaving an index that refuses every key.
    pub async fn load(&self) -> Result<(), PipelineStoreError> {
        let ticket = self.request();
        self.reload(ticket).await
    }

    pub fn snapshot(&self) -> Arc<CatalogAuthSnapshot> {
        self.snapshot.load_full()
    }

    /// Runs a catalog write and then a reload as one task, and returns the
    /// write's answer once the reload is swapped in (or has failed and the
    /// retry task owns it). The caller only awaits the task: dropping it (a
    /// disconnected HTTP client) cancels neither half, so a write SQLite
    /// committed always reaches the index.
    pub async fn commit<T, E>(
        self: &Arc<Self>,
        write: impl Future<Output = Result<T, E>> + Send + 'static,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<WriteAborted> + Send + 'static,
    {
        let catalog = Arc::clone(self);
        let unit = tokio::spawn(async move {
            let answer = write.await;
            // Reloaded whatever the store answered: an error after the commit
            // (a lost reply) must not leave the write unenforced.
            catalog.reload_or_retry().await;
            answer
        });
        unit.await.unwrap_or_else(|join| {
            Err(WriteAborted(format!("catalog write task failed: {join}")).into())
        })
    }

    /// A reload with no write in front of it, on the same uncancellable path.
    #[cfg(test)]
    pub(crate) async fn refresh(self: &Arc<Self>) {
        let _ = self
            .commit(std::future::ready(Ok::<(), WriteAborted>(())))
            .await;
    }

    /// Reloads every `interval` until [`Self::shutdown`]. A change no write
    /// reported (a row edited outside the services, a write task that
    /// panicked) is enforced within one interval plus a reload. Takes a ticket
    /// like any refresh, so it never runs beside another reload.
    pub async fn reload_periodically(self: Arc<Self>, interval: Duration) {
        loop {
            tokio::select! {
                () = self.shutdown.cancelled() => return,
                () = tokio::time::sleep(interval) => {}
            }
            tokio::select! {
                () = self.shutdown.cancelled() => return,
                () = self.reload_or_retry() => {}
            }
        }
    }

    async fn reload_or_retry(self: &Arc<Self>) {
        let ticket = self.request();
        if let Err(error) = self.reload(ticket).await {
            warn!(
                event_class = "lifecycle",
                event_type = "restream.catalog_auth.reload_failed",
                %error,
                "the ingest admission index could not reload; the previous index \
                 stays in force until a retry reloads it",
            );
            self.spawn_retry();
        }
    }

    fn request(&self) -> u64 {
        self.requested.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Makes the snapshot cover request `ticket`. `target` is read under the
    /// lock and before the query, so the load starts after every write counted
    /// up to it committed; a later ticket that `covered` already reaches needs
    /// no load of its own.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "`reloads` serializes reloads across the inputs query by design; only the reload path takes it, connects read the snapshot"
    )]
    async fn reload(&self, ticket: u64) -> Result<(), PipelineStoreError> {
        let _one_at_a_time = self.reloads.lock().await;
        if self.covered.load(Ordering::SeqCst) >= ticket {
            return Ok(());
        }
        let target = self.requested.load(Ordering::SeqCst);
        let rows = self.source.load_inputs().await?;
        self.snapshot
            .store(Arc::new(CatalogAuthSnapshot::from_rows(rows)));
        self.covered.store(target, Ordering::SeqCst);
        Ok(())
    }

    fn spawn_retry(self: &Arc<Self>) {
        if self.retrying.swap(true, Ordering::SeqCst) {
            return;
        }
        let catalog = Arc::clone(self);
        tokio::spawn(async move { catalog.retry_until_covered().await });
    }

    async fn retry_until_covered(&self) {
        let mut delay = RETRY_INITIAL_DELAY;
        loop {
            tokio::select! {
                () = self.shutdown.cancelled() => return,
                () = tokio::time::sleep(delay) => {}
            }
            let ticket = self.requested.load(Ordering::SeqCst);
            let reloaded = tokio::select! {
                () = self.shutdown.cancelled() => return,
                reloaded = self.reload(ticket) => reloaded,
            };
            match reloaded {
                Ok(()) => {
                    info!(
                        event_class = "lifecycle",
                        event_type = "restream.catalog_auth.reload_recovered",
                        "the ingest admission index reloaded after a failed reload",
                    );
                    self.retrying.store(false, Ordering::SeqCst);
                    // A refresh that failed while this reload ran found
                    // `retrying` set and left its write to this task.
                    if self.covered.load(Ordering::SeqCst) >= self.requested.load(Ordering::SeqCst)
                        || self.retrying.swap(true, Ordering::SeqCst)
                    {
                        return;
                    }
                    delay = RETRY_INITIAL_DELAY;
                }
                Err(error) => {
                    delay = (delay * 2).min(RETRY_MAX_DELAY);
                    warn!(
                        event_class = "lifecycle",
                        event_type = "restream.catalog_auth.reload_failed",
                        %error,
                        retry_in_ms = delay.as_millis() as u64,
                        "ingest admission index reload retry failed",
                    );
                }
            }
        }
    }
}

/// [`PipelineAccessAuthenticator`] over the [`CatalogAuth`] index: a map
/// read plus the per-client failure bookkeeping, with nothing awaited.
pub struct CatalogIngestAuthenticator {
    catalog: Arc<CatalogAuth>,
    security: Arc<IngestSecurityService>,
}

impl CatalogIngestAuthenticator {
    pub fn new(catalog: Arc<CatalogAuth>, security: Arc<IngestSecurityService>) -> Self {
        Self { catalog, security }
    }

    fn admit(
        &self,
        mode: PipelineAccessMode,
        stream_key: &str,
        client_ip: &str,
    ) -> Result<AuthenticatedPipeline, PipelineAccessError> {
        // The rate-limit scope, and whether an accepted key clears the
        // client's earlier failures. An RTMP play is not rate limited; an
        // accepted RTMP publish keeps the client's failure count.
        let limit = match mode {
            PipelineAccessMode::RtmpPublish => Some((RateLimitScope::RtmpPublish, false)),
            PipelineAccessMode::RtmpPlay => None,
            PipelineAccessMode::SrtPublish => Some((RateLimitScope::SrtPublish, true)),
            PipelineAccessMode::SrtRead => Some((RateLimitScope::SrtRead, true)),
            PipelineAccessMode::HlsPublish => Some((RateLimitScope::HlsPublish, true)),
            PipelineAccessMode::MoqPublish => Some((RateLimitScope::MoqPublish, true)),
        };
        let snapshot = self.catalog.snapshot.load();
        let grant = match snapshot.keys.get(stream_key) {
            Some(Admission::Grant(grant)) => grant,
            // The key is right; the stored input is broken. Not the client's
            // failure, so nothing is counted against its address.
            Some(Admission::Unreadable) => return Err(PipelineAccessError::InputUnreadable),
            None => {
                if let Some((scope, _)) = limit {
                    self.security.record_failure_for(scope, client_ip);
                }
                return Err(PipelineAccessError::InvalidStreamKey);
            }
        };
        if let Some((scope, true)) = limit {
            self.security.record_success_for(scope, client_ip);
        }

        // A pull input never accepts a push: publishing to it would race the
        // pull task for the input's session.
        //
        // Exhaustive on purpose: a new publish mode must decide this question
        // at compile time rather than inherit "not a publish" from a wildcard.
        let pushing = match mode {
            PipelineAccessMode::RtmpPublish
            | PipelineAccessMode::SrtPublish
            | PipelineAccessMode::HlsPublish
            | PipelineAccessMode::MoqPublish => true,
            PipelineAccessMode::RtmpPlay | PipelineAccessMode::SrtRead => false,
        };
        if pushing && !grant.accepts_push {
            return Err(PipelineAccessError::InvalidStreamKey);
        }
        Ok(AuthenticatedPipeline {
            id: grant.pipeline_id.clone(),
            input_id: grant.input_id.clone(),
            selected: grant.selected,
        })
    }
}

impl PipelineAccessAuthenticator for CatalogIngestAuthenticator {
    fn authenticate<'a>(
        &'a self,
        mode: PipelineAccessMode,
        stream_key: &'a str,
        client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(std::future::ready(self.admit(mode, stream_key, client_ip)))
    }
}

#[cfg(test)]
mod staleness_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod write_visibility_tests;
