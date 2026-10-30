//! The pull registry's tests: starting, replacing and cancelling a pull task
//! from its row, and what a pull session does across a promotion.
//!
//! Kept apart from `registry_tests.rs`, which is near the source-audit line
//! cap; the shared helpers live there and are `pub(super)`.

use super::tests::{fixture, published, send_pull, wait_for_ring};
use super::*;
use crate::hls_ingest::test_support::{INPUT_ID, PIPELINE_ID, stalled_origin, test_ingest};
use crate::hls_pull::{PullIdentity, PullSource};
use restream_domain::domain::pipeline_input::{PullAuth, VariantRule};

fn pull_source(url: &str) -> PullSource {
    PullSource {
        url: url.to_string(),
        variant: VariantRule::HighestBandwidth,
        auth: PullAuth::None,
    }
}

fn pull_identity() -> PullIdentity {
    PullIdentity {
        pipeline_id: PIPELINE_ID.to_string(),
        input_id: "input-pull".to_string(),
        stream_key: "pull-key".to_string(),
        selected: true,
    }
}

/// The reconciler converges every pass, so starting a pull must be
/// idempotent: the same source is left running, a row edit (URL, rendition
/// or credential) replaces the task, and a cancel removes it.
#[tokio::test]
async fn a_pull_task_starts_once_per_source_and_is_replaced_when_the_row_changes() {
    let (_engine, ingest) = test_ingest(HlsIngestLimits::default());
    let (origin, stalled) = stalled_origin();
    let identity = pull_identity();
    let source = pull_source(&format!("http://{origin}/master.m3u8"));

    assert!(
        ingest.ensure_pull(identity.clone(), source.clone()).await,
        "the first pass starts the task"
    );
    assert!(
        !ingest.ensure_pull(identity.clone(), source.clone()).await,
        "a repeated pass leaves the running task alone"
    );
    assert!(ingest.pull_input_ids().contains(&identity.input_id));

    let mut changed = source.clone();
    changed.variant = VariantRule::LowestBandwidth;
    assert!(
        ingest.ensure_pull(identity.clone(), changed).await,
        "an edited row is a different origin and replaces the task"
    );

    assert!(ingest.cancel_pull(&identity.input_id).await);
    assert!(
        !ingest.cancel_pull(&identity.input_id).await,
        "cancelling twice reports that nothing ran"
    );
    assert!(ingest.pull_input_ids().is_empty());
    stalled.abort();
}

/// A pull session that registered while its input was an unselected backup
/// starts as a standby and forwards nothing, and promoting the input rebinds
/// that same session: its next segment reaches the ring, with no respawn and
/// no restart of the pull.
///
/// The `hls-pull-duty` harness doubted this and forced a respawn by editing
/// the input's `variantRule`; this is the evidence that the respawn is
/// unnecessary. A mutation removing `arm_for_promotion` in
/// `MediaEngine::select_pipeline_input` fails it.
#[tokio::test]
async fn a_pull_session_registered_unselected_forwards_after_promotion() {
    let (engine, ingest) = test_ingest(HlsIngestLimits::default());
    // A pull input is created as a backup, so its first session registers
    // with `selected: false` and its gate is a standby.
    let unselected = AuthenticatedPipeline {
        id: PIPELINE_ID.to_string(),
        input_id: INPUT_ID.to_string(),
        selected: false,
    };
    let segment = fixture();

    assert_eq!(
        send_pull(
            &ingest,
            &unselected,
            ObjectKind::TsSegment,
            "index0.ts",
            &segment
        )
        .await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send_pull(
            &ingest,
            &unselected,
            ObjectKind::Playlist,
            "index.m3u8",
            segment_playlist(0, &["index0.ts"]).as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    assert!(
        engine.input_has_session(PIPELINE_ID, INPUT_ID).await,
        "the session registered while unselected"
    );
    // Bounded: the publisher demuxes a segment in milliseconds, so an empty
    // ring after this long is the standby gate dropping, not slowness.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        published(&engine).await,
        0,
        "an unselected session forwards nothing"
    );

    assert!(
        engine.select_pipeline_input(PIPELINE_ID, INPUT_ID).await,
        "promotion finds the registered session"
    );
    assert_eq!(
        send_pull(
            &ingest,
            &unselected,
            ObjectKind::TsSegment,
            "index1.ts",
            &segment
        )
        .await,
        ObjectOutcome::Held
    );
    // The second playlist continues the sequence the first one ended at:
    // reusing sequence 0 would make this segment a duplicate or a rewind, and
    // the test would then pass or fail for reasons unrelated to promotion.
    assert_eq!(
        send_pull(
            &ingest,
            &unselected,
            ObjectKind::Playlist,
            "index.m3u8",
            segment_playlist(1, &["index1.ts"]).as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    wait_for_ring(&engine, 10, "media after promotion", |ring| {
        ring.get_write_idx() > 0
    })
    .await;
    engine.cancel_pipeline_input(INPUT_ID).await;
}

/// A one-segment playlist starting at `sequence`.
fn segment_playlist(sequence: u64, segments: &[&str]) -> String {
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:8\n#EXT-X-MEDIA-SEQUENCE:{sequence}\n"
    );
    for segment in segments {
        playlist.push_str(&format!("#EXTINF:8.0,\n{segment}\n"));
    }
    playlist
}

/// Every branch of the reap decision. A task that ended fatally must be
/// retried, one that ended at `EXT-X-ENDLIST` must never be, and a running
/// one must be left alone — restarting a finished playlist every pass would
/// replay a whole VOD into the ring.
#[test]
fn a_finished_pull_is_reaped_by_reason_and_restarted_only_after_its_backoff() {
    let now = std::time::Instant::now();
    let source = pull_source("http://origin.example/master.m3u8");
    let other = pull_source("http://origin.example/other.m3u8");
    let observe =
        |source: PullSource, finished: bool, end: Option<PullEnd>, retry_at| PullObservation {
            source,
            finished,
            end,
            retry_at,
        };

    assert_eq!(
        pull_action(None, &source, now),
        PullAction::Start { replaced: false },
        "nothing running: start"
    );
    assert_eq!(
        pull_action(
            Some(&observe(source.clone(), false, None, None)),
            &source,
            now
        ),
        PullAction::Leave,
        "a running task with the same source is left alone"
    );
    assert_eq!(
        pull_action(
            Some(&observe(source.clone(), false, None, None)),
            &other,
            now
        ),
        PullAction::Start { replaced: true },
        "a changed row replaces whatever runs, even mid-run"
    );
    assert_eq!(
        pull_action(
            Some(&observe(source.clone(), true, Some(PullEnd::Ended), None)),
            &source,
            now
        ),
        PullAction::Leave,
        "an ENDLIST source is complete and must not be replayed"
    );
    assert_eq!(
        pull_action(
            Some(&observe(source.clone(), true, Some(PullEnd::Fatal), None)),
            &source,
            now
        ),
        PullAction::WaitToRetry,
        "the first fatal end records a deadline instead of restarting at once"
    );
    assert_eq!(
        pull_action(
            Some(&observe(
                source.clone(),
                true,
                Some(PullEnd::Fatal),
                Some(now + std::time::Duration::from_secs(30))
            )),
            &source,
            now
        ),
        PullAction::WaitToRetry,
        "a deadline still ahead keeps waiting"
    );
    assert_eq!(
        pull_action(
            Some(&observe(
                source.clone(),
                true,
                Some(PullEnd::Fatal),
                Some(now - std::time::Duration::from_secs(1))
            )),
            &source,
            now
        ),
        PullAction::Start { replaced: false },
        "a deadline in the past restarts the same source"
    );
    assert_eq!(
        pull_action(
            Some(&observe(source.clone(), true, None, None)),
            &source,
            now
        ),
        PullAction::WaitToRetry,
        "a task that ended without recording why panicked: record a deadline instead of \
         leaving the row's pull dead"
    );
    assert_eq!(
        pull_action(
            Some(&observe(
                source.clone(),
                true,
                None,
                Some(now - std::time::Duration::from_secs(1))
            )),
            &source,
            now
        ),
        PullAction::Start { replaced: false },
        "a panicked task is restarted once its backoff has elapsed, keeping its session"
    );
    assert_eq!(
        pull_action(
            Some(&observe(
                source.clone(),
                true,
                Some(PullEnd::Ended),
                Some(now)
            )),
            &other,
            now
        ),
        PullAction::Start { replaced: true },
        "an edited row starts even when the old source had finished"
    );
}

/// A source change replaces the task *and* ends the session the old source
/// opened: a new origin numbers its media sequence independently, so the old
/// ordering state would classify its first segments as duplicates or hold
/// them until the session idled out.
#[tokio::test]
async fn a_source_change_ends_the_session_the_old_source_opened() {
    let (engine, ingest) = test_ingest(HlsIngestLimits::default());
    let (origin, stalled) = stalled_origin();
    let identity = pull_identity();
    let source = pull_source(&format!("http://{origin}/master.m3u8"));
    assert!(ingest.ensure_pull(identity.clone(), source.clone()).await);

    // The first source's session, with one segment through it.
    let pipeline = AuthenticatedPipeline {
        id: PIPELINE_ID.to_string(),
        input_id: identity.input_id.clone(),
        selected: true,
    };
    assert_eq!(
        send_pull(
            &ingest,
            &pipeline,
            ObjectKind::TsSegment,
            "index0.ts",
            &fixture()
        )
        .await,
        ObjectOutcome::Held
    );
    assert!(
        engine
            .input_has_session(PIPELINE_ID, &identity.input_id)
            .await,
        "the first source opened a session"
    );

    // The row now names a different origin.
    let mut changed = source.clone();
    changed.url = format!("http://{origin}/other.m3u8");
    assert!(ingest.ensure_pull(identity.clone(), changed).await);

    let ended = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while engine
            .input_has_session(PIPELINE_ID, &identity.input_id)
            .await
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        ended.is_ok(),
        "the replaced task must end the session it inherited, or the new origin's \
         sequence numbering is judged against the old one"
    );

    // The new source opens a fresh session and its media publishes.
    assert_eq!(
        send_pull(
            &ingest,
            &pipeline,
            ObjectKind::TsSegment,
            "index0.ts",
            &fixture()
        )
        .await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send_pull(
            &ingest,
            &pipeline,
            ObjectKind::Playlist,
            "index.m3u8",
            segment_playlist(0, &["index0.ts"]).as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    wait_for_ring(&engine, 10, "the new source's media", |ring| {
        ring.get_write_idx() > 0
    })
    .await;

    ingest.cancel_all_pulls();
    stalled.abort();
}

/// A cancel must ask the task to stop *before* it waits for the task, and the
/// wait must not reach the origin. The origin here accepts the connection and
/// never answers, so a task that was waited for before it was cancelled would
/// park for the request timeout (20 s) inside the reconcile pass; the task's
/// cancel branch is first in its `select!`, so asking first costs one poll.
/// A mutation that joins before it cancels fails the elapsed bound.
#[tokio::test]
async fn a_cancel_asks_the_task_to_stop_before_it_waits_for_it() {
    let (_engine, ingest) = test_ingest(HlsIngestLimits::default());
    let (origin, stalled) = stalled_origin();
    let identity = pull_identity();
    let source = pull_source(&format!("http://{origin}/master.m3u8"));
    assert!(ingest.ensure_pull(identity.clone(), source.clone()).await);
    // Let the task reach its first request, which the origin never answers.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    // Where the task records how it ended, read through the map it lives in.
    let end = {
        let pulls = restream_sync::lock::lock(&ingest.pulls);
        pulls
            .get(&identity.input_id)
            .expect("the pull task is registered")
            .task
            .end_state()
    };

    let started = std::time::Instant::now();
    assert!(ingest.cancel_pull(&identity.input_id).await);
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "the cancel waited for the parked request instead of the task's cancel: {elapsed:?}"
    );
    // The task recorded how it ended before the cancel returned, which is the
    // join: a cancel that only asked the task to stop would return with the
    // task still parked and its end still unset.
    assert!(
        restream_sync::lock::lock(&end).is_some(),
        "the task had not stopped when the cancel returned"
    );
    assert!(ingest.pull_input_ids().is_empty());
    stalled.abort();
}

/// A reconcile pass stops every stale pull in one call, and clears every
/// input's applied init whether or not its pull was still running: a session
/// that ended earlier leaves the init it applied behind, and nothing else
/// clears it.
#[tokio::test]
async fn a_batch_cancel_stops_every_pull_and_clears_every_applied_init() {
    let (_engine, ingest) = test_ingest(HlsIngestLimits::default());
    let (origin, stalled) = stalled_origin();
    let mut ids = Vec::new();
    for index in 0..3 {
        let mut identity = pull_identity();
        identity.input_id = format!("input-pull-{index}");
        let source = pull_source(&format!("http://{origin}/master.m3u8"));
        assert!(
            ingest.ensure_pull(identity.clone(), source).await,
            "each pass starts a pull for a row it has not seen"
        );
        ids.push(identity.input_id);
    }
    // An input whose pull has already ended: its init is still cleared.
    let gone = "input-pull-gone".to_string();
    let key = ingest.key_state(&gone);
    restream_sync::lock::lock(&key.init).set(
        0,
        AppliedInit {
            uri: "init.mp4".to_string(),
            bytes: bytes::Bytes::from_static(b"init"),
        },
    );
    ids.push(gone);

    let stopped = ingest.cancel_pulls(&ids).await;
    assert_eq!(
        stopped.len(),
        3,
        "every running pull is reported stopped: {stopped:?}"
    );
    assert!(
        ingest.pull_input_ids().is_empty(),
        "no pull is left running"
    );
    assert!(
        restream_sync::lock::lock(&key.init).is_empty(),
        "an input with no pull running still has its applied init cleared"
    );
    assert!(
        ingest.cancel_pulls(&ids).await.is_empty(),
        "a second pass reports nothing running"
    );
    stalled.abort();
}
