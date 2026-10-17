//! The session registry's tests: per-key upload slots and the shared
//! budget, session start and restart, and the fMP4 objects a session
//! routes to its ordering state.

use super::*;
use crate::media::hls_ingest::test_support::{
    INPUT_ID, IndexedKeys, PIPELINE_ID, RevocableKey, STREAM_KEY, authenticated, emptied_mdat,
    fmp4_hevc_init, fmp4_playlist, fmp4_video_objects, test_ingest, test_ingest_with,
};

/// A session that ended between lookup and send gets exactly one
/// restart, with the object already read.
#[tokio::test]
async fn an_object_for_a_session_that_just_ended_restarts_it_once() {
    let (engine, ingest) = test_ingest(HlsIngestLimits::default());
    let (stale, receiver) = mpsc::channel(OBJECT_QUEUE);
    drop(receiver);
    crate::sync::lock(&ingest.sessions).insert(
        INPUT_ID.to_string(),
        SessionHandle {
            attempt_id: u64::MAX,
            objects: stale,
        },
    );
    let mut body = ingest
        .object_body(ObjectKind::TsSegment, None, INPUT_ID)
        .await
        .unwrap();
    body.push(b"segment").await.unwrap();

    let outcome = ingest
        .dispatch(
            &authenticated(),
            STREAM_KEY,
            "127.0.0.1:9",
            DispatchOrigin::Push,
            ObjectKind::TsSegment,
            "index0.ts".to_string(),
            body,
        )
        .await;

    assert_eq!(outcome, Ok(ObjectOutcome::Held));
    let attempt = crate::sync::lock(&ingest.sessions)[INPUT_ID].attempt_id;
    assert_ne!(attempt, u64::MAX, "the stale handle was replaced");
    assert!(engine.input_has_session(PIPELINE_ID, INPUT_ID).await);
    engine.cancel_pipeline_input(INPUT_ID).await;
}

async fn send(
    ingest: &Arc<HlsIngest>,
    kind: ObjectKind,
    path: &str,
    bytes: &[u8],
) -> ObjectOutcome {
    send_to(ingest, &authenticated(), kind, path, bytes).await
}

/// Send through the pull origin, as the puller does. A pull session records
/// `hls-pull`, and a pulled object must reach the same assembler a pushed one
/// does.
pub(super) async fn send_pull(
    ingest: &Arc<HlsIngest>,
    pipeline: &AuthenticatedPipeline,
    kind: ObjectKind,
    path: &str,
    bytes: &[u8],
) -> ObjectOutcome {
    send_from(ingest, pipeline, DispatchOrigin::Pull, kind, path, bytes).await
}

async fn send_to(
    ingest: &Arc<HlsIngest>,
    pipeline: &AuthenticatedPipeline,
    kind: ObjectKind,
    path: &str,
    bytes: &[u8],
) -> ObjectOutcome {
    send_from(ingest, pipeline, DispatchOrigin::Push, kind, path, bytes).await
}

/// The same, presenting `stream_key`: a session opens only for a key that
/// resolves to `pipeline`'s own input, so a test that publishes from a second
/// input presents that input's key.
async fn send_as(
    ingest: &Arc<HlsIngest>,
    pipeline: &AuthenticatedPipeline,
    stream_key: &str,
    kind: ObjectKind,
    path: &str,
    bytes: &[u8],
) -> ObjectOutcome {
    send_keyed(
        ingest,
        pipeline,
        stream_key,
        DispatchOrigin::Push,
        kind,
        path,
        bytes,
    )
    .await
}

async fn send_from(
    ingest: &Arc<HlsIngest>,
    pipeline: &AuthenticatedPipeline,
    origin: DispatchOrigin,
    kind: ObjectKind,
    path: &str,
    bytes: &[u8],
) -> ObjectOutcome {
    send_keyed(ingest, pipeline, STREAM_KEY, origin, kind, path, bytes).await
}

async fn send_keyed(
    ingest: &Arc<HlsIngest>,
    pipeline: &AuthenticatedPipeline,
    stream_key: &str,
    origin: DispatchOrigin,
    kind: ObjectKind,
    path: &str,
    bytes: &[u8],
) -> ObjectOutcome {
    let mut body = ingest
        .object_body(kind, None, &pipeline.input_id)
        .await
        .unwrap();
    body.push(bytes).await.unwrap();
    ingest
        .dispatch(
            pipeline,
            stream_key,
            "127.0.0.1:9",
            origin,
            kind,
            path.to_string(),
            body,
        )
        .await
        .unwrap()
}

pub(super) fn fixture() -> Vec<u8> {
    std::fs::read(crate::test_fixtures::canonical_h264_ts_fixture().unwrap()).unwrap()
}

/// Wait until the pipeline's current ring satisfies `predicate`, or fail
/// the test after `secs`. The engine can swap the ring after a probe, so
/// the buffer is re-read every pass.
pub(super) async fn wait_for_ring(
    engine: &MediaEngine,
    secs: u64,
    what: &str,
    mut predicate: impl FnMut(&crate::media::ring_buffer::RingBuffer) -> bool,
) {
    tokio::time::timeout(std::time::Duration::from_secs(secs), async {
        loop {
            let ring = engine.get_or_create_pipeline(PIPELINE_ID).await;
            if predicate(&ring) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// How many packets the pipeline's current ring holds.
pub(super) async fn published(engine: &MediaEngine) -> usize {
    engine
        .get_or_create_pipeline(PIPELINE_ID)
        .await
        .get_write_idx()
}

async fn wait_until_unregistered(engine: &MediaEngine) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while engine.input_has_session(PIPELINE_ID, INPUT_ID).await {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("HLS session unregistered");
}

/// How many packets one pipeline's current ring holds.
async fn ring_write_idx(engine: &MediaEngine, pipeline_id: &str) -> usize {
    engine
        .get_or_create_pipeline(pipeline_id)
        .await
        .get_write_idx()
}

/// Wait until `input_id`'s session on `pipeline_id` is gone.
async fn wait_until_session_gone(engine: &MediaEngine, pipeline_id: &str, input_id: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while engine.input_has_session(pipeline_id, input_id).await {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("HLS session unregistered");
}

/// The init an encoder uploads once keeps a later session publishing:
/// after an idle end, a new session resumes with the next segment and
/// the playlist, and the media still reaches the ring.
#[tokio::test]
async fn media_keeps_flowing_after_an_idle_end_without_reuploading_the_init() {
    let (engine, ingest) = test_ingest(HlsIngestLimits {
        idle_floor: std::time::Duration::from_millis(150),
        ..HlsIngestLimits::default()
    });
    let (init, segments) = fmp4_video_objects(2, 4);

    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "init.mp4", &init).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "index0.m4s", &segments[0]).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            fmp4_playlist(0, &["index0.m4s"], false).as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    wait_for_ring(&engine, 10, "the first segment in the ring", |ring| {
        ring.get_write_idx() > 0
    })
    .await;
    wait_until_unregistered(&engine).await;

    // The encoder's next upload: no init this time (FFmpeg uploads it
    // once per run), and the stream continues.
    let before = published(&engine).await;
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "index1.m4s", &segments[1]).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            fmp4_playlist(1, &["index1.m4s"], true).as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    wait_for_ring(&engine, 10, "the resumed segment in the ring", |ring| {
        ring.get_write_idx() > before
    })
    .await;
    wait_until_unregistered(&engine).await;
    let phase = engine
        .recent_ingest_outcome(PIPELINE_ID)
        .await
        .and_then(|outcome| outcome.failure_phase);
    assert_eq!(phase.as_deref(), Some("eof"));
}

/// A refused init must void the map: FFmpeg uploads `init.mp4` on every
/// run and never reads the refusal, so leaving the previous run's init
/// in place would demux the new run's segments with it.
#[tokio::test]
async fn a_refused_init_voids_the_map_and_its_segments_do_not_publish() {
    let (engine, ingest) = test_ingest(HlsIngestLimits {
        idle_floor: std::time::Duration::from_millis(300),
        ..HlsIngestLimits::default()
    });
    let (init, segments) = fmp4_video_objects(2, 4);

    // The first run is H.264 and publishes.
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "init.mp4", &init).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "index0.m4s", &segments[0]).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            fmp4_playlist(0, &["index0.m4s"], false).as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    wait_for_ring(&engine, 10, "the first run in the ring", |ring| {
        ring.get_write_idx() > 0
    })
    .await;

    // The operator switches the key to HEVC: the same `init.mp4` path
    // now carries `hvc1`, which is refused.
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "init.mp4", &fmp4_hevc_init()).await,
        ObjectOutcome::Unsupported("HEVC fMP4 ingest is not implemented".to_string())
    );

    // The next run's media must not be demuxed with the old init: it
    // waits for an init that cannot come, and nowhere is it published.
    let before = published(&engine).await;
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "index1.m4s", &segments[1]).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            fmp4_playlist(1, &["index1.m4s"], true).as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    // Two target durations: long enough that a stale init would have
    // published the segment already.
    tokio::time::sleep(std::time::Duration::from_millis(2_200)).await;
    assert_eq!(
        published(&engine).await,
        before,
        "a refused init must not leave the previous run's init in place"
    );
    engine.cancel_pipeline_input(INPUT_ID).await;
}

/// A refused init must leave no seed behind: the next session (after the
/// encoder's idle end) uploads media and a playlist but no init, and
/// nothing may publish.
#[tokio::test]
async fn a_refused_init_leaves_no_seed_for_the_next_session() {
    let (engine, ingest) = test_ingest(HlsIngestLimits {
        idle_floor: std::time::Duration::from_millis(200),
        ..HlsIngestLimits::default()
    });
    let (init, segments) = fmp4_video_objects(2, 4);
    // A first run publishes and so fills the key's init slot.
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "init.mp4", &init).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "index0.m4s", &segments[0]).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            fmp4_playlist(0, &["index0.m4s"], false).as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    wait_for_ring(&engine, 10, "the first run in the ring", |ring| {
        ring.get_write_idx() > 0
    })
    .await;
    // The encoder switches to HEVC under the same init path: refused.
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "init.mp4", &fmp4_hevc_init()).await,
        ObjectOutcome::Unsupported("HEVC fMP4 ingest is not implemented".to_string())
    );
    wait_until_unregistered(&engine).await;

    // The next session seeds from the key's init slot: the refusal must
    // have emptied it, so these objects cannot publish.
    let before = published(&engine).await;
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "index1.m4s", &segments[1]).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            fmp4_playlist(1, &["index1.m4s"], true).as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    assert_eq!(
        published(&engine).await,
        before,
        "a refused init must leave no seed for the next session"
    );
    wait_until_unregistered(&engine).await;
}

/// `EXT-X-ENDLIST` ends the run for good, so the key's init goes
/// with it: the next session must upload its own.
#[tokio::test]
async fn an_ended_run_leaves_no_init_seed_for_the_next_session() {
    let (engine, ingest) = test_ingest(HlsIngestLimits::default());
    let (init, segments) = fmp4_video_objects(2, 4);
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "init.mp4", &init).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "index0.m4s", &segments[0]).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            fmp4_playlist(0, &["index0.m4s"], true).as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    wait_for_ring(&engine, 10, "the first run in the ring", |ring| {
        ring.get_write_idx() > 0
    })
    .await;
    wait_until_unregistered(&engine).await;

    // A new run, with no init of its own: it must not publish.
    let before = published(&engine).await;
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "index1.m4s", &segments[1]).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            fmp4_playlist(1, &["index1.m4s"], true).as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    assert_eq!(
        published(&engine).await,
        before,
        "an ended run must leave no seed for the next session"
    );
    wait_until_unregistered(&engine).await;
}

/// The init a key applied is the key's own state, not a bounded shared
/// cache. 65 keys each publish their first segment, then the first key's
/// session ends and the encoder resumes without re-uploading its init: the
/// segment still publishes. A 64-entry FIFO cache evicted the first key's
/// init when the 65th key applied one, and its segments then waited for an
/// init that never came and were skipped forever — a publisher's silent
/// death, which is what the init is kept across sessions for.
#[tokio::test]
async fn more_keys_than_a_shared_cache_would_hold_do_not_evict_the_first_init() {
    const KEYS: u64 = 65;
    let (engine, ingest) = test_ingest_with(
        HlsIngestLimits {
            idle_floor: std::time::Duration::from_millis(150),
            ..HlsIngestLimits::default()
        },
        Arc::new(IndexedKeys),
    );
    let (init, segments) = fmp4_video_objects(2, 4);
    let key = |index: u64| AuthenticatedPipeline {
        id: format!("pipe-{index}"),
        input_id: format!("input-{index}"),
        selected: true,
    };

    for index in 0..KEYS {
        let pipeline = key(index);
        let stream_key = format!("key-{index}");
        assert_eq!(
            send_as(
                &ingest,
                &pipeline,
                &stream_key,
                ObjectKind::Fmp4,
                "init.mp4",
                &init
            )
            .await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send_as(
                &ingest,
                &pipeline,
                &stream_key,
                ObjectKind::Fmp4,
                "index0.m4s",
                &segments[0]
            )
            .await,
            ObjectOutcome::Held
        );
        assert_eq!(
            send_as(
                &ingest,
                &pipeline,
                &stream_key,
                ObjectKind::Playlist,
                "index.m3u8",
                fmp4_playlist(0, &["index0.m4s"], false).as_bytes(),
            )
            .await,
            ObjectOutcome::Stored
        );
    }
    // Every key applied its init (each published its first segment).
    for index in 0..KEYS {
        let pipeline_id = key(index).id;
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while ring_write_idx(&engine, &pipeline_id).await == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("key {index} never published its first segment"));
    }

    // The first key's session ends (an idle timeout in production, a
    // cancelled attempt here) and the encoder resumes with the next
    // segment — and no init of its own, as FFmpeg uploads it once per run.
    engine.cancel_pipeline_input(&key(0).input_id).await;
    wait_until_session_gone(&engine, &key(0).id, &key(0).input_id).await;
    let before = ring_write_idx(&engine, &key(0).id).await;
    let pipeline = key(0);
    assert_eq!(
        send_as(
            &ingest,
            &pipeline,
            "key-0",
            ObjectKind::Fmp4,
            "index1.m4s",
            &segments[1]
        )
        .await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send_as(
            &ingest,
            &pipeline,
            "key-0",
            ObjectKind::Playlist,
            "index.m3u8",
            fmp4_playlist(1, &["index1.m4s"], true).as_bytes(),
        )
        .await,
        ObjectOutcome::Stored
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while ring_write_idx(&engine, &pipeline.id).await <= before {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the first key's init must survive the other keys' uploads");

    for index in 0..KEYS {
        engine.cancel_pipeline_input(&key(index).input_id).await;
    }
}

/// A segment whose bytes cannot be demuxed is dropped: the session stays
/// up and the segment after it publishes.
#[tokio::test]
async fn a_segment_that_fails_demux_is_skipped_and_the_next_one_publishes() {
    let (engine, ingest) = test_ingest(HlsIngestLimits {
        idle_floor: std::time::Duration::from_millis(300),
        ..HlsIngestLimits::default()
    });
    let (init, segments) = fmp4_video_objects(2, 4);

    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "init.mp4", &init).await,
        ObjectOutcome::Held
    );
    let broken = emptied_mdat(&segments[0]);
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "index0.m4s", &broken).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(&ingest, ObjectKind::Fmp4, "index1.m4s", &segments[1]).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            fmp4_playlist(0, &["index0.m4s", "index1.m4s"], true).as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    wait_for_ring(&engine, 10, "the segment after the broken one", |ring| {
        ring.get_write_idx() > 0
    })
    .await;
    wait_until_unregistered(&engine).await;
    let phase = engine
        .recent_ingest_outcome(PIPELINE_ID)
        .await
        .and_then(|outcome| outcome.failure_phase);
    assert_eq!(phase.as_deref(), Some("eof"), "the session kept going");
}

/// A key at its buffer share is throttled, not refused: its next body
/// waits until the segment being published returns its bytes, and a
/// second key publishes meanwhile. The wait is exercised on the body
/// here; over a real socket it becomes the encoder's write blocking on
/// a full TCP window, which no test covers yet.
#[tokio::test]
async fn a_key_at_its_share_is_throttled_while_another_key_publishes() {
    let (engine, ingest) = test_ingest_with(
        HlsIngestLimits {
            key_buffer_bytes: 8_000_000,
            ..HlsIngestLimits::default()
        },
        Arc::new(IndexedKeys),
    );
    let segment = fixture();
    let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:8\n#EXTINF:8.0,\nindex0.ts\n";
    assert_eq!(
        send(&ingest, ObjectKind::TsSegment, "index0.ts", &segment).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            playlist.as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );

    // index0 (6.7 MB) publishes over 8 s and holds most of the 8 MB share.
    let started = std::time::Instant::now();
    let mut next = ingest
        .object_body(ObjectKind::TsSegment, None, INPUT_ID)
        .await
        .unwrap();
    let throttled = tokio::spawn(async move {
        let pushed = next.push(&fixture()).await;
        (pushed.is_ok(), started.elapsed())
    });

    let other = AuthenticatedPipeline {
        id: "pipe-1".to_string(),
        input_id: "input-1".to_string(),
        selected: true,
    };
    let other_key = "key-1";
    let other_ring = engine.get_or_create_pipeline(&other.id).await;
    let other_playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1.0,\nindex0.ts\n";
    let other_segment = &segment[..188 * 2_000];
    assert_eq!(
        send_as(
            &ingest,
            &other,
            other_key,
            ObjectKind::TsSegment,
            "index0.ts",
            other_segment
        )
        .await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send_as(
            &ingest,
            &other,
            other_key,
            ObjectKind::Playlist,
            "index.m3u8",
            other_playlist.as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while other_ring.get_write_idx() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the other key publishes");
    assert!(
        !throttled.is_finished(),
        "the capped key's body waits for its share"
    );

    let (pushed, waited) = throttled.await.unwrap();
    assert!(pushed, "the capped key's body is read, never refused");
    assert!(
        waited >= std::time::Duration::from_secs(5),
        "the body waited only {waited:?} for the share"
    );
    engine.cancel_pipeline_input(INPUT_ID).await;
    engine.cancel_pipeline_input(&other.input_id).await;
}

/// Share claims of one key are serialized: a chunked body holds the
/// key's claim lock until it is complete, so a second chunked body
/// waits without a fragment and the first can still grow. Without the
/// lock the second would hold 4 MiB and the first could not claim its
/// next step.
#[tokio::test]
async fn two_chunked_bodies_of_one_key_never_both_hold_a_share_fragment() {
    const MIB: usize = 1024 * 1024;
    let (_engine, ingest) = test_ingest(HlsIngestLimits {
        key_buffer_bytes: 10 * MIB,
        ..HlsIngestLimits::default()
    });
    let mut first = ingest
        .object_body(ObjectKind::TsSegment, None, INPUT_ID)
        .await
        .unwrap();
    first.push(&[0x47; 1024]).await.unwrap();

    let mut second = ingest
        .object_body(ObjectKind::TsSegment, None, INPUT_ID)
        .await
        .unwrap();
    let waiting = tokio::spawn(async move {
        let pushed = second.push(&[0x47; 1024]).await.is_ok();
        (pushed, second)
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !waiting.is_finished(),
        "the second body waits for the first"
    );

    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        first.push(&vec![0x47; 5 * MIB]),
    )
    .await
    .expect("the first body grows past its first claim")
    .unwrap();
    drop(first);
    let (pushed, _second) = tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
        .await
        .expect("the second body proceeds once the first is gone")
        .unwrap();
    assert!(pushed);
}

/// Released media waits to be published only up to two target
/// durations: past that the session stops taking objects, so a
/// faster-than-real-time encoder's next reply waits, though the bytes
/// (6.7 MB) are far below the byte ceiling.
#[tokio::test]
async fn the_outbox_holds_two_target_durations_of_media() {
    let (engine, ingest) = test_ingest(HlsIngestLimits::default());
    let fixture = fixture();
    let slice = fixture.len() / 188 / 6 * 188;
    let mut playlist = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:1\n");
    for index in 0..6 {
        let end = if index == 5 {
            fixture.len()
        } else {
            (index + 1) * slice
        };
        let path = format!("index{index}.ts");
        let bytes = &fixture[index * slice..end];
        assert_eq!(
            send(&ingest, ObjectKind::TsSegment, &path, bytes).await,
            ObjectOutcome::Held
        );
        playlist.push_str(&format!("#EXTINF:1.0,\n{path}\n"));
    }
    // All six (6 s listed) are released at once.
    assert_eq!(
        send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            playlist.as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );

    let asked = std::time::Instant::now();
    let outcome = send(
        &ingest,
        ObjectKind::Playlist,
        "index.m3u8",
        playlist.as_bytes(),
    )
    .await;
    let waited = asked.elapsed();
    assert_eq!(outcome, ObjectOutcome::Stored);
    // Taken once under 2 s remain: five slices of 1.33 s real media.
    assert!(
        waited >= std::time::Duration::from_secs(4),
        "the next object was taken after {waited:?}"
    );
    engine.cancel_pipeline_input(INPUT_ID).await;
}

/// A whole segment released at once reaches the ring at media pace, not
/// in one burst: the wall time from its first to its last published
/// packet covers most of its media duration.
#[tokio::test]
async fn a_released_segment_reaches_the_ring_at_media_pace() {
    let (engine, ingest) = test_ingest(HlsIngestLimits::default());
    let ring = engine.get_or_create_pipeline(PIPELINE_ID).await;
    let start = ring.get_write_idx();
    let fixture = crate::test_fixtures::canonical_h264_ts_fixture().unwrap();
    let segment = std::fs::read(fixture).unwrap();
    let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:8\n#EXTINF:8.0,\nindex0.ts\n#EXT-X-ENDLIST\n";

    assert_eq!(
        send(&ingest, ObjectKind::TsSegment, "index0.ts", &segment).await,
        ObjectOutcome::Held
    );
    assert_eq!(
        send(
            &ingest,
            ObjectKind::Playlist,
            "index.m3u8",
            playlist.as_bytes()
        )
        .await,
        ObjectOutcome::Stored
    );

    // The receive path never waits on pacing: the next upload is
    // answered while the first segment is still being published.
    let asked = std::time::Instant::now();
    assert_eq!(
        send(
            &ingest,
            ObjectKind::TsSegment,
            "index1.ts",
            &segment[..18_800]
        )
        .await,
        ObjectOutcome::Held
    );
    let replied = std::time::Instant::now();

    let mut first = None;
    let mut last = None;
    let mut seen = start;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while engine.input_has_session(PIPELINE_ID, INPUT_ID).await {
        assert!(
            std::time::Instant::now() < deadline,
            "the session never ended"
        );
        let written = ring.get_write_idx();
        if written > seen {
            let now = std::time::Instant::now();
            first.get_or_insert(now);
            last = Some(now);
            seen = written;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let dts: Vec<i64> = (start..ring.get_write_idx())
        .filter_map(|index| ring.read_at(index))
        .map(|packet| packet.dts)
        .collect();
    let media_ms = dts.iter().max().unwrap() - dts.iter().min().unwrap();
    let wall_ms = (last.unwrap() - first.unwrap()).as_millis();
    assert!(media_ms > 7_000, "the segment holds {media_ms} ms of media");
    assert!(
        replied - asked < std::time::Duration::from_millis(500) && last.unwrap() > replied,
        "the second upload was answered after {:?}, publication ended {:?} after it",
        replied - asked,
        last.unwrap().saturating_duration_since(replied)
    );
    assert!(
        wall_ms * 10 >= u128::try_from(media_ms).unwrap() * 8,
        "{media_ms} ms of media reached the ring in {wall_ms} ms"
    );
    assert!(
        wall_ms <= u128::try_from(media_ms).unwrap() + 2_000,
        "pacing must not lag: {media_ms} ms of media took {wall_ms} ms"
    );
}

/// An upload is authorized before its body is read, and a body may arrive up
/// to four target durations later. The input's row can change in that window —
/// disabled, deleted, or switched to a pull input — so the session opens only
/// if the key still resolves to the same input. Without the re-check, the
/// upload registers an `hls` session for an input that no longer accepts
/// pushes, and recreates the ring of a pipeline that is going away.
#[tokio::test]
async fn an_upload_whose_input_changed_before_its_body_opens_no_session() {
    let access = RevocableKey::new();
    let (engine, ingest) = test_ingest_with(HlsIngestLimits::default(), access.clone());
    let pipeline = ingest
        .authorize(STREAM_KEY, "127.0.0.1")
        .await
        .expect("the key resolves while the row stands");

    // The row changes before the body the authorization was for arrives.
    access.revoke();

    let mut body = ingest
        .object_body(ObjectKind::Playlist, None, &pipeline.input_id)
        .await
        .unwrap();
    body.push(b"#EXTM3U\n#EXT-X-VERSION:3\n").await.unwrap();
    let error = ingest
        .dispatch(
            &pipeline,
            STREAM_KEY,
            // The peer address the session meta records, as the API passes it:
            // the re-check must derive the bare IP from it, because the security
            // layer keys bans and rate limits by IP.
            "127.0.0.1:9",
            DispatchOrigin::Push,
            ObjectKind::Playlist,
            "index.m3u8".to_string(),
            body,
        )
        .await
        .expect_err("the session must not open for a key that no longer resolves");
    assert_eq!(error, DispatchError::PublisherActive);
    assert!(
        !engine
            .input_has_session(PIPELINE_ID, &pipeline.input_id)
            .await,
        "no session, and no ring, for an input whose key no longer resolves"
    );
    assert_eq!(
        access.seen_client_ips(),
        vec!["127.0.0.1".to_string(), "127.0.0.1".to_string()],
        "the authorization and the re-check both see the bare IP, not the peer address"
    );
}
