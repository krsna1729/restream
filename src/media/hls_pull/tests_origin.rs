//! HLS pull ingest against a real origin: one tiny HTTP listener per
//! behaviour that needs one, and the helpers only these tests use. The
//! fetch plans and the pure guards are in `tests.rs`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;

use super::fetch::{
    AuthScope, FetchError, RequestAuth, build_client, fetch_small, pump_body, send,
};
use super::*;
use crate::domain::pipeline_input::{PullAuth, VariantRule};
use crate::media::hls_ingest::test_support::{INPUT_ID, PIPELINE_ID, STREAM_KEY, test_ingest};
use crate::media::packet::MediaType;
use tracing_subscriber::layer::SubscriberExt as _;

/// A tiny origin: exact paths on `/live`, recording every request's path (with
/// its query), credentials and cookies. `/stall.m3u8` never answers;
/// `/chunked.m3u8` streams without a `Content-Length`; a registered redirect
/// answers 302.
#[derive(Clone)]
pub(super) struct Origin {
    routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Routes that match the path *and* the query, for a signed URL whose
    /// signature the test changes.
    exact_routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    redirects: Arc<Mutex<HashMap<String, String>>>,
    requests: Arc<Mutex<Vec<String>>>,
    cookies: Arc<Mutex<Vec<String>>>,
    pub(super) authorization: Arc<Mutex<Vec<String>>>,
    pub(super) api_keys: Arc<Mutex<Vec<String>>>,
    set_cookie: Arc<Mutex<Option<String>>>,
}

impl Origin {
    pub(super) fn new() -> Self {
        Self {
            routes: Arc::new(Mutex::new(HashMap::new())),
            exact_routes: Arc::new(Mutex::new(HashMap::new())),
            redirects: Arc::new(Mutex::new(HashMap::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            cookies: Arc::new(Mutex::new(Vec::new())),
            authorization: Arc::new(Mutex::new(Vec::new())),
            api_keys: Arc::new(Mutex::new(Vec::new())),
            set_cookie: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) fn serve(&self, path: &str, body: impl Into<Vec<u8>>) {
        crate::sync::lock(&self.routes).insert(path.to_string(), body.into());
    }

    /// Serve `path_and_query` exactly, taking precedence over [`Self::serve`]:
    /// `?sig=1` and `?sig=2` can answer differently.
    pub(super) fn serve_exact(&self, path_and_query: &str, body: impl Into<Vec<u8>>) {
        crate::sync::lock(&self.exact_routes).insert(path_and_query.to_string(), body.into());
    }

    pub(super) fn unserve_exact(&self, path_and_query: &str) {
        crate::sync::lock(&self.exact_routes).remove(path_and_query);
    }

    /// Answer 302 with `location` for `path`.
    pub(super) fn redirect(&self, path: &str, location: &str) {
        crate::sync::lock(&self.redirects).insert(path.to_string(), location.to_string());
    }

    pub(super) fn count(&self, path: &str) -> usize {
        crate::sync::lock(&self.requests)
            .iter()
            .filter(|request| request.contains(path))
            .count()
    }

    fn set_cookie(&self, value: &str) {
        *crate::sync::lock(&self.set_cookie) = Some(value.to_string());
    }

    pub(super) async fn spawn(&self) -> String {
        let app = Router::new().fallback(get(origin)).with_state(self.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }
}

async fn origin(
    State(origin): State<Origin>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    let path = uri.path().to_string();
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| path.clone());
    crate::sync::lock(&origin.requests).push(path_and_query.clone());
    if let Some(cookie) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    {
        crate::sync::lock(&origin.cookies).push(cookie.to_string());
    }
    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        crate::sync::lock(&origin.authorization).push(auth.to_string());
    }
    if let Some(key) = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
    {
        crate::sync::lock(&origin.api_keys).push(key.to_string());
    }
    if let Some(location) = crate::sync::lock(&origin.redirects).get(&path).cloned() {
        let mut response = StatusCode::FOUND.into_response();
        response.headers_mut().insert(
            header::LOCATION,
            HeaderValue::from_str(&location).expect("test location is a header value"),
        );
        return response;
    }
    if path == "/stall.m3u8" {
        tokio::time::sleep(Duration::from_secs(3_600)).await;
    }
    if path == "/chunked.m3u8" {
        let chunks =
            futures_util::stream::iter([Ok::<_, Infallible>(Bytes::from(vec![b'#'; 4_096]))]);
        let mut response = Response::new(Body::from_stream(chunks));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.apple.mpegurl"),
        );
        return response;
    }
    let body = crate::sync::lock(&origin.exact_routes)
        .get(&path_and_query)
        .cloned()
        .or_else(|| crate::sync::lock(&origin.routes).get(&path).cloned());
    match body {
        Some(body) => {
            let content_type = if path.ends_with(".m3u8") {
                "application/vnd.apple.mpegurl"
            } else {
                "video/mp2t"
            };
            let mut response = body.into_response();
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            if path.ends_with(".m3u8")
                && let Some(cookie) = crate::sync::lock(&origin.set_cookie).clone()
            {
                response
                    .headers_mut()
                    .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
            }
            response
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub(super) fn limits() -> HlsIngestLimits {
    HlsIngestLimits {
        max_playlist_bytes: 8 * 1024,
        max_segment_bytes: 256 * 1024,
        max_init_bytes: 64 * 1024,
        total_buffer_bytes: 8 * 1024 * 1024,
        key_buffer_bytes: 4 * 1024 * 1024,
        hold_bytes: 2 * 1024 * 1024,
        idle_floor: Duration::from_millis(500),
        ..HlsIngestLimits::default()
    }
}

pub(super) fn identity() -> PullIdentity {
    PullIdentity {
        pipeline_id: PIPELINE_ID.to_string(),
        input_id: INPUT_ID.to_string(),
        stream_key: STREAM_KEY.to_string(),
        selected: true,
    }
}

pub(super) fn source(url: String, variant: VariantRule) -> PullSource {
    PullSource {
        url,
        variant,
        auth: PullAuth::None,
    }
}

/// A master whose audio is a separate `EXT-X-MEDIA` rendition, as packaged
/// live HLS (and Restream's own preview) writes it. Used by the audio plan's
/// tests in `tests.rs` and by the pull of a master here.
pub(super) const AUDIO_MASTER: &str = "#EXTM3U\n\
    #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Track 1 (en)\",DEFAULT=YES,AUTOSELECT=YES,\
    CHANNELS=\"2\",URI=\"audio/0/index.m3u8\"\n\
    #EXT-X-STREAM-INF:BANDWIDTH=2000000,AUDIO=\"audio\"\nhigh.m3u8\n\
    #EXT-X-STREAM-INF:BANDWIDTH=500000\nlow.m3u8\n";

/// ~64 KiB of the canonical H.264 MPEG-TS fixture: a whole number of
/// transport packets from an origin that starts with a keyframe.
pub(super) fn ts_segment() -> Vec<u8> {
    let path = crate::test_fixtures::canonical_h264_ts_fixture().unwrap();
    let bytes = std::fs::read(path).unwrap();
    let wanted = (64 * 1024).min(bytes.len());
    bytes[..wanted - wanted % 188].to_vec()
}

pub(super) async fn wait_for_request(origin: &Origin, path: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while origin.count(path) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("origin never saw {path}"));
}

pub(super) async fn wait_for_media(
    engine: &MediaEngine,
    start: usize,
) -> Vec<Arc<crate::media::packet::MediaPacket>> {
    let ring = engine.get_or_create_pipeline(PIPELINE_ID).await;
    tokio::time::timeout(Duration::from_secs(15), async {
        while ring.get_write_idx() == start {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("pulled media reached the ring");
    (start..ring.get_write_idx())
        .filter_map(|index| ring.read_at(index))
        .collect()
}

/// Every event emitted while this layer is the thread's default subscriber,
/// rendered as `field=value` pairs, so a test can assert the pull's one audio
/// line, the text it carries, and that no line carries a credential.
#[derive(Clone, Default)]
pub(super) struct CapturedMessages(Arc<Mutex<Vec<String>>>);

impl CapturedMessages {
    pub(super) fn matching(&self, needle: &str) -> Vec<String> {
        crate::sync::lock(&self.0)
            .iter()
            .filter(|message| message.contains(needle))
            .cloned()
            .collect()
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CapturedMessages {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.0.push_str(&format!(" {}={value}", field.name()));
            }

            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push_str(&format!(" {}={value:?}", field.name()));
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        crate::sync::lock(&self.0).push(fields.0);
    }
}

#[tokio::test]
async fn a_cookie_string_is_sent_as_the_one_cookie_header() {
    let origin = Origin::new();
    origin.serve("/live/index.m3u8", media_playlist(&["seg0.ts"]));
    origin.serve("/live/seg0.ts", ts_segment());
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
    let mut pull_source = source(
        format!("{base}/live/index.m3u8"),
        VariantRule::HighestBandwidth,
    );
    pull_source.auth = PullAuth::Cookies {
        cookies: "CloudFront-Policy=abc; CloudFront-Signature=def".to_string(),
    };
    let task = spawn(engine.clone(), ingest, identity(), pull_source, limits());

    wait_for_request(&origin, "/live/seg0.ts").await;
    let cookies = crate::sync::lock(&origin.cookies).clone();
    assert!(
        cookies
            .iter()
            .all(|cookie| cookie == "CloudFront-Policy=abc; CloudFront-Signature=def"),
        "every request carries the whole cookie string: {cookies:?}"
    );
    assert!(
        cookies.len() >= 2,
        "the playlist and segment requests carry it"
    );

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

// ---- bounded reads --------------------------------------------------------

#[tokio::test]
async fn an_oversize_playlist_is_refused() {
    let origin = Origin::new();
    origin.serve("/live/huge.m3u8", "#EXTM3U\n".repeat(512));
    let base = origin.spawn().await;
    let client = build_client(false);
    let url = Url::parse(&format!("{base}/live/huge.m3u8")).unwrap();
    let error = fetch_small(&client, &url, &AuthScope::new(&url, RequestAuth::None), 64)
        .await
        .unwrap_err();
    assert!(matches!(&error, FetchError::TooLarge), "{error:?}");
}

#[tokio::test]
async fn an_oversize_segment_is_refused_declared_or_streamed() {
    let origin = Origin::new();
    origin.serve("/live/huge.ts", vec![0x47u8; 4_096]);
    let base = origin.spawn().await;
    let client = build_client(false);
    let (_engine, ingest) = test_ingest(limits());

    let declared = Url::parse(&format!("{base}/live/huge.ts")).unwrap();
    let response = send(
        &client,
        &declared,
        &AuthScope::new(&declared, RequestAuth::None),
    )
    .await
    .unwrap();
    let mut body = ingest
        .object_body(ObjectKind::TsSegment, response.content_length(), INPUT_ID)
        .await
        .unwrap();
    let error = pump_body(response, 128, &mut body).await.unwrap_err();
    assert!(matches!(&error, FetchError::TooLarge), "{error:?}");

    // A body with no Content-Length is bounded while it streams.
    let chunked = Url::parse(&format!("{base}/chunked.m3u8")).unwrap();
    let response = send(
        &client,
        &chunked,
        &AuthScope::new(&chunked, RequestAuth::None),
    )
    .await
    .unwrap();
    assert_eq!(response.content_length(), None);
    let mut body = ingest
        .object_body(ObjectKind::Playlist, None, INPUT_ID)
        .await
        .unwrap();
    let error = pump_body(response, 128, &mut body).await.unwrap_err();
    assert!(matches!(&error, FetchError::TooLarge), "{error:?}");
}

// ---- full pulls against a real origin ------------------------------------

const MASTER: &str = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=2000000\nhigh.m3u8\n\
                      #EXT-X-STREAM-INF:BANDWIDTH=500000\nlow.m3u8\n";

pub(super) fn media_playlist(segments: &[&str]) -> String {
    let mut text = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n");
    for segment in segments {
        text.push_str(&format!("#EXTINF:0.5,\n{segment}\n"));
    }
    text
}

#[tokio::test]
async fn pull_reaches_the_assembler_and_never_refetches_a_segment() {
    let origin = Origin::new();
    origin.serve("/live/master.m3u8", MASTER);
    origin.serve("/live/low.m3u8", media_playlist(&["low0.ts"]));
    origin.serve("/live/high.m3u8", media_playlist(&["seg0.ts", "seg1.ts"]));
    origin.serve("/live/seg0.ts", ts_segment());
    origin.serve("/live/seg1.ts", ts_segment());
    origin.serve("/live/seg2.ts", ts_segment());
    origin.serve("/live/low0.ts", ts_segment());
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
    let ring = engine.get_or_create_pipeline(PIPELINE_ID).await;
    let start = ring.get_write_idx();
    let task = spawn(
        engine.clone(),
        ingest,
        identity(),
        source(
            format!("{base}/live/master.m3u8"),
            VariantRule::HighestBandwidth,
        ),
        limits(),
    );

    wait_for_request(&origin, "/live/seg0.ts").await;
    wait_for_request(&origin, "/live/seg1.ts").await;
    // A later reload lists the next segment; the first two stay taken.
    origin.serve(
        "/live/high.m3u8",
        media_playlist(&["seg0.ts", "seg1.ts", "seg2.ts"]),
    );
    wait_for_request(&origin, "/live/seg2.ts").await;

    let packets = wait_for_media(&engine, start).await;
    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "pulled MPEG-TS reached the ring"
    );
    assert_eq!(
        engine
            .active_ingest_protocol_for_probe(PIPELINE_ID)
            .await
            .as_deref(),
        Some("hls-pull"),
        "the session is registered with the pull protocol label"
    );
    assert_eq!(
        origin.count("/live/seg0.ts"),
        1,
        "a taken segment is not refetched"
    );
    assert_eq!(origin.count("/live/seg1.ts"), 1);
    assert_eq!(origin.count("/live/seg2.ts"), 1);
    assert_eq!(
        origin.count("/live/low.m3u8"),
        0,
        "the highest variant is followed"
    );

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

#[tokio::test]
async fn a_master_with_a_separate_audio_rendition_is_pulled_video_only_and_says_so() {
    let origin = Origin::new();
    origin.serve("/live/master.m3u8", AUDIO_MASTER);
    origin.serve("/live/high.m3u8", media_playlist(&["seg0.ts"]));
    origin.serve("/live/seg0.ts", ts_segment());
    origin.serve("/live/audio/0/index.m3u8", media_playlist(&["a0.ts"]));
    origin.serve("/live/audio/0/a0.ts", ts_segment());
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
    let ring = engine.get_or_create_pipeline(PIPELINE_ID).await;
    let start = ring.get_write_idx();
    // The pull task runs on this test's runtime thread, so a thread-local
    // default subscriber sees its lines.
    let messages = CapturedMessages::default();
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(messages.clone()));
    let guard = tracing::dispatcher::set_default(&dispatch);
    let task = spawn(
        engine.clone(),
        ingest,
        identity(),
        source(
            format!("{base}/live/master.m3u8"),
            VariantRule::HighestBandwidth,
        ),
        limits(),
    );

    wait_for_request(&origin, "/live/seg0.ts").await;
    let packets = wait_for_media(&engine, start).await;
    drop(guard);

    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "the chosen video variant is pulled"
    );
    assert_eq!(
        origin.count("/live/audio/0/index.m3u8"),
        0,
        "a separate audio rendition is not followed"
    );
    assert_eq!(origin.count("/live/audio/0/a0.ts"), 0);
    assert_eq!(
        origin.count("/live/low.m3u8"),
        0,
        "the highest variant is still the one followed"
    );
    let lines = messages.matching("EXT-X-MEDIA rendition");
    assert_eq!(
        lines.len(),
        1,
        "the pull states the audio gap once, not once per segment: {lines:?}"
    );
    assert!(lines[0].contains("Track 1 (en)"), "{}", lines[0]);
    assert!(lines[0].contains("video-only"), "{}", lines[0]);

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

#[tokio::test]
async fn the_low_bandwidth_rule_follows_the_low_rendition() {
    let origin = Origin::new();
    origin.serve("/live/master.m3u8", MASTER);
    origin.serve("/live/low.m3u8", media_playlist(&["low0.ts"]));
    origin.serve("/live/high.m3u8", media_playlist(&["high0.ts"]));
    origin.serve("/live/low0.ts", ts_segment());
    origin.serve("/live/high0.ts", ts_segment());
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
    let task = spawn(
        engine.clone(),
        ingest,
        identity(),
        source(
            format!("{base}/live/master.m3u8"),
            VariantRule::LowestBandwidth,
        ),
        limits(),
    );

    wait_for_request(&origin, "/live/low0.ts").await;
    assert_eq!(origin.count("/live/low.m3u8"), 1);
    assert_eq!(origin.count("/live/high.m3u8"), 0);

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

#[tokio::test]
async fn bearer_auth_and_playlist_cookies_travel_with_every_request() {
    let origin = Origin::new();
    origin.serve("/live/index.m3u8", media_playlist(&["seg0.ts"]));
    origin.serve("/live/seg0.ts", ts_segment());
    origin.set_cookie("sid=abc; Path=/");
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
    let mut pull_source = source(
        format!("{base}/live/index.m3u8"),
        VariantRule::HighestBandwidth,
    );
    pull_source.auth = PullAuth::Bearer {
        token: "origin-token".to_string(),
    };
    let task = spawn(engine.clone(), ingest, identity(), pull_source, limits());

    wait_for_request(&origin, "/live/seg0.ts").await;
    let authorization = crate::sync::lock(&origin.authorization).clone();
    assert!(
        authorization
            .iter()
            .all(|value| value == "Bearer origin-token"),
        "{authorization:?}"
    );
    assert!(
        authorization.len() >= 2,
        "the playlist and segment requests carry it"
    );
    let cookies = crate::sync::lock(&origin.cookies).clone();
    assert!(
        cookies.iter().any(|cookie| cookie.contains("sid=abc")),
        "the playlist's cookie is presented with its segment: {cookies:?}"
    );

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

#[tokio::test]
async fn cancellation_stops_a_stalled_pull_and_joins() {
    let origin = Origin::new();
    let base = origin.spawn().await;
    let (engine, ingest) = test_ingest(limits());
    let task = spawn(
        engine.clone(),
        ingest,
        identity(),
        source(format!("{base}/stall.m3u8"), VariantRule::HighestBandwidth),
        limits(),
    );

    wait_for_request(&origin, "/stall.m3u8").await;
    task.cancel();
    tokio::time::timeout(Duration::from_secs(2), task.join())
        .await
        .expect("a cancelled pull joins promptly");
    assert!(!engine.input_has_session(PIPELINE_ID, INPUT_ID).await);
}

#[tokio::test]
async fn a_finished_pull_reports_why_it_stopped() {
    let origin = Origin::new();
    // An ended playlist: every listed segment is handed over, then the task
    // stops with `Ended`, which a restart would replay.
    origin.serve(
        "/live/index.m3u8",
        format!("{}#EXT-X-ENDLIST\n", media_playlist(&["seg0.ts"])),
    );
    origin.serve("/live/seg0.ts", ts_segment());
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
    let task = spawn(
        engine.clone(),
        ingest,
        identity(),
        source(
            format!("{base}/live/index.m3u8"),
            VariantRule::HighestBandwidth,
        ),
        limits(),
    );
    // A caller may keep the shared state and read it after dropping the
    // handle: that is what lets a registry see how a task ended.
    let end_state = task.end_state();
    assert!(!task.is_finished(), "the task is running");
    assert_eq!(task.end(), None, "a running task has no end");
    tokio::time::timeout(Duration::from_secs(15), async {
        while !task.is_finished() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the ended playlist finishes the task");
    assert_eq!(task.end(), Some(PullEnd::Ended));
    assert_eq!(*crate::sync::lock(&end_state), Some(PullEnd::Ended));
    engine.cancel_pipeline_input(INPUT_ID).await;

    // A source that can never be pulled ends `Fatal`: the source may
    // recover, so a caller may restart it.
    let (engine, ingest) = test_ingest(limits());
    let task = spawn(
        engine.clone(),
        ingest,
        identity(),
        source(
            "ftp://origin.example/live.m3u8".to_string(),
            VariantRule::HighestBandwidth,
        ),
        limits(),
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        while !task.is_finished() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a refused source ends the task");
    assert_eq!(task.end(), Some(PullEnd::Fatal));
    engine.cancel_pipeline_input(INPUT_ID).await;
}

#[tokio::test]
async fn a_failing_pull_never_logs_the_source_token() {
    let (engine, ingest) = test_ingest(limits());
    let messages = CapturedMessages::default();
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(messages.clone()));
    let guard = tracing::dispatcher::set_default(&dispatch);
    // Nothing listens on port 1, so the request fails at the transport, whose
    // `Display` embeds the URL unless it is stripped first.
    let task = spawn(
        engine.clone(),
        ingest,
        identity(),
        source(
            "http://127.0.0.1:1/live/index.m3u8?token=SECRETVALUE".to_string(),
            VariantRule::HighestBandwidth,
        ),
        limits(),
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        while messages.matching("HLS pull failed").is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the failing pull reports the retry");
    let lines = messages.matching("HLS pull failed");
    let all = messages.matching("");
    drop(guard);

    let logged = all.join("\n");
    assert_eq!(lines.len(), 1, "one retry line: {logged}");
    // `redact_url` keeps a prefix and a suffix of a token, as every other
    // credential in this codebase does; what it must never do is print the
    // token whole.
    assert!(
        !logged.contains("SECRETVALUE") && !logged.contains("token=SECRETVALUE"),
        "no line prints the signed query whole: {logged}"
    );
    // The transport error is named without its URL, which `Display` would
    // otherwise embed along with the token.
    assert!(logged.contains("origin request failed"), "{logged}");
    assert!(
        !logged.contains("for url"),
        "the transport error does not carry the URL: {logged}"
    );
    // The origin is still identifiable on the line that names it. That line
    // is `INFO`, and another test in this binary installs a global subscriber
    // (an `EnvFilter` built from an unset `RUST_LOG`), which can disable INFO
    // callsites process-wide and race this thread-local capture — so the
    // redaction itself is asserted, and the captured line must agree when it
    // arrives at all.
    let redacted = redact_url("http://127.0.0.1:1/live/index.m3u8?token=SECRETVALUE");
    assert!(redacted.contains("127.0.0.1"), "{redacted}");
    assert!(!redacted.contains("SECRETVALUE"), "{redacted}");
    let started = messages.matching("HLS pull started").join("\n");
    assert!(!started.contains("SECRETVALUE"), "{started}");
    if !started.is_empty() {
        assert!(started.contains("127.0.0.1"), "{started}");
    }

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}
