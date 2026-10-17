//! HLS pull ingest: the guards a real origin can prove — where credentials
//! may go, where the first cycle enters a window, an expired variant URL, a
//! stuck object, and cancellation. The origin stub and its helpers live in
//! `tests_origin.rs`; this module imports them.

use std::time::Duration;

use super::fetch::{AuthScope, RequestAuth, build_client};
use super::tests_origin::{
    CapturedMessages, Origin, identity, limits, media_playlist, source, ts_segment, wait_for_media,
    wait_for_request,
};
use super::*;
use crate::domain::pipeline_input::{HeaderPair, PullAuth, VariantRule};
use crate::media::hls_ingest::test_support::{
    INPUT_ID, PIPELINE_ID, STREAM_KEY, authenticated, test_ingest,
};
use crate::media::packet::MediaType;
use tracing_subscriber::layer::SubscriberExt as _;

// ---- the credential guards against a real origin -------------------------

async fn wait_for_requests(origin: &Origin, path: &str, count: usize) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while origin.count(path) < count {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("origin never saw {count} requests for {path}"));
}

/// Wait until the pull logs a line containing `needle`, and return them.
async fn wait_for_lines(log: &CapturedMessages, needle: &str) -> Vec<String> {
    tokio::time::timeout(Duration::from_secs(15), async {
        while log.matching(needle).is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the pull never logged {needle}"));
    log.matching(needle)
}

/// Capture the pull's lines for as long as this value lives: the subscriber is
/// the runtime thread's default, which is the thread the task runs on.
struct CapturedLog {
    messages: CapturedMessages,
    _guard: tracing::dispatcher::DefaultGuard,
}

fn capture_log() -> CapturedLog {
    let messages = CapturedMessages::default();
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(messages.clone()));
    let guard = tracing::dispatcher::set_default(&dispatch);
    CapturedLog {
        messages,
        _guard: guard,
    }
}

/// The credentials of a pull belong to the source's origin. A playlist may
/// name another origin — an SSAI ad segment, a second CDN, a compromised edge
/// — and that origin gets none of them.
#[tokio::test]
async fn credentials_do_not_leave_the_source_origin() {
    let origin = Origin::new();
    let other = Origin::new();
    let other_base = other.spawn().await;
    origin.serve(
        "/live/index.m3u8",
        format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
             #EXTINF:0.5,\n{other_base}/ad/seg0.ts\n#EXTINF:0.5,\nseg1.ts\n"
        ),
    );
    origin.serve("/live/seg1.ts", ts_segment());
    other.serve("/ad/seg0.ts", ts_segment());
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
    let mut pull_source = source(
        format!("{base}/live/index.m3u8"),
        VariantRule::HighestBandwidth,
    );
    // A custom header is the case reqwest's own redirect guard misses, and
    // `Authorization` is the case it strips: neither may reach another origin.
    pull_source.auth = PullAuth::Headers {
        headers: vec![
            HeaderPair {
                name: "Authorization".to_string(),
                value: "Bearer origin-token".to_string(),
            },
            HeaderPair {
                name: "X-Api-Key".to_string(),
                value: "origin-key".to_string(),
            },
        ],
    };
    let task = spawn(engine.clone(), ingest, identity(), pull_source, limits());

    wait_for_request(&origin, "/live/seg1.ts").await;
    wait_for_request(&other, "/ad/seg0.ts").await;
    let own = crate::sync::lock(&origin.authorization).clone();
    assert!(
        own.iter().all(|value| value == "Bearer origin-token") && own.len() >= 2,
        "the source's own requests carry it: {own:?}"
    );
    let own_keys = crate::sync::lock(&origin.api_keys).clone();
    assert!(
        own_keys.iter().all(|value| value == "origin-key") && own_keys.len() >= 2,
        "{own_keys:?}"
    );
    assert!(
        crate::sync::lock(&other.authorization).is_empty(),
        "the other origin never sees Authorization"
    );
    assert!(
        crate::sync::lock(&other.api_keys).is_empty(),
        "the other origin never sees the custom header"
    );

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

/// A redirect out of the source's origin is not followed while a credential could
/// carries the credentials there — not even a custom header, which reqwest's
/// own cross-host guard leaves in place.
#[tokio::test]
async fn a_credential_less_redirect_to_another_origin_is_followed() {
    let origin = Origin::new();
    let other = Origin::new();
    let other_base = other.spawn().await;
    origin.serve("/live/index.m3u8", media_playlist(&["seg0.ts"]));
    origin.redirect("/live/seg0.ts", &format!("{other_base}/seg0.ts"));
    other.serve("/seg0.ts", ts_segment());
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
    let ring = engine.get_or_create_pipeline(PIPELINE_ID).await;
    let start = ring.get_write_idx();
    // No credentials to protect, and the source itself is on loopback, so the
    // hop passes both rules: a nearest-edge redirect works.
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

    wait_for_request(&other, "/seg0.ts").await;
    let packets = wait_for_media(&engine, start).await;
    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "the followed hop delivered the segment"
    );

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

#[tokio::test]
async fn a_redirect_out_of_the_source_origin_is_not_followed() {
    let origin = Origin::new();
    let other = Origin::new();
    let other_base = other.spawn().await;
    origin.serve("/live/index.m3u8", media_playlist(&["seg0.ts"]));
    origin.redirect("/live/seg0.ts", &format!("{other_base}/seg0.ts"));
    other.serve("/seg0.ts", ts_segment());
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
    let log = capture_log();
    let mut pull_source = source(
        format!("{base}/live/index.m3u8"),
        VariantRule::HighestBandwidth,
    );
    pull_source.auth = PullAuth::Bearer {
        token: "origin-token".to_string(),
    };
    let task = spawn(engine.clone(), ingest, identity(), pull_source, limits());

    wait_for_request(&origin, "/live/seg0.ts").await;
    let lines = wait_for_lines(&log.messages, "redirect the pull does not follow").await;
    assert_eq!(lines.len(), 1, "the retry says why: {lines:?}");
    assert_eq!(
        other.count("/seg0.ts"),
        0,
        "the redirect was refused, not followed"
    );
    assert!(
        crate::sync::lock(&other.authorization).is_empty(),
        "and the other origin saw no credential"
    );

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

// ---- the first cycle, an expired variant URL, and a stuck object ----------

/// A live window is not read from its start: the first cycle enters three
/// target durations from the end, so the segments before that are never
/// fetched, and the origin is not asked to backfill a DVR window.
#[tokio::test]
async fn a_live_window_is_entered_near_its_end() {
    let origin = Origin::new();
    origin.serve(
        "/live/index.m3u8",
        media_playlist(&[
            "seg0.ts", "seg1.ts", "seg2.ts", "seg3.ts", "seg4.ts", "seg5.ts", "seg6.ts", "seg7.ts",
            "seg8.ts", "seg9.ts",
        ]),
    );
    for sequence in 4..10 {
        origin.serve(&format!("/live/seg{sequence}.ts"), ts_segment());
    }
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

    // Half-second segments with a one-second target: three target durations
    // is six entries, so sequence 4 is the edge and 0..3 are never fetched.
    wait_for_request(&origin, "/live/seg9.ts").await;
    for sequence in 0..4 {
        assert_eq!(
            origin.count(&format!("/live/seg{sequence}.ts")),
            0,
            "the window's start (seg{sequence}) is left alone"
        );
    }
    assert_eq!(origin.count("/live/seg4.ts"), 1, "entry point");
    assert_eq!(origin.count("/live/seg9.ts"), 1);

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

/// A per-variant URL that a signed master handed out expires: the media
/// playlist starts answering 404, and the pull resolves the master again
/// instead of retrying a dead URL for ever.
#[tokio::test]
async fn an_expired_variant_url_resolves_the_master_again() {
    let origin = Origin::new();
    origin.serve(
        "/live/master.m3u8",
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=2000000\nhigh.m3u8?sig=1\n",
    );
    origin.serve_exact("/live/high.m3u8?sig=1", media_playlist(&["seg0.ts"]));
    origin.serve("/live/seg0.ts", ts_segment());
    origin.serve("/live/seg1.ts", ts_segment());
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
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
    // The signed URL expires and the master renews it: the old one now
    // answers 404, and the new one lists the next segment too.
    origin.serve(
        "/live/master.m3u8",
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=2000000\nhigh.m3u8?sig=2\n",
    );
    origin.unserve_exact("/live/high.m3u8?sig=1");
    origin.serve_exact(
        "/live/high.m3u8?sig=2",
        media_playlist(&["seg0.ts", "seg1.ts"]),
    );

    wait_for_request(&origin, "/live/high.m3u8?sig=2").await;
    wait_for_request(&origin, "/live/seg1.ts").await;
    assert_eq!(
        origin.count("/live/high.m3u8?sig=1"),
        2,
        "the URL's first, good fetch and the one 404 that sent the pull back to the \
         master — not the six attempts of a retry burst"
    );
    assert!(
        origin.count("/live/master.m3u8") >= 2,
        "the master is what the pull goes back to"
    );

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

/// A signed origin re-signs the same `EXT-X-MAP` URI on every playlist, and
/// the init is fetched once: the map's identity is scheme, host and path.
#[tokio::test]
async fn a_resigned_map_uri_does_not_refetch_the_init() {
    let origin = Origin::new();
    origin.serve(
        "/live/index.m3u8",
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-MAP:URI=\"init.mp4?sig=1\"\n#EXTINF:1.0,\nseg0.m4s\n",
    );
    origin.serve("/live/init.mp4", vec![0u8; 512]);
    origin.serve("/live/seg0.m4s", vec![0u8; 512]);
    origin.serve("/live/seg1.m4s", vec![0u8; 512]);
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

    wait_for_request(&origin, "/live/seg0.m4s").await;
    origin.serve(
        "/live/index.m3u8",
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-MAP:URI=\"init.mp4?sig=2\"\n#EXTINF:1.0,\nseg0.m4s\n#EXTINF:1.0,\nseg1.m4s\n",
    );
    wait_for_request(&origin, "/live/seg1.m4s").await;
    assert_eq!(
        origin.count("/live/init.mp4"),
        1,
        "a re-signed map URI is the same map, so the init is not fetched again"
    );

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

/// A segment that answers 404 does not stall the pull until it leaves the
/// window: after a few tries it is skipped, once, and the pull carries on.
#[tokio::test]
async fn a_missing_segment_is_skipped_after_bounded_tries() {
    let origin = Origin::new();
    // The listed segment carries a signature of its own, which the warning
    // about it must not print.
    origin.serve(
        "/live/index.m3u8",
        media_playlist(&["missing0.ts?hdnts=st=1~exp=2~hmac=SECRETSIG", "seg1.ts"]),
    );
    origin.serve("/live/seg1.ts", ts_segment());
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
    let ring = engine.get_or_create_pipeline(PIPELINE_ID).await;
    let start = ring.get_write_idx();
    let log = capture_log();
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

    wait_for_request(&origin, "/live/seg1.ts").await;
    let packets = wait_for_media(&engine, start).await;
    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "the segment after the missing one is published"
    );
    assert_eq!(
        origin.count("/live/missing0.ts"),
        SEGMENT_ATTEMPTS as usize,
        "the missing object is tried a bounded number of times"
    );
    let lines = log.messages.matching("HLS pull skipped a segment");
    assert_eq!(lines.len(), 1, "one warning names it: {lines:?}");
    assert!(lines[0].contains("sequence=0"), "{}", lines[0]);
    assert!(
        lines[0].contains("live/miss"),
        "the warning names the object: {}",
        lines[0]
    );
    assert!(
        !lines[0].contains("SECRETSIG") && !lines[0].contains("hdnts"),
        "and not its signature: {}",
        lines[0]
    );

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

/// A playlist whose every segment the assembler refuses says so once, not
/// once per segment: a ladder whose top variant is HEVC refuses for ever.
#[tokio::test]
async fn a_refused_segment_is_logged_once() {
    let origin = Origin::new();
    // A map that names an init which is not one: this is an fMP4 playlist,
    // and both of its segments are refused with the same reason.
    origin.serve(
        "/live/index.m3u8",
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:1.0,\nseg0.m4s?Policy=SECRETPOLICY\n\
         #EXTINF:1.0,\nseg1.m4s?Policy=SECRETPOLICY\n",
    );
    origin.serve("/live/init.mp4", vec![0u8; 512]);
    origin.serve("/live/seg0.m4s", vec![0u8; 512]);
    origin.serve("/live/seg1.m4s", vec![0u8; 512]);
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
    let log = capture_log();
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

    wait_for_request(&origin, "/live/seg1.m4s").await;
    // The next reload says the cycle that refused both segments is over.
    wait_for_requests(&origin, "/live/index.m3u8", 2).await;
    assert_eq!(origin.count("/live/seg0.m4s"), 1);
    assert_eq!(origin.count("/live/seg1.m4s"), 1);
    let lines = log.messages.matching("HLS pull segment refused");
    assert_eq!(lines.len(), 1, "one line for one reason: {lines:?}");
    assert!(
        !lines[0].contains("SECRETPOLICY"),
        "the refused segment's signature is dropped: {}",
        lines[0]
    );

    task.cancel();
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .expect("the pull task stops");
    engine.cancel_pipeline_input(INPUT_ID).await;
}

/// An encrypted or byte-range playlist cannot be fetched into this ingest, so
/// the pull refuses it: one clear reason, once, and no segment fetched.
#[tokio::test]
async fn an_encrypted_or_byte_range_playlist_is_refused_with_one_reason() {
    for (playlist, needle) in [
        (
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
             #EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"key.bin\"\n#EXTINF:1.0,\nseg0.ts\n",
            "SAMPLE-AES",
        ),
        (
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
             #EXT-X-BYTERANGE:1000@0\n#EXTINF:1.0,\nseg0.ts\n",
            "EXT-X-BYTERANGE",
        ),
    ] {
        let origin = Origin::new();
        origin.serve("/live/index.m3u8", playlist);
        origin.serve("/live/seg0.ts", ts_segment());
        let base = origin.spawn().await;

        let (engine, ingest) = test_ingest(limits());
        let log = capture_log();
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

        tokio::time::timeout(Duration::from_secs(15), async {
            while !task.is_finished() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a playlist the pull refuses ends the task");
        assert_eq!(task.end(), Some(PullEnd::Fatal));
        let lines = log.messages.matching("refuses");
        assert_eq!(lines.len(), 1, "{needle}: one reason, once: {lines:?}");
        assert!(lines[0].contains(needle), "{}", lines[0]);
        assert!(
            lines[0].contains("HLS pull stopped"),
            "the task says it stopped: {}",
            lines[0]
        );
        assert_eq!(
            origin.count("/live/seg0.ts"),
            0,
            "no segment of a refused playlist is fetched"
        );
        engine.cancel_pipeline_input(INPUT_ID).await;
    }
}

/// Userinfo in the source URL is refused: it is a password every `redact_url`
/// would print half of, and the credential choices are where it belongs.
#[tokio::test]
async fn a_source_url_with_userinfo_is_refused() {
    let (engine, ingest) = test_ingest(limits());
    let log = capture_log();
    let task = spawn(
        engine.clone(),
        ingest,
        identity(),
        source(
            "http://user:pw1234@127.0.0.1:1/live/index.m3u8".to_string(),
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
    .expect("a source with userinfo ends the task");
    assert_eq!(task.end(), Some(PullEnd::Fatal));
    let lines = log.messages.matching("userinfo");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("basic"), "{}", lines[0]);
    let all = log.messages.matching("").join("\n");
    assert!(
        !all.contains("pw1234"),
        "the password is never printed: {all}"
    );
    engine.cancel_pipeline_input(INPUT_ID).await;
}

// ---- cancellation --------------------------------------------------------

/// A cancelled task dispatches nothing: `HlsIngest::dispatch` would open a
/// session under an input the next source is about to own. Deterministic,
/// because the guard is what is called here — a cancellation that lands
/// between the last byte of a body and the dispatch cannot be produced from a
/// test without pausing inside the task's own poll.
#[tokio::test]
async fn a_cancelled_task_dispatches_nothing() {
    let origin = Origin::new();
    origin.serve("/live/seg0.ts", ts_segment());
    let base = origin.spawn().await;
    let (engine, ingest) = test_ingest(limits());

    let cancel = CancellationToken::new();
    let source_url = Url::parse(&format!("{base}/live/index.m3u8")).unwrap();
    let state = PullState {
        source_url: source_url.clone(),
        next_url: source_url.clone(),
        allow_private: true,
        scope: AuthScope::new(&source_url, RequestAuth::None),
        cancel: cancel.clone(),
        variant: VariantRule::HighestBandwidth,
        pipeline: authenticated(),
        stream_key: STREAM_KEY.to_string(),
        client_addr: "127.0.0.1".to_string(),
        next_sequence: None,
        last_media_sequence: None,
        current_map: None,
        segment_tries: HashMap::new(),
        last_refused: None,
        re_expired: false,
    };
    cancel.cancel();

    let playlist = state
        .dispatch_playlist(&ingest, b"#EXTM3U\n".to_vec())
        .await;
    assert!(
        matches!(playlist, Err(PullError::Cancelled)),
        "a cancelled task hands over no playlist"
    );
    assert!(
        !engine.input_has_session(PIPELINE_ID, INPUT_ID).await,
        "and opens no session"
    );

    let client = build_client(false, true);
    let url = Url::parse(&format!("{base}/live/seg0.ts")).unwrap();
    let object = state
        .fetch_object(
            &client,
            &ingest,
            &url,
            limits().max_segment_bytes,
            ObjectKind::TsSegment,
            "pull/s0.ts".to_string(),
        )
        .await;
    assert!(matches!(object, Err(PullError::Cancelled)));
    assert!(!engine.input_has_session(PIPELINE_ID, INPUT_ID).await);
    assert_eq!(
        origin.count("/live/seg0.ts"),
        0,
        "the request is not even sent"
    );
}

/// The same thing through the task: cancelled with a request in flight, it
/// joins without dispatching anything afterwards.
#[tokio::test]
async fn a_cancel_in_flight_dispatches_nothing_after_it() {
    let origin = Origin::new();
    // The listed segment is the one the origin never answers.
    origin.serve("/live/index.m3u8", media_playlist(&["../stall.m3u8"]));
    let base = origin.spawn().await;

    let (engine, ingest) = test_ingest(limits());
    let ring = engine.get_or_create_pipeline(PIPELINE_ID).await;
    let start = ring.get_write_idx();
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

    wait_for_request(&origin, "/stall.m3u8").await;
    task.cancel();
    tokio::time::timeout(Duration::from_secs(2), task.join())
        .await
        .expect("a cancelled pull joins promptly");
    assert_eq!(
        origin.count("/stall.m3u8"),
        1,
        "the in-flight request is not retried"
    );
    assert_eq!(
        ring.get_write_idx(),
        start,
        "nothing was dispatched after the cancel"
    );
    engine.cancel_pipeline_input(INPUT_ID).await;
}

/// Parsing a fetched playlist must not run on the worker the pull task runs
/// on: a playlist is capped at `max_playlist_bytes`, and parsing one that size
/// costs tens of milliseconds of CPU, which the API and every other protocol
/// share. The runtime here has a single worker, so a parse on it would delay
/// the timer below by the parse's own duration.
///
/// The playlist is far larger than production's 1 MiB cap to make the signal
/// unambiguous; the property under test is where the parse runs, not the cap.
#[tokio::test(flavor = "current_thread")]
async fn a_playlist_parse_does_not_block_the_pull_task_worker() {
    let origin = Origin::new();
    let mut text = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n");
    while text.len() < 4 * 1024 * 1024 {
        text.push_str("#EXTINF:0.5,\nseg.ts\n");
    }
    origin.serve("/live/index.m3u8", text.clone());
    let base = origin.spawn().await;
    let (engine, ingest) = test_ingest(HlsIngestLimits {
        max_playlist_bytes: 8 * 1024 * 1024,
        ..limits()
    });
    let task = spawn(
        engine.clone(),
        ingest,
        identity(),
        source(
            format!("{base}/live/index.m3u8"),
            VariantRule::HighestBandwidth,
        ),
        HlsIngestLimits {
            max_playlist_bytes: 8 * 1024 * 1024,
            ..limits()
        },
    );

    // Sample a 10 ms timer on the same runtime until the playlist has reached
    // the ingest, so the parse and the passes that follow it are inside the
    // measured span, and take the worst delay over it.
    let mut worst = Duration::ZERO;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut arrived = false;
    while tokio::time::Instant::now() < deadline {
        let started = std::time::Instant::now();
        tokio::time::sleep(Duration::from_millis(10)).await;
        worst = worst.max(started.elapsed());
        if engine.input_has_session(PIPELINE_ID, INPUT_ID).await {
            arrived = true;
            break;
        }
    }
    assert!(arrived, "the pulled playlist never reached the ingest");
    // Wide enough for a loaded host to deschedule the process, far below the
    // ~1 s an inline parse blocks the only worker for.
    assert!(
        worst < Duration::from_millis(200),
        "the pull blocked its worker for {worst:?}: a parse is running on it"
    );
    task.cancel();
    task.join().await;
}

// ---- the resolver guard ----------------------------------------------------

/// A *name* that resolves into a refused range is refused by the client's
/// resolver, so no request is issued: the literal guard sees only addresses,
/// and `localhost` stands here for any name an attacker points at an internal
/// address. The client connects to the addresses the resolver returned, so
/// the name cannot rebind between the check and the connect.
#[tokio::test]
async fn a_name_that_resolves_to_a_private_address_is_refused_before_any_request() {
    let origin = Origin::new();
    origin.serve("/live/index.m3u8", media_playlist(&["seg0.ts"]));
    let base = origin.spawn().await;
    let port = Url::parse(&base).unwrap().port().unwrap();
    let url = Url::parse(&format!("http://localhost:{port}/live/index.m3u8")).unwrap();
    let scope = AuthScope::new(&url, RequestAuth::new(&PullAuth::None).unwrap());

    let refused = super::fetch::fetch_small(&build_client(false, false), &url, &scope, 4096).await;
    match refused {
        Err(super::fetch::FetchError::PrivateTarget(reason)) => {
            assert!(reason.contains("localhost"), "{reason}")
        }
        other => panic!("a private name is refused by the resolver, got {other:?}"),
    }
    assert_eq!(
        origin.count("/live/index.m3u8"),
        0,
        "no request reaches a refused address"
    );
    assert!(
        matches!(
            PullError::from(super::fetch::FetchError::PrivateTarget(String::new())),
            PullError::Fatal(_)
        ),
        "a refused target is final, not retried every backoff"
    );

    // A pull whose own source is private may reach it.
    let allowed = super::fetch::fetch_small(&build_client(false, true), &url, &scope, 4096).await;
    assert!(
        allowed.is_ok(),
        "a private pull reaches its origin: {allowed:?}"
    );
    assert_eq!(origin.count("/live/index.m3u8"), 1);
}

/// A source whose name resolves into a refused range is a private pull.
#[tokio::test]
async fn a_source_named_into_a_private_range_is_a_private_pull() {
    let url = Url::parse("http://localhost:9/live/index.m3u8").unwrap();
    assert!(super::fetch::source_resolves_private(&url).await);
    let public = Url::parse("http://192.0.2.1/live/index.m3u8").unwrap();
    assert!(
        !super::fetch::source_resolves_private(&public).await,
        "a literal is judged by the literal guard, not resolved"
    );
}

// ---- HTTP Digest -----------------------------------------------------------

/// A local origin that challenges with Digest. It accepts an `Authorization:
/// Digest` naming its current nonce, and records every `Authorization` it
/// sees. The response hash itself is checked against the RFC examples in
/// `digest_tests`; this origin checks the exchange around it.
#[derive(Clone, Default)]
struct DigestOrigin {
    /// The `WWW-Authenticate` parameters after `Digest `, without the nonce.
    challenge: String,
    /// Nonces in the order the origin issues them: the first challenge uses
    /// the first, and a stale answer is told to move to the next.
    nonces: Vec<&'static str>,
    /// Every answer is refused without `stale`: the credentials are wrong.
    reject_all: bool,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

async fn digest_origin(
    axum::extract::State(origin): axum::extract::State<DigestOrigin>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let authorization = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let answered = {
        let mut seen = crate::sync::lock(&origin.seen);
        seen.push(authorization.clone());
        seen.iter()
            .filter(|value| value.starts_with("Digest "))
            .count()
    };
    let current = origin.nonces[answered.saturating_sub(1).min(origin.nonces.len() - 1)];
    let latest = *origin.nonces.last().unwrap();
    if !origin.reject_all
        && authorization.starts_with("Digest ")
        && authorization.contains(&format!("nonce=\"{latest}\""))
    {
        return (axum::http::StatusCode::OK, media_playlist(&["seg0.ts"])).into_response();
    }
    let stale = authorization.starts_with("Digest ") && !origin.reject_all && current != latest;
    let nonce = if stale {
        origin.nonces[answered.min(origin.nonces.len() - 1)]
    } else {
        current
    };
    let mut value = format!("Digest {}, nonce=\"{nonce}\"", origin.challenge);
    if stale {
        value.push_str(", stale=true");
    }
    (
        axum::http::StatusCode::UNAUTHORIZED,
        [
            (
                axum::http::header::WWW_AUTHENTICATE,
                "Basic realm=\"pull\"".to_string(),
            ),
            (axum::http::header::WWW_AUTHENTICATE, value),
        ],
    )
        .into_response()
}

async fn fetch_with_digest(
    origin: DigestOrigin,
) -> (Result<Vec<u8>, super::fetch::FetchError>, Vec<String>) {
    let app = axum::Router::new()
        .fallback(axum::routing::get(digest_origin))
        .with_state(origin.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = Url::parse(&format!("http://{address}/live/index.m3u8?token=1")).unwrap();
    let auth = RequestAuth::new(&PullAuth::Basic {
        username: "Mufasa".to_string(),
        password: "Circle of Life".to_string(),
    })
    .unwrap();
    let scope = AuthScope::new(&url, auth);
    let result = super::fetch::fetch_small(&build_client(true, true), &url, &scope, 4096).await;
    let seen = crate::sync::lock(&origin.seen).clone();
    (result, seen)
}

/// A `basic` credential answers a Digest challenge, as FFmpeg's client does,
/// with each supported algorithm and `qop`: the second request carries the
/// Digest answer for the challenged URI.
#[tokio::test]
async fn a_digest_challenge_is_answered_with_each_supported_algorithm() {
    for (challenge, algorithm, qop) in [
        (r#"realm="r", qop="auth", algorithm=MD5"#, "MD5", true),
        (
            r#"realm="r", qop="auth", algorithm=MD5-sess"#,
            "MD5-sess",
            true,
        ),
        (
            r#"realm="r", qop="auth,auth-int", algorithm=SHA-256"#,
            "SHA-256",
            true,
        ),
        (
            r#"realm="r", qop="auth", algorithm=SHA-256-sess"#,
            "SHA-256-sess",
            true,
        ),
        (r#"realm="r""#, "MD5", false),
    ] {
        let (result, seen) = fetch_with_digest(DigestOrigin {
            challenge: challenge.to_string(),
            nonces: vec!["n1"],
            ..DigestOrigin::default()
        })
        .await;
        assert!(result.is_ok(), "{challenge}: {result:?} after {seen:?}");
        assert_eq!(seen.len(), 2, "{challenge}: {seen:?}");
        assert!(
            seen[0].starts_with("Basic "),
            "the first request is as configured"
        );
        let answer = &seen[1];
        for field in [
            "username=\"Mufasa\"".to_string(),
            "realm=\"r\"".to_string(),
            "nonce=\"n1\"".to_string(),
            "uri=\"/live/index.m3u8?token=1\"".to_string(),
            format!("algorithm={algorithm},"),
        ] {
            assert!(
                answer.contains(&field),
                "{challenge}: {field} missing from {answer}"
            );
        }
        assert_eq!(answer.contains("qop=auth, nc=00000001"), qop, "{answer}");
    }
}

/// A `stale=true` challenge (the nonce expired, the credentials did not) is
/// answered once more with the new nonce, and its count starts again.
#[tokio::test]
async fn a_stale_nonce_is_answered_again_with_the_new_one() {
    let (result, seen) = fetch_with_digest(DigestOrigin {
        challenge: r#"realm="r", qop="auth""#.to_string(),
        nonces: vec!["n1", "n2"],
        ..DigestOrigin::default()
    })
    .await;
    assert!(result.is_ok(), "{result:?} after {seen:?}");
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(seen[1].contains("nonce=\"n1\""), "{}", seen[1]);
    assert!(seen[2].contains("nonce=\"n2\""), "{}", seen[2]);
    assert!(seen[2].contains("nc=00000001"), "{}", seen[2]);
}

/// Wrong credentials: the Digest answer is refused without `stale`, and the
/// `401` stands after one answer rather than being retried.
#[tokio::test]
async fn a_refused_digest_answer_is_not_repeated() {
    let (result, seen) = fetch_with_digest(DigestOrigin {
        challenge: r#"realm="r", qop="auth""#.to_string(),
        nonces: vec!["n1"],
        reject_all: true,
        ..DigestOrigin::default()
    })
    .await;
    assert!(
        matches!(result, Err(super::fetch::FetchError::Status(401))),
        "{result:?}"
    );
    assert_eq!(
        seen.len(),
        2,
        "one Digest answer, then the 401 stands: {seen:?}"
    );
}

/// A Digest challenge the pull cannot answer ends it with the reason.
#[tokio::test]
async fn an_unsupported_digest_challenge_is_refused_with_its_reason() {
    let (result, seen) = fetch_with_digest(DigestOrigin {
        challenge: r#"realm="r", algorithm=SHA-512-256"#.to_string(),
        nonces: vec!["n1"],
        ..DigestOrigin::default()
    })
    .await;
    match result {
        Err(super::fetch::FetchError::Auth(reason)) => {
            assert!(reason.contains("SHA-512-256"), "{reason}")
        }
        other => panic!("an unanswerable challenge is refused, got {other:?}"),
    }
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(matches!(
        PullError::from(super::fetch::FetchError::Auth(String::new())),
        PullError::Fatal(_)
    ));
}
