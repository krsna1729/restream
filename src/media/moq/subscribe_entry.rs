//! A headless MoQ subscriber over the production moq-net/hang path.
//!
//! [`run`] dials a relay or origin with native QUIC (the same
//! [`transport::connect`] and `moq_net::Client` the egress leaf and origin
//! use), resolves the broadcast, reads its hang catalog and counts what the
//! `video` and `audio` tracks deliver: frames, keyframes, audio frames and
//! bytes, startup delay and A/V arrival skew. It is the protocol evidence
//! behind the `moq-impairment` harness mode; the browser E2E stays the
//! product proof.
//!
//! The executable over this entry is `src/bin/moq_subscriber.rs`. It prints
//! one JSON object and exits non-zero only for a connection or protocol
//! failure; a short delivery window with few frames is a report the caller
//! judges, not a transport failure.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use moq_mux::container::{self, Kind, loc};
use moq_net::Timestamp;
use moq_net::origin;
use serde::Serialize;

use crate::media::moq::{tls, transport};
use crate::media::srt::AddressFamily;

/// The `hang` catalog track Restream publishes.
const CATALOG_TRACK: &str = "catalog.json";
/// Track names Restream publishes; used when the catalog names none.
const VIDEO_TRACK: &str = "video";
const AUDIO_TRACK: &str = "audio";
/// Frames whose media timestamps are within this tolerance carry the same
/// instant on the two tracks (30 fps video, ~21 ms AAC frames).
const SKEW_MATCH_TOLERANCE_US: u64 = 25_000;
/// How long a delivered frame stays available to be matched by the other
/// track. A retransmission gap longer than this is a delivery transient, not
/// an arrival skew sample, and the frame counts carry it.
const SKEW_MATCH_AGE: Duration = Duration::from_secs(2);
/// Per-track matching buffer bound; ~2 s of both tracks at the fixture rates.
const SKEW_BUFFER_FRAMES: usize = 256;
/// Skew samples retained for the percentile (a 15 s window has ~700).
const SKEW_SAMPLE_CAP: usize = 4096;
/// The subscriber joins two independent track cursors, so the first cached
/// groups can sit at different points of the shared timeline. Skew is only
/// taken after this much of the watch window, which excludes that join
/// transient (video groups are GOPs, audio about 1 s).
pub const SKEW_WARMUP: Duration = Duration::from_secs(2);

/// How the subscriber reaches the broadcast.
#[derive(Clone, Debug)]
pub struct SubscriberConfig {
    /// The peer's QUIC address (`https://127.0.0.1:<port>/` in the harness).
    pub url: String,
    /// The CA that signs the peer's leaf, as PEM.
    pub ca_pem: PathBuf,
    /// The announced broadcast path, e.g. `live/egress.hang`.
    pub broadcast: String,
    /// The single ALPN to offer, e.g. `moq-lite-06`.
    pub alpn: String,
    /// Server name the leaf is issued for.
    pub server_name: String,
    /// How long media is counted after the catalog resolves.
    pub watch: Duration,
    pub connect_timeout: Duration,
    pub catalog_timeout: Duration,
    /// When the caller judged the publisher ready (Unix epoch millis); the
    /// report's startup delay runs from here to the first delivered frame.
    pub publish_ready_unix_ms: u64,
}

/// The observation that set the largest skew sample: the matched media
/// timestamps and how much later the second track delivered the same instant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkewSample {
    pub video_us: u64,
    pub audio_us: u64,
    /// Milliseconds since the first delivered frame either way.
    pub at_ms: u64,
    pub arrival_skew_us: u64,
}

/// One delivered media track's counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaCounters {
    pub video_frames: u64,
    pub video_bytes: u64,
    pub keyframes: u64,
    pub audio_frames: u64,
    pub audio_bytes: u64,
    /// The skew this report measures.
    pub skew_kind: &'static str,
    /// Matched pairs used for the arrival skew.
    pub skew_samples: u64,
    /// Largest wall-clock gap between the two tracks delivering the same
    /// media instant (matched by timestamp within
    /// [`Self::skew_match_tolerance_us`]), in µs.
    pub max_abs_skew_us: u64,
    /// Smallest sampled arrival gap.
    pub min_abs_skew_us: Option<u64>,
    /// 95th percentile arrival gap. A loss-recovery spike is reported in
    /// [`Self::max_abs_skew_us`]; sustained A/V drift is this value.
    pub p95_abs_skew_us: u64,
    /// How close two frames' media timestamps must be to count as the same
    /// instant (30 fps video and ~21 ms AAC frames in the harness fixture).
    pub skew_match_tolerance_us: u64,
    /// The sample that set [`Self::max_abs_skew_us`].
    pub worst_skew: Option<SkewSample>,
    #[serde(skip)]
    samples: VecDeque<u64>,
    #[serde(skip)]
    recent_video: VecDeque<(u64, Instant)>,
    #[serde(skip)]
    recent_audio: VecDeque<(u64, Instant)>,
    #[serde(skip)]
    first_at: Option<Instant>,
    #[serde(skip)]
    warmup: Duration,
}

impl Default for MediaCounters {
    fn default() -> Self {
        Self {
            video_frames: 0,
            video_bytes: 0,
            keyframes: 0,
            audio_frames: 0,
            audio_bytes: 0,
            skew_kind: "arrival",
            skew_samples: 0,
            max_abs_skew_us: 0,
            min_abs_skew_us: None,
            p95_abs_skew_us: 0,
            skew_match_tolerance_us: SKEW_MATCH_TOLERANCE_US,
            worst_skew: None,
            samples: VecDeque::new(),
            recent_video: VecDeque::new(),
            recent_audio: VecDeque::new(),
            first_at: None,
            warmup: SKEW_WARMUP,
        }
    }
}

impl MediaCounters {
    /// Counters whose skew samples start after `warmup` (tests use zero).
    pub fn with_warmup(warmup: Duration) -> Self {
        Self {
            warmup,
            ..Self::default()
        }
    }

    /// A delivered video frame; `timestamp_us` is on the broadcast timeline.
    pub fn observe_video(&mut self, timestamp_us: u64, keyframe: bool, bytes: usize, now: Instant) {
        self.first_at.get_or_insert(now);
        self.video_frames = self.video_frames.saturating_add(1);
        self.video_bytes = self.video_bytes.saturating_add(bytes as u64);
        if keyframe {
            self.keyframes = self.keyframes.saturating_add(1);
        }
        let matched = match_nearest(&self.recent_audio, timestamp_us, now);
        if let Some((audio_us, arrival_us)) = matched {
            self.record_skew(timestamp_us, audio_us, arrival_us, now);
        }
        push_recent(&mut self.recent_video, timestamp_us, now);
    }

    /// A delivered audio frame; `timestamp_us` is on the broadcast timeline.
    pub fn observe_audio(&mut self, timestamp_us: u64, bytes: usize, now: Instant) {
        self.first_at.get_or_insert(now);
        self.audio_frames = self.audio_frames.saturating_add(1);
        self.audio_bytes = self.audio_bytes.saturating_add(bytes as u64);
        let matched = match_nearest(&self.recent_video, timestamp_us, now);
        if let Some((video_us, arrival_us)) = matched {
            self.record_skew(video_us, timestamp_us, arrival_us, now);
        }
        push_recent(&mut self.recent_audio, timestamp_us, now);
    }

    /// The stored samples' 95th percentile (0 without samples), nearest-rank
    /// like the harness's other percentile (`ceil(0.95*n)-1`).
    fn p95(&self) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        let mut values: Vec<u64> = self.samples.iter().copied().collect();
        values.sort_unstable();
        let index = (values.len() * 95).div_ceil(100).clamp(1, values.len()) - 1;
        values[index]
    }

    fn record_skew(&mut self, video_us: u64, audio_us: u64, arrival_us: u64, now: Instant) {
        if self
            .first_at
            .is_some_and(|first| now.saturating_duration_since(first) < self.warmup)
        {
            return;
        }
        self.skew_samples = self.skew_samples.saturating_add(1);
        if self.samples.len() == SKEW_SAMPLE_CAP {
            self.samples.pop_front();
        }
        self.samples.push_back(arrival_us);
        self.min_abs_skew_us = Some(
            self.min_abs_skew_us
                .map_or(arrival_us, |min| min.min(arrival_us)),
        );
        if arrival_us >= self.max_abs_skew_us {
            self.max_abs_skew_us = arrival_us;
            self.worst_skew = Some(SkewSample {
                video_us,
                audio_us,
                at_ms: self.first_at.map_or(0, |first| {
                    now.saturating_duration_since(first).as_millis() as u64
                }),
                arrival_skew_us: arrival_us,
            });
        }
    }
}

/// The nearest buffered frame within the match tolerance, as
/// `(its media timestamp, how long ago it arrived)`.
fn match_nearest(
    buffer: &VecDeque<(u64, Instant)>,
    timestamp_us: u64,
    now: Instant,
) -> Option<(u64, u64)> {
    buffer
        .iter()
        .filter_map(|(pts, at)| {
            let age = now.saturating_duration_since(*at);
            let delta = pts.abs_diff(timestamp_us);
            (delta <= SKEW_MATCH_TOLERANCE_US && age <= SKEW_MATCH_AGE)
                .then_some((delta, *pts, age))
        })
        .min_by_key(|(delta, _, _)| *delta)
        .map(|(_, pts, age)| (pts, age.as_micros().min(u128::from(u64::MAX)) as u64))
}

fn push_recent(buffer: &mut VecDeque<(u64, Instant)>, timestamp_us: u64, now: Instant) {
    buffer.push_back((timestamp_us, now));
    while buffer.len() > SKEW_BUFFER_FRAMES {
        buffer.pop_front();
    }
    while buffer
        .front()
        .is_some_and(|(_, at)| now.saturating_duration_since(*at) > SKEW_MATCH_AGE)
    {
        buffer.pop_front();
    }
}

/// The JSON object the subscriber prints.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriberReport {
    pub url: String,
    pub broadcast: String,
    pub alpn: String,
    pub publish_ready_unix_ms: u64,
    pub connected: bool,
    pub connection_ms: f64,
    pub catalog_ms: f64,
    pub video_track: Option<String>,
    pub audio_track: Option<String>,
    pub watch_secs: f64,
    pub first_media_unix_ms: Option<u64>,
    /// First delivered media (either track) minus publish-ready.
    pub startup_delay_ms: Option<f64>,
    pub ended: bool,
    #[serde(flatten)]
    pub counters: MediaCounters,
    pub error: Option<String>,
}

/// One run's report plus its terminal verdict. A connection or protocol
/// failure sets `report.error`; the counts collected before it are kept.
#[derive(Clone, Debug, PartialEq)]
pub struct SubscriberOutcome {
    pub report: SubscriberReport,
}

impl SubscriberOutcome {
    pub fn protocol_failed(&self) -> bool {
        self.report.error.is_some()
    }
}

/// Why [`run`] could not produce a report. [`Self::Config`] is a rejected
/// invocation (URL, ALPN, CA path or PEM) and belongs with the usage exit;
/// [`Self::Runtime`] means the report machinery itself could not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubscriberError {
    Config(String),
    Runtime(String),
}

impl std::fmt::Display for SubscriberError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(message) => write!(f, "configuration: {message}"),
            Self::Runtime(message) => write!(f, "runtime: {message}"),
        }
    }
}

impl std::error::Error for SubscriberError {}

/// Connect, resolve `broadcast`, count both media tracks for `watch`, and
/// return the report. A rejected configuration returns [`SubscriberError::Config`]
/// before any network work; connection and protocol failures set
/// `report.error` and keep the counts gathered so far.
pub fn run(config: SubscriberConfig) -> Result<SubscriberOutcome, SubscriberError> {
    parse_target(&config.url).map_err(SubscriberError::Config)?;
    if !moq_net::ALPNS.contains(&config.alpn.as_str()) {
        return Err(SubscriberError::Config(format!(
            "unsupported ALPN {:?}; expected one of {}",
            config.alpn,
            moq_net::ALPNS.join(", ")
        )));
    }
    let ca = std::fs::read(&config.ca_pem).map_err(|error| {
        SubscriberError::Config(format!("read CA {}: {error}", config.ca_pem.display()))
    })?;
    let tls = tls::client_config(&[config.alpn.as_str()], Some(&ca), false)
        .map_err(SubscriberError::Config)?;
    let runtime = compio::runtime::Runtime::new().map_err(|error| {
        SubscriberError::Runtime(format!(
            "create compio runtime for the MoQ subscriber: {error}"
        ))
    })?;
    runtime.block_on(run_async(config, tls))
}

/// Parse `url` into (remote address, setup path).
pub(crate) fn parse_target(url: &str) -> Result<(SocketAddr, String), String> {
    let parsed =
        url::Url::parse(url).map_err(|error| format!("subscriber url {url:?}: {error}"))?;
    if parsed.scheme() != "https" {
        return Err(format!("subscriber url {url:?} is not https://"));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("subscriber url {url:?} has no host"))?;
    let ip: IpAddr = host
        .parse()
        .map_err(|error| format!("subscriber url host {host:?}: {error}"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| format!("subscriber url {url:?} has no port"))?;
    let mut path = parsed.path().to_string();
    if path.is_empty() {
        path.push('/');
    }
    if let Some(query) = parsed.query() {
        path.push('?');
        path.push_str(query);
    }
    Ok((SocketAddr::new(ip, port), path))
}

fn epoch_millis(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn micros(timestamp: Timestamp) -> u64 {
    timestamp.as_micros().min(u128::from(u64::MAX)) as u64
}

/// What one track task ended with.
enum TrackEnd {
    Window,
    Ended,
    Failed(String),
}

#[derive(Default)]
struct Observation {
    counters: MediaCounters,
    first_media: Option<SystemTime>,
}

impl Observation {
    fn note_media(&mut self, at: SystemTime) {
        self.first_media.get_or_insert(at);
    }
}

async fn read_track(
    mut consumer: container::Consumer<loc::Wire>,
    kind: Kind,
    shared: Rc<RefCell<Observation>>,
    watch: Duration,
) -> TrackEnd {
    let read = async {
        loop {
            match consumer.read().await {
                Ok(Some(frame)) => {
                    let now = Instant::now();
                    let timestamp_us = micros(frame.timestamp);
                    let bytes = frame.payload.len();
                    let mut observation = shared.borrow_mut();
                    match kind {
                        Kind::Video => observation.counters.observe_video(
                            timestamp_us,
                            frame.keyframe,
                            bytes,
                            now,
                        ),
                        _ => observation.counters.observe_audio(timestamp_us, bytes, now),
                    }
                    observation.note_media(SystemTime::now());
                }
                Ok(None) => return TrackEnd::Ended,
                Err(error) => return TrackEnd::Failed(error.to_string()),
            }
        }
    };
    match compio::time::timeout(watch, read).await {
        Ok(end) => end,
        Err(_) => TrackEnd::Window,
    }
}

async fn run_async(
    config: SubscriberConfig,
    tls: compio::quic::ClientConfig,
) -> Result<SubscriberOutcome, SubscriberError> {
    let (remote, setup_path) = parse_target(&config.url).map_err(SubscriberError::Config)?;
    let started = Instant::now();
    let mut report = SubscriberReport {
        url: config.url.clone(),
        broadcast: config.broadcast.clone(),
        alpn: config.alpn.clone(),
        publish_ready_unix_ms: config.publish_ready_unix_ms,
        watch_secs: config.watch.as_secs_f64(),
        ..SubscriberReport::default()
    };
    let connect = async {
        let endpoint = transport::client_endpoint(AddressFamily::V4)
            .await
            .map_err(|error| format!("MoQ client endpoint: {error}"))?;
        let session = transport::connect(&endpoint, remote, &config.server_name, tls)
            .await
            .map_err(|error| error.to_string())?;
        let (viewer, origin_driver) =
            origin::Producer::new(origin::Config::new(moq_net::Hop::random()));
        compio::runtime::spawn(transport::run_driver(origin_driver)).detach();
        let (session, driver) = moq_net::Client::new()
            .with_subscriber(viewer.clone())
            .with_path(setup_path)
            .connect(std::time::Instant::now(), session)
            .await
            .map_err(|error| format!("MoQ handshake: {error}"))?;
        compio::runtime::spawn(transport::run_driver(driver)).detach();
        Ok::<_, String>((viewer, session, endpoint))
    };
    let connected = compio::time::timeout(config.connect_timeout, connect).await;
    let (viewer, _session, _endpoint) = match connected {
        Ok(Ok(parts)) => {
            report.connected = true;
            report.connection_ms = started.elapsed().as_secs_f64() * 1000.0;
            parts
        }
        Ok(Err(error)) => {
            report.connection_ms = started.elapsed().as_secs_f64() * 1000.0;
            report.error = Some(error);
            return Ok(SubscriberOutcome { report });
        }
        Err(_) => {
            report.connection_ms = started.elapsed().as_secs_f64() * 1000.0;
            report.error = Some(format!(
                "MoQ connect exceeded {} s",
                config.connect_timeout.as_secs()
            ));
            return Ok(SubscriberOutcome { report });
        }
    };
    let connected_at = Instant::now();

    let consumer = viewer.consume();
    let catalog = compio::time::timeout(
        config.catalog_timeout,
        consumer.routed_broadcast(config.broadcast.as_str()),
    )
    .await;
    let broadcast = match catalog {
        Ok(Ok(broadcast)) => {
            report.catalog_ms = connected_at.elapsed().as_secs_f64() * 1000.0;
            broadcast
        }
        Ok(Err(error)) => {
            report.catalog_ms = connected_at.elapsed().as_secs_f64() * 1000.0;
            report.error = Some(format!("resolve broadcast {:?}: {error}", config.broadcast));
            return Ok(SubscriberOutcome { report });
        }
        Err(_) => {
            report.catalog_ms = connected_at.elapsed().as_secs_f64() * 1000.0;
            report.error = Some(format!(
                "broadcast {:?} did not resolve within {} s",
                config.broadcast,
                config.catalog_timeout.as_secs()
            ));
            return Ok(SubscriberOutcome { report });
        }
    };

    let catalog = match compio::time::timeout(config.catalog_timeout, async {
        let mut catalog = hang::Catalog::<()>::subscribe(&broadcast).await?;
        catalog.next().await
    })
    .await
    {
        Ok(Ok(Some(catalog))) => catalog,
        Ok(Ok(None)) => {
            report.catalog_ms = connected_at.elapsed().as_secs_f64() * 1000.0;
            report.error = Some(format!(
                "broadcast {:?} ended its catalog track without a catalog",
                config.broadcast
            ));
            return Ok(SubscriberOutcome { report });
        }
        Ok(Err(error)) => {
            report.catalog_ms = connected_at.elapsed().as_secs_f64() * 1000.0;
            report.error = Some(format!("read {CATALOG_TRACK}: {error}"));
            return Ok(SubscriberOutcome { report });
        }
        Err(_) => {
            report.catalog_ms = connected_at.elapsed().as_secs_f64() * 1000.0;
            report.error = Some(format!(
                "catalog track did not resolve within {} s",
                config.catalog_timeout.as_secs()
            ));
            return Ok(SubscriberOutcome { report });
        }
    };
    report.catalog_ms = connected_at.elapsed().as_secs_f64() * 1000.0;

    let video_track = catalog
        .video
        .renditions
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| VIDEO_TRACK.to_string());
    let audio_track = catalog
        .audio
        .renditions
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| AUDIO_TRACK.to_string());
    report.video_track = Some(video_track.clone());
    report.audio_track = Some(audio_track.clone());

    let video = match broadcast
        .track(&video_track)
        .map_err(|error| format!("video track {video_track:?}: {error}"))
        .map(|track| track.subscribe(None::<moq_net::track::Subscription>))
    {
        Ok(subscribe) => compio::time::timeout(config.catalog_timeout, subscribe).await,
        Err(error) => {
            report.error = Some(error);
            return Ok(SubscriberOutcome { report });
        }
    };
    let video = match video {
        Ok(Ok(subscriber)) => container::Consumer::new(subscriber, loc::Wire(Kind::Video)),
        Ok(Err(error)) => {
            report.error = Some(format!("subscribe to {video_track:?}: {error}"));
            return Ok(SubscriberOutcome { report });
        }
        Err(_) => {
            report.error = Some(format!(
                "subscribe to {video_track:?} exceeded {} s",
                config.catalog_timeout.as_secs()
            ));
            return Ok(SubscriberOutcome { report });
        }
    };

    let audio = match broadcast
        .track(&audio_track)
        .map_err(|error| format!("audio track {audio_track:?}: {error}"))
        .map(|track| track.subscribe(None::<moq_net::track::Subscription>))
    {
        Ok(subscribe) => compio::time::timeout(config.catalog_timeout, subscribe).await,
        Err(error) => {
            report.error = Some(error);
            return Ok(SubscriberOutcome { report });
        }
    };
    let audio = match audio {
        Ok(Ok(subscriber)) => container::Consumer::new(subscriber, loc::Wire(Kind::Audio)),
        Ok(Err(error)) => {
            report.error = Some(format!("subscribe to {audio_track:?}: {error}"));
            return Ok(SubscriberOutcome { report });
        }
        Err(_) => {
            report.error = Some(format!(
                "subscribe to {audio_track:?} exceeded {} s",
                config.catalog_timeout.as_secs()
            ));
            return Ok(SubscriberOutcome { report });
        }
    };

    let shared = Rc::new(RefCell::new(Observation::default()));
    let watch = config.watch;
    let video_task =
        compio::runtime::spawn(read_track(video, Kind::Video, Rc::clone(&shared), watch));
    let audio_task =
        compio::runtime::spawn(read_track(audio, Kind::Audio, Rc::clone(&shared), watch));
    let (video_end, audio_end) = futures_util::join!(video_task, audio_task);
    let video_end = match video_end {
        Ok(end) => end,
        Err(error) => TrackEnd::Failed(format!("video task: {error}")),
    };
    let audio_end = match audio_end {
        Ok(end) => end,
        Err(error) => TrackEnd::Failed(format!("audio task: {error}")),
    };
    let observation = shared.borrow();
    let mut counters = observation.counters.clone();
    counters.p95_abs_skew_us = counters.p95();
    report.counters = counters;
    report.first_media_unix_ms = observation.first_media.map(epoch_millis);
    report.startup_delay_ms = observation.first_media.map(|first| {
        let first = epoch_millis(first) as f64;
        (first - config.publish_ready_unix_ms as f64).max(0.0)
    });
    report.ended = matches!(video_end, TrackEnd::Ended) && matches!(audio_end, TrackEnd::Ended);
    let error = match (video_end, audio_end) {
        (TrackEnd::Failed(error), _) | (_, TrackEnd::Failed(error)) => Some(error),
        _ => None,
    };
    report.error = error;
    Ok(SubscriberOutcome { report })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(millis: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_millis(millis)
    }

    #[test]
    fn counts_frames_keyframes_and_audio_bytes() {
        let mut counters = MediaCounters::with_warmup(Duration::ZERO);
        counters.observe_video(0, true, 100, at(0));
        counters.observe_video(33_333, false, 20, at(33));
        counters.observe_audio(0, 10, at(0));
        counters.observe_audio(21_333, 11, at(21));
        assert_eq!(counters.video_frames, 2);
        assert_eq!(counters.video_bytes, 120);
        assert_eq!(counters.keyframes, 1);
        assert_eq!(counters.audio_frames, 2);
        assert_eq!(counters.audio_bytes, 21);
    }

    #[test]
    fn arrival_skew_matches_the_nearest_media_time() {
        let mut counters = MediaCounters::with_warmup(Duration::ZERO);
        counters.observe_audio(0, 1, at(0));
        counters.observe_video(10_000, true, 1, at(40));
        // Same instant (10 ms apart in media time) delivered 40 ms later on
        // video.
        assert_eq!(counters.skew_samples, 1);
        assert_eq!(counters.max_abs_skew_us, 40_000);
        // No audio near this video instant: no pair, no sample.
        counters.observe_video(1_000_000, false, 1, at(100));
        assert_eq!(counters.skew_samples, 1);
        // Audio now matches the buffered video instant and is 60 ms later.
        counters.observe_audio(1_010_000, 1, at(160));
        assert_eq!(counters.skew_samples, 2);
        assert_eq!(counters.max_abs_skew_us, 60_000);
        assert_eq!(counters.min_abs_skew_us, Some(40_000));
        let worst = counters.worst_skew.expect("worst sample");
        assert_eq!(worst.video_us, 1_000_000);
        assert_eq!(worst.audio_us, 1_010_000);
        assert_eq!(worst.arrival_skew_us, 60_000);
    }

    #[test]
    fn frames_outside_the_match_tolerance_are_not_a_pair() {
        let mut counters = MediaCounters::with_warmup(Duration::ZERO);
        counters.observe_audio(0, 1, at(0));
        // 30 ms apart in media time exceeds the 25 ms tolerance.
        counters.observe_video(30_000, true, 1, at(10));
        assert_eq!(counters.skew_samples, 0);
        counters.observe_video(20_000, false, 1, at(20));
        assert_eq!(counters.skew_samples, 1);
        assert_eq!(counters.max_abs_skew_us, 20_000);
    }

    #[test]
    fn a_stale_frame_is_not_matched() {
        let mut counters = MediaCounters::with_warmup(Duration::ZERO);
        counters.observe_audio(0, 1, at(0));
        // More than SKEW_MATCH_AGE later the buffered audio frame is gone.
        counters.observe_video(0, true, 1, at(2_100));
        assert_eq!(counters.skew_samples, 0);
    }

    #[test]
    fn skew_samples_wait_for_the_join_warmup() {
        let mut counters = MediaCounters::with_warmup(Duration::from_secs(2));
        counters.observe_video(1_000_000, true, 1, at(0));
        counters.observe_audio(1_000_000, 1, at(10));
        // Inside the warmup window both tracks can still sit on different
        // cached groups; that is a join transient, not A/V skew.
        assert_eq!(counters.skew_samples, 0);
        assert_eq!(counters.max_abs_skew_us, 0);
        counters.observe_audio(2_480_000, 1, at(2_050));
        counters.observe_video(2_490_000, false, 1, at(2_100));
        assert_eq!(counters.skew_samples, 1);
        assert_eq!(counters.max_abs_skew_us, 50_000);
    }

    #[test]
    fn p95_reports_sustained_skew_not_a_retransmission_spike() {
        let mut counters = MediaCounters::with_warmup(Duration::ZERO);
        for index in 0..20u64 {
            let pts = index * 1_000_000;
            let millis = index * 1000;
            counters.observe_audio(pts, 1, at(millis));
            counters.observe_video(pts, index == 0, 1, at(millis + 10));
        }
        counters.observe_audio(20_000_000, 1, at(20_000));
        counters.observe_video(20_000_000, true, 1, at(20_900));
        assert_eq!(counters.skew_samples, 21);
        assert_eq!(counters.p95(), 10_000);
        assert_eq!(counters.max_abs_skew_us, 900_000);
    }

    #[test]
    fn p95_uses_nearest_rank_at_twenty_samples() {
        let mut counters = MediaCounters::with_warmup(Duration::ZERO);
        for _ in 0..19 {
            counters.samples.push_back(10_000);
        }
        counters.samples.push_back(900_000);
        // ceil(0.95 * 20) - 1 = 18: the 19th smallest, not the maximum.
        assert_eq!(counters.p95(), 10_000);
    }

    #[test]
    fn target_parsing_keeps_the_setup_path_and_query() {
        let (remote, path) = parse_target("https://127.0.0.1:4443/?token=abc").expect("target");
        assert_eq!(remote, SocketAddr::from(([127, 0, 0, 1], 4443)));
        assert_eq!(path, "/?token=abc");
        let (_, path) = parse_target("https://127.0.0.1:4443").expect("target");
        assert_eq!(path, "/");
        assert!(parse_target("http://127.0.0.1:4443/").is_err());
    }

    fn subscriber_config() -> SubscriberConfig {
        SubscriberConfig {
            url: "https://127.0.0.1:4443/".into(),
            ca_pem: PathBuf::from("/nonexistent/ca.pem"),
            broadcast: "live/x.hang".into(),
            alpn: "moq-lite-06".into(),
            server_name: "localhost".into(),
            watch: Duration::from_secs(1),
            connect_timeout: Duration::from_secs(1),
            catalog_timeout: Duration::from_secs(1),
            publish_ready_unix_ms: 0,
        }
    }

    #[test]
    fn rejected_invocations_are_configuration_errors_before_connecting() {
        let mut bad_url = subscriber_config();
        bad_url.url = "http://127.0.0.1:4443/".into();
        assert!(matches!(run(bad_url), Err(SubscriberError::Config(_))));

        let mut bad_alpn = subscriber_config();
        bad_alpn.alpn = "not-a-moq-alpn".into();
        assert!(matches!(run(bad_alpn), Err(SubscriberError::Config(_))));

        let missing_ca = subscriber_config();
        assert!(matches!(run(missing_ca), Err(SubscriberError::Config(_))));

        let ca =
            std::env::temp_dir().join(format!("moq-subscriber-bad-ca-{}.pem", std::process::id()));
        std::fs::write(&ca, b"not a certificate").expect("write test CA");
        let mut bad_pem = subscriber_config();
        bad_pem.ca_pem = ca.clone();
        let rejected = run(bad_pem);
        let _ = std::fs::remove_file(&ca);
        assert!(matches!(rejected, Err(SubscriberError::Config(_))));
    }

    #[test]
    fn report_serializes_the_documented_fields() {
        let report = SubscriberReport {
            url: "https://127.0.0.1:4443/".into(),
            broadcast: "live/x.hang".into(),
            alpn: "moq-lite-06".into(),
            publish_ready_unix_ms: 1_000,
            connected: true,
            connection_ms: 5.0,
            catalog_ms: 7.5,
            video_track: Some("video".into()),
            audio_track: Some("audio".into()),
            watch_secs: 15.0,
            first_media_unix_ms: Some(1_120),
            startup_delay_ms: Some(120.0),
            ended: false,
            counters: MediaCounters {
                video_frames: 450,
                video_bytes: 1_000,
                keyframes: 8,
                audio_frames: 700,
                audio_bytes: 2_000,
                skew_samples: 1_100,
                max_abs_skew_us: 33_000,
                ..MediaCounters::default()
            },
            error: None,
        };
        let value = serde_json::to_value(&report).expect("report json");
        assert_eq!(value["videoFrames"], 450);
        assert_eq!(value["keyframes"], 8);
        assert_eq!(value["audioBytes"], 2_000);
        assert_eq!(value["maxAbsSkewUs"], 33_000);
        assert_eq!(value["skewKind"], "arrival");
        assert_eq!(value["startupDelayMs"], 120.0);
        assert_eq!(value["firstMediaUnixMs"], 1_120);
        assert!(value.get("latestVideoUs").is_none());
    }
}
