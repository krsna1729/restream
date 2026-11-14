use super::*;
use proptest::prelude::*;

fn started(id: &str, at: &str) -> RecordingMetadataEvent {
    RecordingMetadataEvent::Started {
        recording_id: id.to_string(),
        pipeline_id: "pipe".to_string(),
        started_at: at.to_string(),
        temp_path: format!("/tmp/{id}.ts"),
    }
}

fn finalized(id: &str, at: &str) -> RecordingMetadataEvent {
    RecordingMetadataEvent::Finalized {
        recording_id: id.to_string(),
        ended_at: at.to_string(),
        final_path: format!("/tmp/{id}.mp4"),
    }
}

fn failed(id: &str, error: &str) -> RecordingMetadataEvent {
    RecordingMetadataEvent::Failed {
        recording_id: id.to_string(),
        error: error.to_string(),
    }
}

/// A start and an end for one recording are one entry, both parts kept;
/// a start that lands after a pending end keeps that end.
#[tokio::test]
async fn updates_for_one_recording_merge_into_one_entry() {
    let (reporter, queue) = recording_metadata_channel();
    reporter.report(finalized("a", "t9"));
    reporter.report(started("a", "t0"));
    reporter.report(started("b", "t1"));
    reporter.report(failed("b", "first"));
    reporter.report(finalized("b", "t2"));
    assert_eq!(queue.pending_len(), 2);

    let a = queue.next().await.unwrap();
    assert_eq!(a.recording_id, "a");
    assert_eq!(a.started.unwrap().started_at, "t0");
    assert_eq!(
        a.end,
        Some(RecordingEnd::Finalized {
            ended_at: "t9".to_string(),
            final_path: "/tmp/a.mp4".to_string()
        })
    );
    let b = queue.next().await.unwrap();
    assert_eq!(b.started.unwrap().started_at, "t1");
    assert!(matches!(b.end, Some(RecordingEnd::Finalized { .. })));
    drop(reporter);
    assert_eq!(queue.next().await, None, "every reporter gone, drained");
}

/// The update a writer took and never acknowledged comes back first on
/// restore, merged under what was reported for it since.
#[tokio::test]
async fn an_unacknowledged_update_is_restored_under_newer_reports() {
    let (reporter, queue) = recording_metadata_channel();
    reporter.report(started("a", "t0"));
    reporter.report(started("b", "t1"));
    let taken = queue.next().await.unwrap();
    assert_eq!(taken.recording_id, "a");
    reporter.report(finalized("a", "t5"));

    queue.restore();
    assert_eq!(queue.pending_len(), 2);
    let a = queue.next().await.unwrap();
    assert_eq!(a.recording_id, "b", "a was queued again after b");
    let a_again = queue.next().await.unwrap();
    assert_eq!(a_again.recording_id, "a");
    assert_eq!(a_again.started.unwrap().started_at, "t0");
    assert!(matches!(a_again.end, Some(RecordingEnd::Finalized { .. })));

    // Acknowledged by taking the next: nothing to restore.
    reporter.report(started("c", "t6"));
    let _ = queue.next().await.unwrap();
    reporter.report(started("d", "t7"));
    let c_ack = queue.next().await.unwrap();
    assert_eq!(c_ack.recording_id, "d");
    queue.restore();
    assert_eq!(queue.next().await.unwrap().recording_id, "d");
}

#[derive(Clone, Debug)]
enum Op {
    Start(u8),
    Finalize(u8, u8),
    Fail(u8, u8),
    /// The writer takes the next update and writes it.
    Write,
    /// The writer takes the next update and panics before writing it; the
    /// restarted writer restores it.
    Crash,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..4).prop_map(Op::Start),
        (0u8..4, any::<u8>()).prop_map(|(id, n)| Op::Finalize(id, n)),
        (0u8..4, any::<u8>()).prop_map(|(id, n)| Op::Fail(id, n)),
        Just(Op::Write),
        Just(Op::Crash),
    ]
}

/// What the catalog holds for one recording: its row's start and end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Row {
    started: Option<RecordingStarted>,
    end: Option<RecordingEnd>,
}

/// The writer's effect, field by field as production's SQL: the row is
/// created once (a replayed create is a unique violation, ignored) and the
/// end overwrites. The queue's own merge replaces a pending start with a
/// newer one; a recording starts once (the ops below report one start per
/// id), so the two agree for every sequence production can produce.
fn write(rows: &mut HashMap<String, Row>, update: RecordingMetadataUpdate) {
    let row = rows.entry(update.recording_id).or_default();
    if row.started.is_none() {
        row.started = update.started;
    }
    if update.end.is_some() {
        row.end = update.end;
    }
}

proptest! {
    /// Against a model that writes every report in order: after any mix of
    /// reports, writes and writer panics, draining leaves each recording
    /// with its start and its latest end, and the queue never holds more
    /// entries than recordings.
    #[test]
    fn drained_rows_match_writing_every_report_in_order(ops in prop::collection::vec(op(), 0..64)) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            let (reporter, queue) = recording_metadata_channel();
            let mut rows = HashMap::new();
            let mut model = HashMap::new();
            let mut started_ids = std::collections::HashSet::new();
            for op in ops {
                let event = match op {
                    // A recording starts once.
                    Op::Start(id) if started_ids.insert(id) => Some(started(&id.to_string(), "t")),
                    Op::Start(_) => None,
                    Op::Finalize(id, n) => Some(finalized(&id.to_string(), &n.to_string())),
                    Op::Fail(id, n) => Some(failed(&id.to_string(), &n.to_string())),
                    Op::Write => {
                        if queue.pending_len() > 0 {
                            write(&mut rows, queue.next().await.unwrap());
                        }
                        None
                    }
                    Op::Crash => {
                        if queue.pending_len() > 0 {
                            let _lost_with_the_writer = queue.next().await.unwrap();
                            queue.restore();
                        }
                        None
                    }
                };
                if let Some(event) = event {
                    write(&mut model, RecordingMetadataUpdate::from_event(event.clone()));
                    reporter.report(event);
                }
                prop_assert!(queue.pending_len() <= model.len());
            }
            drop(reporter);
            while let Some(update) = queue.next().await {
                write(&mut rows, update);
            }
            prop_assert_eq!(rows, model);
            Ok::<(), TestCaseError>(())
        })?;
    }
}
