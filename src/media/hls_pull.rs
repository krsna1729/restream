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
//! URIs in a remote playlist are absolute URLs, while the assembler parses
//! upload paths inside one stream key's namespace. The puller therefore
//! rewrites each fetched media playlist into that namespace: sequence `N` is
//! `pull/sN.ts` (or `.m4s`), and each `EXT-X-MAP` URI becomes a stable
//! `pull/m<hash>.mp4`. The objects are dispatched under those same paths, so
//! the assembler sees the playlist and segments it would have seen from an
//! encoder.

use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Url};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::domain::pipeline_input::{HeaderPair, PullAuth, VariantRule};
use crate::media::engine::MediaEngine;
use crate::media::hls::upload_policy::backoff;
use crate::media::hls_ingest::playlist::{
    self, MasterPlaylist, MediaPlaylist, Playlist, SegmentEntry, Variant,
};
use crate::media::hls_ingest::{
    BodyError, DispatchError, DispatchOrigin, HlsIngest, HlsIngestLimits, ObjectBody, ObjectKind,
    ObjectOutcome,
};
use crate::media::ingest_auth::AuthenticatedPipeline;

/// The namespace the assembler sees: the media playlist's fixed path and the
/// prefix of every object the puller dispatches under it.
const PULL_DIR: &str = "pull";
const PLAYLIST_PATH: &str = "pull/index.m3u8";
/// Connect timeout for origin requests; a stalled request is bounded by
/// `REQUEST_TIMEOUT` and by task cancellation.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Attempts in one retry burst before the puller waits a quiet period. The
/// requests inside a burst follow [`backoff`].
const MAX_ATTEMPTS: u32 = 6;
/// After a burst, wait this long before starting the next one, so a wrong
/// URL or an origin that stays down is a slow poll, never a spin.
const QUIET_PERIOD: Duration = Duration::from_secs(30);
const USER_AGENT: &str = concat!("Restream/", env!("CARGO_PKG_VERSION"));

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
    /// A signed URL carries its own token in the query, and `auth` carries
    /// the rest, so neither is printed.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PullSource")
            .field("url", &crate::secret_display::redact_url(&self.url))
            .field("variant", &self.variant)
            .field("auth", &self.auth)
            .finish()
    }
}

/// A running pull. `cancel` stops it at its next await; `join` waits for it
/// to release every resource.
pub struct HlsPullTask {
    cancel: CancellationToken,
    join: JoinHandle<()>,
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
    let join = tokio::spawn(async move {
        run(engine, ingest, identity, source, limits, token).await;
    });
    HlsPullTask { cancel, join }
}

/// What one playlist cycle did.
enum Cycle {
    /// The playlist is live: refresh again after the pacing delay.
    Live { had_new: bool, target: Duration },
    /// `EXT-X-ENDLIST` and every listed segment was handed to the assembler.
    Ended,
}

/// Why a cycle did not complete. Both variants are logged once and followed
/// by a backoff; `Fatal` also ends the task.
#[derive(Debug)]
enum PullError {
    Fatal(String),
    PublisherActive,
    Retry(String),
}

impl From<FetchError> for PullError {
    fn from(error: FetchError) -> Self {
        Self::Retry(error.to_string())
    }
}

/// A pull's origin credentials, validated once. Applied to every request;
/// reqwest strips `Authorization` and `Cookie` on a cross-host redirect
/// (see `remove_sensitive_headers`), and the cookie jar scopes cookies by
/// domain, so a redirect cannot carry them to another host.
enum RequestAuth {
    None,
    Bearer(String),
    Basic { username: String, password: String },
    Headers(HeaderMap),
}

impl RequestAuth {
    fn new(auth: &PullAuth) -> Result<Self, String> {
        Ok(match auth {
            PullAuth::None => Self::None,
            PullAuth::Bearer { token } => {
                // An invalid token would only fail at send time; reject it once.
                HeaderValue::from_str(token).map_err(|_| "invalid bearer token".to_string())?;
                Self::Bearer(token.clone())
            }
            PullAuth::Basic { username, password } => Self::Basic {
                username: username.clone(),
                password: password.clone(),
            },
            PullAuth::Headers { headers } => Self::Headers(pair_headers(headers)?),
        })
    }

    fn apply(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Self::None => builder,
            Self::Bearer(token) => builder.bearer_auth(token),
            Self::Basic { username, password } => builder.basic_auth(username, Some(password)),
            Self::Headers(headers) => builder.headers(headers.clone()),
        }
    }
}

struct PullState {
    /// The playlist to fetch this cycle: the source URL until a master
    /// playlist is resolved, then the chosen media playlist.
    next_url: Url,
    variant: VariantRule,
    auth: RequestAuth,
    pipeline: AuthenticatedPipeline,
    stream_key: String,
    client_addr: String,
    /// First sequence not yet fetched, and the first sequence the previous
    /// playlist listed (to notice a restarted encoder).
    next_sequence: Option<u64>,
    last_media_sequence: Option<u64>,
    /// `EXT-X-MAP` URLs whose init this task already handed to the session.
    fetched_maps: HashSet<String>,
}

async fn run(
    engine: Arc<MediaEngine>,
    ingest: Arc<HlsIngest>,
    identity: PullIdentity,
    source: PullSource,
    limits: HlsIngestLimits,
    cancel: CancellationToken,
) {
    let mut url = match Url::parse(&source.url) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url,
        Ok(url) => {
            error!(url = %safe_url(&url), "HLS pull source must be an HTTP or HTTPS URL");
            return;
        }
        Err(error) => {
            // The URL itself may carry a token, so only the parse error is logged.
            error!(%error, "HLS pull source URL does not parse");
            return;
        }
    };
    url.set_fragment(None);
    let client_addr = url.host_str().unwrap_or("hls-origin").to_string();
    let auth = match RequestAuth::new(&source.auth) {
        Ok(auth) => auth,
        Err(error) => {
            // The error names the mechanism, never its secret.
            error!(%error, "HLS pull origin credentials are invalid");
            return;
        }
    };
    let client = build_client();
    let mut state = PullState {
        next_url: url,
        variant: source.variant,
        auth,
        pipeline: AuthenticatedPipeline {
            id: identity.pipeline_id.clone(),
            input_id: identity.input_id.clone(),
            selected: identity.selected,
        },
        stream_key: identity.stream_key,
        client_addr,
        next_sequence: None,
        last_media_sequence: None,
        fetched_maps: HashSet::new(),
    };
    info!(
        pipeline = %state.pipeline.id,
        input = %state.pipeline.input_id,
        url = %safe_url(&state.next_url),
        "HLS pull started"
    );

    let mut attempts = 0u32;
    let mut last_error: Option<String> = None;
    loop {
        let result = tokio::select! {
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
                attempts = attempts.saturating_add(1);
                let delay = if attempts > MAX_ATTEMPTS {
                    attempts = 1;
                    QUIET_PERIOD
                } else {
                    backoff(attempts)
                };
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
        let mut bytes = fetch_small(
            client,
            &self.next_url,
            &self.auth,
            limits.max_playlist_bytes,
        )
        .await?;
        let mut parsed = parse_remote(&self.next_url, &bytes)?;
        let media = match parsed {
            Playlist::Media(media) => media,
            Playlist::Master(master) => {
                let variant = choose_variant(&master, self.variant).ok_or_else(|| {
                    PullError::Fatal("HLS master playlist lists no variant".to_string())
                })?;
                let media_url = Url::parse(&variant.path).map_err(|error| {
                    PullError::Fatal(format!("HLS variant URL does not parse: {error}"))
                })?;
                bytes =
                    fetch_small(client, &media_url, &self.auth, limits.max_playlist_bytes).await?;
                parsed = parse_remote(&media_url, &bytes)?;
                match parsed {
                    Playlist::Media(media) => {
                        // Follow the chosen rendition from now on.
                        self.next_url = media_url;
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
        }
        self.last_media_sequence = Some(media.media_sequence);

        let mut next = self.next_sequence.unwrap_or(media.media_sequence);
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
        }

        let mut had_new = false;
        for segment in media
            .segments
            .iter()
            .filter(|segment| segment.sequence >= next)
        {
            self.fetch_segment(client, ingest, limits, segment).await?;
            self.next_sequence = Some(segment.sequence.saturating_add(1));
            had_new = true;
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

    async fn dispatch_playlist(
        &self,
        ingest: &Arc<HlsIngest>,
        bytes: Vec<u8>,
    ) -> Result<(), PullError> {
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let mut body = ingest
            .object_body(ObjectKind::Playlist, Some(length), &self.pipeline.input_id)
            .await
            .map_err(playlist_body_error)?;
        body.push(&bytes).await.map_err(playlist_body_error)?;
        let outcome = ingest
            .dispatch(
                &self.pipeline,
                &self.stream_key,
                &self.client_addr,
                DispatchOrigin::Pull,
                ObjectKind::Playlist,
                PLAYLIST_PATH.to_string(),
                body,
            )
            .await
            .map_err(dispatch_error)?;
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
            && !self.fetched_maps.contains(map)
        {
            self.fetch_init(client, ingest, limits, map).await?;
            self.fetched_maps.insert(map.clone());
        }
        let url = Url::parse(&segment.path).map_err(|error| {
            PullError::Retry(format!("HLS segment URL does not parse: {error}"))
        })?;
        let path = format!("{PULL_DIR}/{}", segment_name(segment.sequence, kind));
        let outcome = self
            .fetch_object(client, ingest, &url, kind.max_bytes(limits), kind, path)
            .await?;
        match outcome {
            ObjectOutcome::Stored | ObjectOutcome::Held => Ok(()),
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
                // segment and let ordering count the gap.
                warn!(
                    pipeline = %self.pipeline.id,
                    input = %self.pipeline.input_id,
                    url = %safe_url(&url),
                    %reason,
                    "HLS pull segment refused"
                );
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
                    url = %safe_url(&url),
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
        let response = send(client, url, &self.auth).await?;
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

/// The container a pulled segment carries. A playlist that names an
/// `EXT-X-MAP` carries fMP4 media; otherwise the URL's extension decides,
/// and an unknown extension is tried as MPEG-TS (the assembler refuses it
/// with a reason if that is wrong).
fn object_kind(segment: &SegmentEntry) -> ObjectKind {
    if segment.map_uri.is_some() {
        return ObjectKind::Fmp4;
    }
    let path = segment.path.split(['?', '#']).next().unwrap_or_default();
    ObjectKind::of(path).unwrap_or(ObjectKind::TsSegment)
}

fn segment_name(sequence: u64, kind: ObjectKind) -> String {
    match kind {
        ObjectKind::Fmp4 => format!("s{sequence}.m4s"),
        ObjectKind::Playlist | ObjectKind::TsSegment => format!("s{sequence}.ts"),
    }
}

/// A stable object name for one `EXT-X-MAP` URL, so the map's identity
/// survives playlist reloads without keeping per-map state.
fn map_name(url: &str) -> String {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("m{:016x}.mp4", hasher.finish())
}

/// Rewrite a remote media playlist into the push namespace: sequence `N` is
/// `sN.ts`/`sN.m4s`, the map is its [`map_name`], and every ordering-relevant
/// tag survives. URIs are relative to [`PLAYLIST_PATH`]'s directory.
fn push_playlist_text(media: &MediaPlaylist) -> String {
    let mut text = String::with_capacity(256 + media.segments.len() * 40);
    let _ = writeln!(text, "#EXTM3U");
    let _ = writeln!(
        text,
        "#EXT-X-TARGETDURATION:{}",
        media.target_duration.as_secs_f64()
    );
    let _ = writeln!(text, "#EXT-X-MEDIA-SEQUENCE:{}", media.media_sequence);
    let mut current_map: Option<String> = None;
    for segment in &media.segments {
        if segment.discontinuity {
            let _ = writeln!(text, "#EXT-X-DISCONTINUITY");
        }
        if let Some(map) = &segment.map_uri {
            let name = map_name(map);
            if current_map.as_deref() != Some(name.as_str()) {
                let _ = writeln!(text, "#EXT-X-MAP:URI=\"{name}\"");
                current_map = Some(name);
            }
        }
        let _ = writeln!(text, "#EXTINF:{},", segment.duration.as_secs_f64());
        let _ = writeln!(
            text,
            "{}",
            segment_name(segment.sequence, object_kind(segment))
        );
    }
    if media.ended {
        let _ = writeln!(text, "#EXT-X-ENDLIST");
    }
    text
}

/// Parse a playlist fetched from a remote origin: URIs resolve against the
/// playlist URL (an absolute URI is kept), and the fragment is dropped
/// because it never reaches the origin.
fn parse_remote(base: &Url, bytes: &[u8]) -> Result<Playlist, PullError> {
    playlist::parse_with(bytes, |uri| {
        let mut url = base.join(uri).ok()?;
        url.set_fragment(None);
        Some(url.to_string())
    })
    .map_err(|error| PullError::Retry(format!("HLS playlist does not parse: {error}")))
}

fn pair_headers(pairs: &[HeaderPair]) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    for pair in pairs {
        let name = HeaderName::from_bytes(pair.name.trim().as_bytes())
            .map_err(|_| format!("invalid request header name {:?}", pair.name))?;
        // The value is a credential: the error names the header, never it.
        let value = HeaderValue::from_str(&pair.value)
            .map_err(|_| format!("invalid value for request header {:?}", pair.name))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn build_client() -> Client {
    let tls = crate::media::egress::tls::resolve_client_config(None).unwrap_or_else(|error| {
        warn!(%error, "HLS pull TLS configuration unusable; using the default roots");
        crate::media::egress::tls::rustls_client_config()
    });
    Client::builder()
        .user_agent(USER_AGENT)
        // Mechanism 2 (signed cookies): a `Set-Cookie` on a playlist is
        // presented with that playlist's segments, as a browser or FFmpeg
        // `-cookies` does. Cookies are scoped by domain by the jar.
        .cookie_store(true)
        .use_preconfigured_tls((*tls).clone())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_else(|error| {
            warn!(%error, "HLS pull client build failed; using a default client");
            Client::new()
        })
}

async fn send(
    client: &Client,
    url: &Url,
    auth: &RequestAuth,
) -> Result<reqwest::Response, FetchError> {
    let response = auth
        .apply(client.get(url.clone()))
        .send()
        .await
        .map_err(|error| FetchError::Request(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Status(status.as_u16()));
    }
    Ok(response)
}

/// Read a bounded playlist body.
async fn fetch_small(
    client: &Client,
    url: &Url,
    auth: &RequestAuth,
    max_bytes: usize,
) -> Result<Vec<u8>, FetchError> {
    let response = send(client, url, auth).await?;
    let declared = response.content_length();
    if declared.is_some_and(|length| length > u64::try_from(max_bytes).unwrap_or(u64::MAX)) {
        return Err(FetchError::TooLarge);
    }
    let capacity = declared
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(max_bytes);
    let mut body = Vec::with_capacity(capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| FetchError::Body(error.to_string()))?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Stream a bounded segment body into an ingest body, as an upload handler
/// frames its request body.
async fn pump_body(
    response: reqwest::Response,
    limit: usize,
    body: &mut ObjectBody,
) -> Result<(), FetchError> {
    if let Some(length) = response.content_length()
        && length > u64::try_from(limit).unwrap_or(u64::MAX)
    {
        return Err(FetchError::TooLarge);
    }
    let mut total = 0usize;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| FetchError::Body(error.to_string()))?;
        total = total.saturating_add(chunk.len());
        if total > limit {
            return Err(FetchError::TooLarge);
        }
        body.push(&chunk).await.map_err(FetchError::BodyWrite)?;
    }
    Ok(())
}

#[derive(Debug)]
enum FetchError {
    Request(String),
    Status(u16),
    TooLarge,
    Body(String),
    BodyWrite(BodyError),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(error) => write!(formatter, "origin request failed: {error}"),
            Self::Status(status) => write!(formatter, "origin answered HTTP {status}"),
            Self::TooLarge => write!(formatter, "origin object is over its size bound"),
            Self::Body(error) => write!(formatter, "origin body read failed: {error}"),
            Self::BodyWrite(BodyError::TooLarge) => {
                write!(formatter, "origin object is over the ingest bound")
            }
            Self::BodyWrite(BodyError::BufferFull) => {
                write!(formatter, "HLS ingest buffer is full")
            }
        }
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
        DispatchError::Unavailable => {
            PullError::Retry("HLS ingest session is unavailable".to_string())
        }
    }
}

/// Refresh cadence: one target duration normally, half of it after a reload
/// that brought nothing new (RFC 8216 section 6.3.4).
fn refresh_delay(target: Duration, had_new: bool) -> Duration {
    if had_new { target } else { target / 2 }
}

async fn sleep_or_cancel(delay: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => true,
        _ = tokio::time::sleep(delay) => false,
    }
}

/// A URL safe to log: signed URLs carry their token in the query.
fn safe_url(url: &Url) -> String {
    let mut safe = url.clone();
    safe.set_query(None);
    safe.set_fragment(None);
    safe.to_string()
}

#[cfg(test)]
#[path = "hls_pull/tests.rs"]
mod tests;
