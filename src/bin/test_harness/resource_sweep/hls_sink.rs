//! Counting HLS PUT receiver and the segment-based delivery rule for resource
//! sweeps (`MSR_PEER=sink`).
//!
//! Restream uploads each HLS segment (cut on keyframes, about 3 s with the
//! checked-in fixtures and at most the 6 s target) and the playlist to every
//! HLS PUT output. Each output names segments its own way (an output-local
//! number behind a per-attempt token), so a segment is identified by its
//! bytes: every output uploads the same bytes for the same segment. The
//! sink streams and discards each body, so 1000 concurrent segment uploads
//! never buffer whole segments, and records when each output finished each
//! segment.
//!
//! Byte-rate delivery cannot grade HLS: a 30 s window holds only about ten
//! segments, so an output one segment behind at the window edge reads as 0.9
//! of the offered rate and fails the 0.95 floor. Instead a segment is *due* once any output has it and
//! `HLS_LAG_BUDGET` has passed; an output is delivered when it received every
//! due segment of the window, each within `HLS_LAG_BUDGET` of the first output
//! to receive it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::any;
use futures_util::StreamExt;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// How far behind the first output any output may finish a segment: about one
/// segment with the fixtures, half the 6 s target duration, so a live player
/// with the usual three-segment buffer never starves.
pub(super) const HLS_LAG_BUDGET: Duration = Duration::from_secs(3);

/// Destination-name prefix for HLS byte counters in delivery samples.
pub(super) const HLS_DESTINATION_PREFIX: &str = "hls-sink:";

#[derive(Default)]
struct Arrivals {
    /// Cumulative body bytes per output (`cid`).
    bytes: HashMap<String, u64>,
    /// Completion instant of each segment (keyed by a hash of its bytes) at
    /// each output.
    segments: HashMap<String, BTreeMap<u64, Instant>>,
}

pub(crate) struct HlsCountingSink {
    arrivals: Arc<Mutex<Arrivals>>,
    cancel: CancellationToken,
}

/// Cloneable read handle over a running sink's counters.
#[derive(Clone)]
pub(crate) struct HlsSinkHandle {
    arrivals: Arc<Mutex<Arrivals>>,
}

impl HlsCountingSink {
    pub(crate) async fn start(port: u16) -> Result<Self, String> {
        let arrivals = Arc::new(Mutex::new(Arrivals::default()));
        let app = Router::new()
            .route("/{*path}", any(receive))
            .with_state(arrivals.clone());
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|error| format!("harness HLS PUT sink on {port}: {error}"))?;
        let cancel = CancellationToken::new();
        let shutdown = cancel.clone();
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
            {
                eprintln!("[hls-sink] server failed: {error}");
            }
        });
        Ok(Self { arrivals, cancel })
    }

    pub(crate) fn handle(&self) -> HlsSinkHandle {
        HlsSinkHandle {
            arrivals: self.arrivals.clone(),
        }
    }

    pub(crate) fn stop(self) {
        self.cancel.cancel();
    }
}

async fn receive(
    State(arrivals): State<Arc<Mutex<Arrivals>>>,
    Query(query): Query<HashMap<String, String>>,
    body: Body,
) -> StatusCode {
    let cid = query.get("cid").cloned().unwrap_or_default();
    let mut received = 0u64;
    let mut content = SegmentIdentity::default();
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => {
                received += chunk.len() as u64;
                content.update(&chunk);
            }
            Err(_) => return StatusCode::BAD_REQUEST,
        }
    }
    let segment = query
        .get("file")
        .is_some_and(|file| file.ends_with(".ts"))
        .then(|| content.finish());
    let now = Instant::now();
    let mut arrivals = restream::sync::lock(&arrivals);
    *arrivals.bytes.entry(cid.clone()).or_default() += received;
    if let Some(index) = segment {
        arrivals
            .segments
            .entry(cid)
            .or_default()
            .entry(index)
            .or_insert(now);
    }
    StatusCode::NO_CONTENT
}

/// Bytes at each end of a body that, with its length, identify a segment.
const IDENTITY_EDGE_BYTES: usize = 4096;

/// A segment's identity from its length and its first and last
/// `IDENTITY_EDGE_BYTES`, independent of how the body was chunked. Every
/// output uploads the same bytes for a segment; two segments of one window
/// differ in length or in their final TS packets. Constant work per body:
/// hashing every byte cost the receiver about one core at HLS x1000, which
/// delayed its arrival timestamps and inflated the measured lag.
#[derive(Default)]
struct SegmentIdentity {
    length: u64,
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
}

impl SegmentIdentity {
    fn update(&mut self, bytes: &[u8]) {
        self.length += bytes.len() as u64;
        let head_room = IDENTITY_EDGE_BYTES.saturating_sub(self.head.len());
        self.head
            .extend_from_slice(&bytes[..head_room.min(bytes.len())]);
        let keep = bytes.len().min(IDENTITY_EDGE_BYTES);
        self.tail.extend(&bytes[bytes.len() - keep..]);
        while self.tail.len() > IDENTITY_EDGE_BYTES {
            self.tail.pop_front();
        }
    }

    fn finish(&self) -> u64 {
        let mut hash = Fnv1a::new();
        hash.update(&self.length.to_le_bytes());
        hash.update(&self.head);
        let (first, second) = self.tail.as_slices();
        hash.update(first);
        hash.update(second);
        hash.finish()
    }
}

/// FNV-1a over a byte stream; the result does not depend on how the stream
/// is chunked.
struct Fnv1a(u64);

impl Fnv1a {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

impl HlsSinkHandle {
    /// Cumulative body bytes per output, for fairness over byte rates.
    pub(super) fn per_output_bytes(&self) -> Vec<(String, u64)> {
        let arrivals = restream::sync::lock(&self.arrivals);
        arrivals
            .bytes
            .iter()
            .map(|(cid, bytes)| (cid.clone(), *bytes))
            .collect()
    }

    pub(super) fn window(&self, start: Instant, end: Instant) -> HlsWindow {
        let arrivals = restream::sync::lock(&self.arrivals);
        // An output that uploaded only playlists (or failed every segment)
        // still counts as a destination.
        let mut segments = arrivals.segments.clone();
        for cid in arrivals.bytes.keys() {
            segments.entry(cid.clone()).or_default();
        }
        summarize_window(&segments, start, end, HLS_LAG_BUDGET)
    }
}

/// Segment delivery over one rated window.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct HlsWindow {
    pub(super) destinations: usize,
    pub(super) delivered: usize,
    /// Segments due in the window (first received in `[start, end - budget]`).
    pub(super) due_segments: usize,
    /// Per output: due segments received / due segments.
    pub(super) ratio_min: f64,
    pub(super) ratio_median: f64,
    /// Finish time behind the first output, over every due segment received.
    pub(super) lag_p50_ms: u64,
    pub(super) lag_p99_ms: u64,
    pub(super) lag_max_ms: u64,
}

pub(super) fn summarize_window(
    segments: &HashMap<String, BTreeMap<u64, Instant>>,
    start: Instant,
    end: Instant,
    budget: Duration,
) -> HlsWindow {
    let mut first: BTreeMap<u64, Instant> = BTreeMap::new();
    for arrivals in segments.values() {
        for (index, at) in arrivals {
            first
                .entry(*index)
                .and_modify(|seen| *seen = (*seen).min(*at))
                .or_insert(*at);
        }
    }
    let due: Vec<(u64, Instant)> = first
        .into_iter()
        .filter(|(_, at)| *at >= start && *at + budget <= end)
        .collect();
    let mut ratios = Vec::with_capacity(segments.len());
    let mut lags = Vec::new();
    let mut delivered = 0;
    for arrivals in segments.values() {
        let mut received = 0;
        let mut late = false;
        for (index, first_at) in &due {
            if let Some(at) = arrivals.get(index) {
                let lag = at.saturating_duration_since(*first_at);
                late |= lag > budget;
                received += 1;
                lags.push(lag.as_millis() as u64);
            }
        }
        let ratio = if due.is_empty() {
            0.0
        } else {
            received as f64 / due.len() as f64
        };
        if !due.is_empty() && received == due.len() && !late {
            delivered += 1;
        }
        ratios.push(ratio);
    }
    ratios.sort_by(f64::total_cmp);
    lags.sort_unstable();
    let percentile = |p: usize| {
        lags.get((lags.len().saturating_sub(1) * p) / 100)
            .copied()
            .unwrap_or(0)
    };
    HlsWindow {
        destinations: segments.len(),
        delivered,
        due_segments: due.len(),
        ratio_min: ratios.first().copied().unwrap_or(0.0),
        ratio_median: ratios.get(ratios.len() / 2).copied().unwrap_or(0.0),
        lag_p50_ms: percentile(50),
        lag_p99_ms: percentile(99),
        lag_max_ms: lags.last().copied().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arrivals(
        start: Instant,
        rows: &[(&str, &[(u64, u64)])],
    ) -> HashMap<String, BTreeMap<u64, Instant>> {
        rows.iter()
            .map(|(cid, segments)| {
                let segments = segments
                    .iter()
                    .map(|(index, millis)| (*index, start + Duration::from_millis(*millis)))
                    .collect();
                (cid.to_string(), segments)
            })
            .collect()
    }

    #[test]
    fn outputs_with_every_due_segment_within_budget_are_delivered() {
        let start = Instant::now();
        let end = start + Duration::from_secs(30);
        let rows = arrivals(
            start,
            &[
                ("a", &[(1, 1_000), (2, 7_000), (3, 13_000), (4, 28_000)]),
                ("b", &[(1, 1_400), (2, 7_600), (3, 13_200)]),
            ],
        );
        let window = summarize_window(&rows, start, end, HLS_LAG_BUDGET);
        // Segment 4 first arrived 2 s before the end: not yet due.
        assert_eq!(window.due_segments, 3);
        assert_eq!(window.destinations, 2);
        assert_eq!(window.delivered, 2);
        assert_eq!(window.ratio_min, 1.0);
        assert_eq!(window.lag_max_ms, 600);
    }

    #[test]
    fn a_missing_or_late_segment_fails_that_output_only() {
        let start = Instant::now();
        let end = start + Duration::from_secs(30);
        let rows = arrivals(
            start,
            &[
                ("fast", &[(1, 1_000), (2, 7_000), (3, 13_000)]),
                ("missing", &[(1, 1_100), (3, 13_100)]),
                ("late", &[(1, 1_000), (2, 11_000), (3, 13_000)]),
            ],
        );
        let window = summarize_window(&rows, start, end, HLS_LAG_BUDGET);
        assert_eq!(window.due_segments, 3);
        assert_eq!(window.delivered, 1);
        assert!((window.ratio_min - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(window.lag_max_ms, 4_000);
    }

    #[test]
    fn segment_identity_does_not_depend_on_chunking() {
        // Larger than both edges, so head, middle and tail all exist.
        let body: Vec<u8> = (0..20_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut whole = SegmentIdentity::default();
        whole.update(&body);
        for cut in [1, 4095, 4096, 4097, 9_000, 19_999] {
            let mut split = SegmentIdentity::default();
            split.update(&body[..cut]);
            split.update(&body[cut..]);
            assert_eq!(whole.finish(), split.finish(), "cut at {cut}");
        }
        let mut other_end = body.clone();
        *other_end.last_mut().unwrap() ^= 1;
        let mut other = SegmentIdentity::default();
        other.update(&other_end);
        assert_ne!(whole.finish(), other.finish(), "a different last packet");
        let mut shorter = SegmentIdentity::default();
        shorter.update(&body[..19_000]);
        assert_ne!(whole.finish(), shorter.finish(), "a different length");
    }

    #[test]
    fn no_segments_means_nothing_delivered() {
        let start = Instant::now();
        let rows = arrivals(start, &[("a", &[])]);
        let window = summarize_window(
            &rows,
            start,
            start + Duration::from_secs(30),
            HLS_LAG_BUDGET,
        );
        assert_eq!(window.delivered, 0);
        assert_eq!(window.due_segments, 0);
    }
}
