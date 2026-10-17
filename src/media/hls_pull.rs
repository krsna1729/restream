//! HLS pull ingest: the engine fetches a remote live HLS origin and feeds
//! what it fetches into the same session assembler as HLS push ingest.
//!
//! One task follows one input. It refreshes the media playlist every target
//! duration (half after a reload that brought nothing new, RFC 8216 section
//! 6.3.4), fetches new segments in order with Reqwest, and hands each object
//! to [`HlsIngest::object_body`] + [`HlsIngest::dispatch`] exactly as the
//! upload handlers do. Ordering, discontinuity handling, timestamps and
//! pacing stay in that path; this module only decides what to fetch and
//! bounds every read.
//!
//! Credentials belong to the source's origin and go nowhere else: a playlist
//! URI on another host is fetched without them, and a redirect out of the
//! origin — or an `https`→`http` downgrade with credentials configured — is
//! not followed at all. The first live cycle enters three target durations
//! from the end of a window (RFC 8216 section 6.3.3), so a DVR or EVENT
//! window is neither read from its start nor paced a window behind live.
//!
//! URIs in a remote playlist are absolute URLs, while the assembler parses
//! upload paths inside one stream key's namespace. The puller therefore
//! rewrites each fetched media playlist into that namespace: sequence `N` is
//! `pull/sN.ts` (or `.m4s`), and each `EXT-X-MAP` URI becomes a stable
//! `pull/m<hash>.mp4`. The objects are dispatched under those same paths, so
//! the assembler sees the playlist and segments it would have seen from an
//! encoder.
//!
//! Audio. One task follows one rendition, so a master that carries audio as a
//! separate `EXT-X-MEDIA` rendition — packaged live HLS does, and so does
//! Restream's own preview — is pulled as its video variant. That is a known
//! gap, and the pull states it once per pull ([`AudioPlan`]) rather than
//! dropping the audio silently.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::{Client, Url};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use self::fetch::{
    AuthScope, FetchError, RequestAuth, build_client, fetch_small, host_refused, log_url,
    pump_body, send, variant_expired,
};
use self::namespace::{
    encryption_refusal, live_start, map_name, object_kind, parse_remote, push_playlist_text,
    refuse_private_targets, refuse_unfetchable, segment_name,
};
use crate::domain::pipeline_input::{PullAuth, VariantRule};
use crate::media::engine::MediaEngine;
use crate::media::hls::upload_policy::backoff;
use crate::media::hls_ingest::playlist::{MasterPlaylist, Playlist, SegmentEntry, Variant};
use crate::media::hls_ingest::{
    BodyError, DispatchError, DispatchOrigin, HlsIngest, HlsIngestLimits, ObjectBody, ObjectKind,
    ObjectOutcome,
};
use crate::media::ingest_auth::AuthenticatedPipeline;

mod audio;
mod fetch;
mod namespace;
use audio::report_audio_rendition;
// Re-exported for the tests in `hls_pull/tests.rs`, which exercise the audio
// plan through this module.
#[cfg(test)]
use audio::{AudioPlan, audio_plan};

/// The namespace the assembler sees: the media playlist's fixed path and the
/// prefix of every object the puller dispatches under it.
const PULL_DIR: &str = "pull";
const PLAYLIST_PATH: &str = "pull/index.m3u8";
/// Attempts in one retry burst before the puller waits a quiet period. The
/// requests inside a burst follow [`backoff`].
const MAX_ATTEMPTS: u32 = 6;
/// After a burst, wait this long before starting the next one, so a wrong
/// URL or an origin that stays down is a slow poll, never a spin.
const QUIET_PERIOD: Duration = Duration::from_secs(30);
/// The shortest refresh delay a playlist can ask for. A hostile or broken
/// origin can offer any non-zero fractional `EXT-X-TARGETDURATION`, and the
/// pull sleeps one target duration (or half of it): without a floor,
/// `TARGETDURATION:0.001` would re-poll, re-parse and re-dispatch as fast as
/// round-trip time allows.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_millis(500);
/// Attempts one segment gets before it is skipped. A 404 on one object would
/// otherwise fail every cycle until it leaves the window, which is as long as
/// the window is; the gap a skipped segment leaves is the assembler's to
/// account for, as it is for a push upload that never arrives.
const SEGMENT_ATTEMPTS: u32 = 4;

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

impl std::fmt::Debug for PullSource {
    /// A signed URL carries its own credential in the query, and `auth`
    /// carries the rest, so neither is printed: the URL is rendered with its
    /// query dropped (see [`log_url`]).
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PullSource")
            .field("url", &log_url(&self.url))
            .field("variant", &self.variant)
            .field("auth", &self.auth)
            .finish()
    }
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
        *crate::sync::lock(&self.end)
    }

    /// The state [`Self::end`] reads, shared with the task.
    pub fn end_state(&self) -> Arc<Mutex<Option<PullEnd>>> {
        self.end.clone()
    }
}

/// Start pulling `source` for `identity`. The task runs on Tokio: the
/// requests are async and the assembler does its own MEDIA-executor work.
pub fn spawn(
    engine: Arc<MediaEngine>,
    ingest: Arc<HlsIngest>,
    identity: PullIdentity,
    source: PullSource,
    limits: HlsIngestLimits,
) -> HlsPullTask {
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let end = Arc::new(Mutex::new(None));
    let task_end = end.clone();
    let join = tokio::spawn(async move {
        let ended = run(engine, ingest, identity, source, limits, token).await;
        *crate::sync::lock(&task_end) = Some(ended);
    });
    HlsPullTask { cancel, join, end }
}

/// What one playlist cycle did.
enum Cycle {
    /// The playlist is live: refresh again after the pacing delay.
    Live { had_new: bool, target: Duration },
    /// `EXT-X-ENDLIST` and every listed segment was handed to the assembler.
    Ended,
}

/// Why a cycle did not complete. `Fatal` and `PublisherActive` are logged
/// once and end the task, `Retry` is logged once and followed by a backoff,
/// and `Cancelled` stops the task without a line: the caller cancelling it is
/// what happened.
#[derive(Debug)]
enum PullError {
    Fatal(String),
    PublisherActive,
    Retry(String),
    Cancelled,
}

impl From<FetchError> for PullError {
    fn from(error: FetchError) -> Self {
        Self::Retry(error.to_string())
    }
}

struct PullState {
    /// The URL the operator configured: where a master is resolved from, and
    /// where an expired variant URL goes back to.
    source_url: Url,
    /// The playlist to fetch this cycle: the source URL until a master
    /// playlist is resolved, then the chosen media playlist.
    next_url: Url,
    /// The source's own host is in one of the refused address ranges, which
    /// is what lets its playlist point there too.
    allow_private: bool,
    scope: AuthScope,
    cancel: CancellationToken,
    variant: VariantRule,
    pipeline: AuthenticatedPipeline,
    stream_key: String,
    client_addr: String,
    /// First sequence not yet fetched, and the first sequence the previous
    /// playlist listed (to notice a restarted encoder).
    next_sequence: Option<u64>,
    last_media_sequence: Option<u64>,
    /// The map identity in force: the object name of the `EXT-X-MAP` whose
    /// init this task already handed to the session. Only the current one is
    /// kept, because a map *change* is what the assembler must see; the
    /// history of maps is worth nothing and would grow one `String` per
    /// playlist reload.
    current_map: Option<String>,
    /// Failed attempts per sequence not yet skipped, so one persistently
    /// missing object cannot stall the pull until it leaves the window.
    segment_tries: HashMap<u64, u32>,
    /// The last refusal reason logged, so a playlist whose every segment is
    /// refused logs one line rather than one per segment.
    last_refused: Option<String>,
    /// The pull is following a variant URL that has expired: the line saying
    /// the master is resolved again is logged once per expiry, not once per
    /// burst. Cleared when a variant playlist is fetched again.
    re_expired: bool,
}

async fn run(
    engine: Arc<MediaEngine>,
    ingest: Arc<HlsIngest>,
    identity: PullIdentity,
    source: PullSource,
    limits: HlsIngestLimits,
    cancel: CancellationToken,
) -> PullEnd {
    let mut url = match Url::parse(&source.url) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url,
        Ok(url) => {
            error!(
                url = %log_url(url.as_str()),
                "HLS pull source must be an HTTP or HTTPS URL"
            );
            return PullEnd::Fatal;
        }
        Err(error) => {
            // The URL itself may carry a token, so only the parse error is logged.
            error!(%error, "HLS pull source URL does not parse");
            return PullEnd::Fatal;
        }
    };
    url.set_fragment(None);
    if !url.username().is_empty() || url.password().is_some() {
        // Userinfo in the URL is a password a log line would render in part,
        // and a URL is not the place for this credential: it is configuration
        // that gets copied around and shown back.
        error!(
            url = %log_url(url.as_str()),
            "HLS pull source URL carries userinfo; use the basic credential choice instead"
        );
        return PullEnd::Fatal;
    }
    let client_addr = url.host_str().unwrap_or("hls-origin").to_string();
    let auth = match RequestAuth::new(&source.auth) {
        Ok(auth) => auth,
        Err(error) => {
            // The error names the mechanism, never its secret.
            error!(%error, "HLS pull origin credentials are invalid");
            return PullEnd::Fatal;
        }
    };
    let scope = AuthScope::new(&url, auth);
    // A source in one of the refused ranges says the operator meant an
    // internal origin, so its playlist may point there too; anything else may
    // not.
    let allow_private = host_refused(&url);
    // One client — and one cookie jar — per pull task: an origin's
    // `Set-Cookie` is scoped to this task's requests, never another input's.
    let client = build_client(scope.is_configured(), allow_private);
    let mut state = PullState {
        source_url: url.clone(),
        next_url: url,
        allow_private,
        scope,
        cancel: cancel.clone(),
        variant: source.variant,
        pipeline: AuthenticatedPipeline {
            id: identity.pipeline_id.clone(),
            input_id: identity.input_id.clone(),
            selected: identity.selected,
        },
        stream_key: identity.stream_key,
        client_addr,
        next_sequence: None,
        last_media_sequence: None,
        current_map: None,
        segment_tries: HashMap::new(),
        last_refused: None,
        re_expired: false,
    };
    info!(
        pipeline = %state.pipeline.id,
        input = %state.pipeline.input_id,
        url = %log_url(state.next_url.as_str()),
        "HLS pull started"
    );

    let mut attempts = 0u32;
    let mut last_error: Option<String> = None;
    let mut end = PullEnd::Fatal;
    loop {
        let result = tokio::select! {
            // Cancellation first, and biased: a task woken with both branches
            // ready stops instead of racing its next request, so a caller that
            // cancels and then joins (to hand the input to another source)
            // knows nothing is still in flight.
            biased;
            _ = cancel.cancelled() => break,
            result = state.pull_once(&client, &ingest, &limits) => result,
        };
        match result {
            Ok(Cycle::Ended) => {
                info!(
                    pipeline = %state.pipeline.id,
                    input = %state.pipeline.input_id,
                    "HLS pull reached EXT-X-ENDLIST"
                );
                end = PullEnd::Ended;
                break;
            }
            Ok(Cycle::Live { had_new, target }) => {
                attempts = 0;
                if last_error.take().is_some() {
                    info!(
                        pipeline = %state.pipeline.id,
                        input = %state.pipeline.input_id,
                        "HLS pull recovered"
                    );
                }
                if sleep_or_cancel(refresh_delay(target, had_new), &cancel).await {
                    break;
                }
            }
            Err(PullError::PublisherActive) => {
                error!(
                    pipeline = %state.pipeline.id,
                    input = %state.pipeline.input_id,
                    "HLS pull stopped: another publisher holds the input"
                );
                break;
            }
            Err(PullError::Fatal(error)) => {
                error!(
                    pipeline = %state.pipeline.id,
                    input = %state.pipeline.input_id,
                    %error,
                    "HLS pull stopped"
                );
                if engine
                    .input_has_session(&state.pipeline.id, &state.pipeline.input_id)
                    .await
                {
                    engine.cancel_pipeline_input(&state.pipeline.input_id).await;
                }
                break;
            }
            Err(PullError::Cancelled) => break,
            Err(PullError::Retry(error)) => {
                if last_error.as_deref() != Some(error.as_str()) {
                    warn!(
                        pipeline = %state.pipeline.id,
                        input = %state.pipeline.input_id,
                        %error,
                        "HLS pull failed; retrying"
                    );
                    last_error = Some(error);
                }
                let delay = retry_delay(&mut attempts);
                if delay == QUIET_PERIOD {
                    // A burst failed: the URL the pull is following is gone or
                    // the edge serving it is, so go back to the source and
                    // resolve the master again, which is where a fresh signed
                    // per-variant URL comes from. A source that never resolved
                    // a variant has nowhere to go back to, and `re_resolve`
                    // says nothing in that case.
                    state.re_resolve("the retry burst failed six times");
                }
                if sleep_or_cancel(delay, &cancel).await {
                    break;
                }
            }
        }
    }
    info!(
        pipeline = %state.pipeline.id,
        input = %state.pipeline.input_id,
        "HLS pull stopped"
    );
    end
}

impl PullState {
    /// Fetch the current playlist, choose a rendition if needed, apply the
    /// media playlist, then fetch and hand over its new segments in order.
    async fn pull_once(
        &mut self,
        client: &Client,
        ingest: &Arc<HlsIngest>,
        limits: &HlsIngestLimits,
    ) -> Result<Cycle, PullError> {
        let mut bytes = match fetch_small(
            client,
            &self.next_url,
            &self.scope,
            limits.max_playlist_bytes,
        )
        .await
        {
            Ok(bytes) => bytes,
            // A per-variant URL that a signed master handed out expires, and
            // the session key behind it rotates; the master is the one URL
            // that is renewed, so a media playlist that is gone sends the
            // pull back to it instead of retrying a dead URL forever.
            Err(error) if self.next_url != self.source_url && variant_expired(&error) => {
                self.re_resolve("the variant playlist answered 401, 403 or 404");
                return Err(PullError::Retry(error.to_string()));
            }
            Err(error) => return Err(error.into()),
        };
        let mut parsed = parse_remote(&self.next_url, &bytes)?;
        refuse_private_targets(&parsed, self.allow_private)?;
        let media = match parsed {
            Playlist::Media(media) => media,
            Playlist::Master(master) => {
                if let Some(encryption) = &master.session_key {
                    return Err(encryption_refusal(encryption));
                }
                let variant = choose_variant(&master, self.variant).ok_or_else(|| {
                    PullError::Fatal("HLS master playlist lists no variant".to_string())
                })?;
                report_audio_rendition(&master, variant, &self.pipeline);
                let media_url = Url::parse(&variant.path).map_err(|error| {
                    PullError::Fatal(format!("HLS variant URL does not parse: {error}"))
                })?;
                bytes =
                    fetch_small(client, &media_url, &self.scope, limits.max_playlist_bytes).await?;
                parsed = parse_remote(&media_url, &bytes)?;
                refuse_private_targets(&parsed, self.allow_private)?;
                match parsed {
                    Playlist::Media(media) => {
                        // Follow the chosen rendition from now on, and forget
                        // an earlier expiry: this URL works.
                        self.next_url = media_url;
                        self.re_expired = false;
                        media
                    }
                    Playlist::Master(_) => {
                        return Err(PullError::Fatal(
                            "HLS master playlist points at another master playlist".to_string(),
                        ));
                    }
                }
            }
        };
        refuse_unfetchable(&media)?;

        let text = push_playlist_text(&media);
        self.dispatch_playlist(ingest, text.into_bytes()).await?;

        if media.media_sequence < self.last_media_sequence.unwrap_or(media.media_sequence) {
            // The origin restarted its numbering; follow the new run.
            debug!(
                pipeline = %self.pipeline.id,
                input = %self.pipeline.input_id,
                sequence = media.media_sequence,
                "HLS pull media sequence restarted"
            );
            self.next_sequence = Some(media.media_sequence);
            self.segment_tries.clear();
        }
        self.last_media_sequence = Some(media.media_sequence);

        // The first cycle of a live playlist starts near the end: RFC 8216
        // section 6.3.3 has a client enter no less than three target
        // durations from it, so a DVR or EVENT window is not read from its
        // start and paced in real time — a whole window behind live, with the
        // backfill loaded onto the origin.
        let mut next = self.next_sequence.unwrap_or_else(|| {
            if media.ended {
                media.media_sequence
            } else {
                live_start(&media)
            }
        });
        let first_new = media
            .segments
            .iter()
            .find(|segment| segment.sequence >= next);
        if let Some(first) = first_new
            && first.sequence > next
        {
            // The window slid past sequences this task never saw.
            debug!(
                pipeline = %self.pipeline.id,
                input = %self.pipeline.input_id,
                skipped = first.sequence - next,
                "HLS pull playlist moved past segments"
            );
            next = first.sequence;
            self.next_sequence = Some(next);
            self.segment_tries.retain(|sequence, _| *sequence >= next);
        }

        let mut had_new = false;
        for segment in media
            .segments
            .iter()
            .filter(|segment| segment.sequence >= next)
        {
            match self.fetch_segment(client, ingest, limits, segment).await {
                Ok(()) => {
                    self.segment_tries.remove(&segment.sequence);
                    self.next_sequence = Some(segment.sequence.saturating_add(1));
                    had_new = true;
                }
                // One object that will not fetch must not stall the pull
                // until it leaves the window: after a few tries it is
                // skipped, and the gap it leaves is the assembler's to
                // account for, as a push upload that never arrives is.
                Err(PullError::Retry(reason)) => {
                    let tries = {
                        let tries = self.segment_tries.entry(segment.sequence).or_insert(0);
                        *tries = tries.saturating_add(1);
                        *tries
                    };
                    if tries < SEGMENT_ATTEMPTS {
                        return Err(PullError::Retry(reason));
                    }
                    warn!(
                        pipeline = %self.pipeline.id,
                        input = %self.pipeline.input_id,
                        sequence = segment.sequence,
                        url = %log_url(&segment.path),
                        tries,
                        %reason,
                        "HLS pull skipped a segment that would not fetch"
                    );
                    self.segment_tries.remove(&segment.sequence);
                    self.next_sequence = Some(segment.sequence.saturating_add(1));
                }
                Err(error) => return Err(error),
            }
        }

        if media.ended {
            Ok(Cycle::Ended)
        } else {
            Ok(Cycle::Live {
                had_new,
                target: media.target_duration,
            })
        }
    }

    /// Go back to the source URL, so the master is resolved again and a fresh
    /// per-variant URL comes with it. `reason` says why the URL the pull was
    /// following is gone. Says so once per expiry: a master that keeps handing
    /// out the same dead URL is a slow poll, not a line every burst.
    fn re_resolve(&mut self, reason: &'static str) {
        if self.next_url == self.source_url {
            return;
        }
        if !self.re_expired {
            self.re_expired = true;
            warn!(
                pipeline = %self.pipeline.id,
                input = %self.pipeline.input_id,
                url = %log_url(self.next_url.as_str()),
                %reason,
                "HLS pull resolves the master playlist again"
            );
        }
        self.next_url = self.source_url.clone();
    }

    /// Whether this pull may still hand an object to the ingest. A cancelled
    /// task may not: the session it was writing to is being torn down for the
    /// next source, and dispatching would open a fresh one under the input
    /// that source is about to own.
    fn may_dispatch(&self) -> Result<(), PullError> {
        if self.cancel.is_cancelled() {
            return Err(PullError::Cancelled);
        }
        Ok(())
    }

    async fn dispatch_playlist(
        &self,
        ingest: &Arc<HlsIngest>,
        bytes: Vec<u8>,
    ) -> Result<(), PullError> {
        self.may_dispatch()?;
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let mut body = ingest
            .object_body(ObjectKind::Playlist, Some(length), &self.pipeline.input_id)
            .await
            .map_err(playlist_body_error)?;
        body.push(&bytes).await.map_err(playlist_body_error)?;
        self.may_dispatch()?;
        let outcome = self
            .dispatch(
                ingest,
                ObjectKind::Playlist,
                PLAYLIST_PATH.to_string(),
                body,
            )
            .await?;
        match outcome {
            ObjectOutcome::Stored => Ok(()),
            ObjectOutcome::Retry => Err(PullError::Retry(
                "HLS ingest ended under the pulled playlist".to_string(),
            )),
            other => Err(PullError::Fatal(format!(
                "pulled playlist did not apply: {other:?}"
            ))),
        }
    }

    async fn fetch_segment(
        &mut self,
        client: &Client,
        ingest: &Arc<HlsIngest>,
        limits: &HlsIngestLimits,
        segment: &SegmentEntry,
    ) -> Result<(), PullError> {
        let kind = object_kind(segment);
        if kind == ObjectKind::Fmp4
            && let Some(map) = &segment.map_uri
        {
            // Only a *change* of map identity needs the init sent again: a
            // signed origin re-signs the same `EXT-X-MAP` URI on every
            // reload, and the query is the credential, not the identity.
            let name = map_name(map);
            if self.current_map.as_deref() != Some(name.as_str()) {
                self.fetch_init(client, ingest, limits, map).await?;
                self.current_map = Some(name);
            }
        }
        let url = Url::parse(&segment.path).map_err(|error| {
            PullError::Retry(format!("HLS segment URL does not parse: {error}"))
        })?;
        let path = format!("{PULL_DIR}/{}", segment_name(segment.sequence, kind));
        let outcome = self
            .fetch_object(client, ingest, &url, kind.max_bytes(limits), kind, path)
            .await?;
        match outcome {
            ObjectOutcome::Stored | ObjectOutcome::Held => {
                // A refusal said its reason; a delivered segment means the
                // reason is old news, so the next one logs again.
                self.last_refused = None;
                Ok(())
            }
            ObjectOutcome::Dropped => {
                debug!(
                    pipeline = %self.pipeline.id,
                    input = %self.pipeline.input_id,
                    "HLS pull segment was already taken"
                );
                Ok(())
            }
            ObjectOutcome::Retry => Err(PullError::Retry(
                "HLS ingest ended under the pulled segment".to_string(),
            )),
            ObjectOutcome::Malformed(reason)
            | ObjectOutcome::Unsupported(reason)
            | ObjectOutcome::TooLarge(reason) => {
                // The same bytes cannot become valid on a retry; skip the
                // segment and let ordering count the gap. One reason repeats
                // for every segment of a playlist the assembler will not take
                // (a ladder whose top variant is HEVC, say), so it is logged
                // once, as the retry reason is; a segment that is delivered
                // clears it, so a later refusal logs again.
                if self.last_refused.as_deref() != Some(reason.as_str()) {
                    warn!(
                        pipeline = %self.pipeline.id,
                        input = %self.pipeline.input_id,
                        url = %log_url(url.as_str()),
                        %reason,
                        "HLS pull segment refused"
                    );
                    self.last_refused = Some(reason);
                }
                Ok(())
            }
        }
    }

    async fn fetch_init(
        &mut self,
        client: &Client,
        ingest: &Arc<HlsIngest>,
        limits: &HlsIngestLimits,
        map: &str,
    ) -> Result<(), PullError> {
        let url = Url::parse(map)
            .map_err(|error| PullError::Retry(format!("HLS map URL does not parse: {error}")))?;
        let path = format!("{PULL_DIR}/{}", map_name(map));
        let outcome = self
            .fetch_object(
                client,
                ingest,
                &url,
                limits.max_init_bytes,
                ObjectKind::Fmp4,
                path,
            )
            .await?;
        match outcome {
            ObjectOutcome::Stored | ObjectOutcome::Held | ObjectOutcome::Dropped => Ok(()),
            ObjectOutcome::Retry => Err(PullError::Retry(
                "HLS ingest ended under the pulled init segment".to_string(),
            )),
            ObjectOutcome::Malformed(reason)
            | ObjectOutcome::Unsupported(reason)
            | ObjectOutcome::TooLarge(reason) => {
                warn!(
                    pipeline = %self.pipeline.id,
                    input = %self.pipeline.input_id,
                    url = %log_url(url.as_str()),
                    %reason,
                    "HLS pull init segment refused"
                );
                Ok(())
            }
        }
    }

    /// Fetch one object into an ingest body, bounded by `limit`, and hand it
    /// to the shared assembler. The body claims its `Content-Length` from the
    /// ingest budget before the first byte is read, as an upload does.
    async fn fetch_object(
        &self,
        client: &Client,
        ingest: &Arc<HlsIngest>,
        url: &Url,
        limit: usize,
        kind: ObjectKind,
        path: String,
    ) -> Result<ObjectOutcome, PullError> {
        self.may_dispatch()?;
        let response = send(client, url, &self.scope).await?;
        if let Some(length) = response.content_length()
            && length > u64::try_from(limit).unwrap_or(u64::MAX)
        {
            return Err(FetchError::TooLarge.into());
        }
        let mut body = ingest
            .object_body(kind, response.content_length(), &self.pipeline.input_id)
            .await
            .map_err(ingest_body_error)?;
        pump_body(response, limit, &mut body).await?;
        // The bytes are in hand; a cancellation that arrived while they were
        // read must not reach `dispatch`, whose `session_for` would open a
        // session under an input another source is about to own.
        self.may_dispatch()?;
        self.dispatch(ingest, kind, path, body).await
    }

    /// Hand one object to the ingest.
    ///
    /// Inline, not on a task: the registry's own guard releases a registration
    /// whose `session_for` future is dropped, so a cancellation that lands while
    /// this is in flight cannot leave an attempt with no owner. Detaching here
    /// would instead let the dispatch finish after the caller's teardown and
    /// open a session for an input the next source is about to own.
    async fn dispatch(
        &self,
        ingest: &Arc<HlsIngest>,
        kind: ObjectKind,
        path: String,
        body: ObjectBody,
    ) -> Result<ObjectOutcome, PullError> {
        ingest
            .dispatch(
                &self.pipeline,
                &self.stream_key,
                &self.client_addr,
                DispatchOrigin::Pull,
                kind,
                path,
                body,
            )
            .await
            .map_err(dispatch_error)
    }
}

/// Choose the rendition a pull follows.
fn choose_variant(master: &MasterPlaylist, rule: VariantRule) -> Option<&Variant> {
    match rule {
        VariantRule::HighestBandwidth => master.highest_bandwidth(),
        VariantRule::LowestBandwidth => master.lowest_bandwidth(),
    }
}

fn ingest_body_error(error: BodyError) -> PullError {
    PullError::Retry(FetchError::BodyWrite(error).to_string())
}

/// A generated playlist that outgrew the bound cannot shrink on a retry.
fn playlist_body_error(error: BodyError) -> PullError {
    match error {
        BodyError::TooLarge => {
            PullError::Fatal("pulled playlist exceeds max_playlist_bytes".to_string())
        }
        BodyError::BufferFull => PullError::Retry("HLS ingest buffer is full".to_string()),
    }
}

fn dispatch_error(error: DispatchError) -> PullError {
    match error {
        DispatchError::PublisherActive => PullError::PublisherActive,
        // The row changed under a pull of our own making: the pull is replaced
        // on the next pass, so this is not a retry of the same source.
        DispatchError::AuthorizationChanged => {
            PullError::Fatal("the pull input no longer accepts its own objects".to_string())
        }
        DispatchError::Unavailable => {
            PullError::Retry("HLS ingest session is unavailable".to_string())
        }
    }
}

/// Refresh cadence: one target duration normally, half of it after a reload
/// that brought nothing new (RFC 8216 section 6.3.4), and never shorter than
/// [`MIN_REFRESH_INTERVAL`].
fn refresh_delay(target: Duration, had_new: bool) -> Duration {
    let delay = if had_new { target } else { target / 2 };
    delay.max(MIN_REFRESH_INTERVAL)
}

/// The delay before the cycle after the `attempts`-th consecutive failure,
/// and the count the next burst starts from: [`MAX_ATTEMPTS`] failures make a
/// burst, then the pull waits [`QUIET_PERIOD`] before starting the next one,
/// whether that is the first burst or the tenth. [`backoff`] paces the
/// requests inside a burst.
fn retry_delay(attempts: &mut u32) -> Duration {
    *attempts = attempts.saturating_add(1);
    if *attempts >= MAX_ATTEMPTS {
        *attempts = 0;
        QUIET_PERIOD
    } else {
        backoff(*attempts)
    }
}

async fn sleep_or_cancel(delay: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => true,
        _ = tokio::time::sleep(delay) => false,
    }
}

#[cfg(test)]
#[path = "hls_pull/tests.rs"]
mod tests;
// The integration halves: `tests_origin` owns the origin stub and the helpers
// both integration modules use, `tests_guards` the guards a real origin can
// prove. Split because one file of them would break the 999-line cap.
#[cfg(test)]
#[path = "hls_pull/tests_guards.rs"]
mod tests_guards;
#[cfg(test)]
#[path = "hls_pull/tests_origin.rs"]
mod tests_origin;
