use super::hub::{DELTA_HISTORY, Since, StateHub};
use super::*;
use crate::domain::output_spec::OutputConfig;
use ts_rs::TS;

const GENERATED: &str = "web/ts/core/generated/runtime-state.ts";

fn render_contract() -> String {
    let cfg = ts_rs::Config::new();
    let decls = [
        RuntimeState::decl(&cfg),
        StateDelta::decl(&cfg),
        PipelineState::decl(&cfg),
        IngestState::decl(&cfg),
        OutputState::decl(&cfg),
        Convergence::decl(&cfg),
        Flow::decl(&cfg),
        RetryState::decl(&cfg),
        FailureEvidence::decl(&cfg),
        crate::application::diagnosis::Diagnosis::decl(&cfg),
        crate::application::diagnosis::Layer::decl(&cfg),
        crate::application::diagnosis::Cause::decl(&cfg),
        DesiredOutputState::decl(&cfg),
        EgressPhase::decl(&cfg),
        crate::capacity::CapacitySnapshot::decl(&cfg),
        crate::capacity::ServiceCenter::decl(&cfg),
        crate::capacity::FlowDiagnosis::decl(&cfg),
        crate::capacity::FlowStatus::decl(&cfg),
    ];
    let mut out = String::from(
        "// Generated from src/application/runtime_state, diagnosis and capacity (ts-rs). Do not edit.\n\
         // Regenerate: cargo test --lib runtime_state::tests::write_generated -- --ignored\n\n",
    );
    for decl in decls {
        out.push_str("export ");
        out.push_str(&decl);
        out.push_str("\n\n");
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

fn generated_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GENERATED)
}

/// The checked-in TypeScript contract matches the Rust types.
#[test]
fn generated_typescript_contract_is_current() {
    let checked_in = std::fs::read_to_string(generated_path()).unwrap_or_default();
    assert!(
        checked_in == render_contract(),
        "{GENERATED} is stale; regenerate with \
         cargo test --lib runtime_state::tests::write_generated -- --ignored"
    );
}

/// Writes the TypeScript contract (run on demand, see the drift test).
#[test]
#[ignore = "writes web/ts/core/generated/runtime-state.ts"]
fn write_generated_typescript_contract() {
    let path = generated_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, render_contract()).unwrap();
}

fn output(id: &str, desired: DesiredOutputState) -> Output {
    Output {
        id: id.to_string(),
        pipeline_id: "pipe".to_string(),
        name: id.to_string(),
        url: "rtmp://127.0.0.1:9/live/k".to_string(),
        monitoring_url: None,
        desired_state: desired,
        retries_exhausted: false,
        config: OutputConfig::source(),
    }
}

fn projection_with(outputs: &[(&str, Convergence)]) -> Projection {
    let mut projection = Projection::default();
    for (id, convergence) in outputs {
        projection.outputs.insert(
            id.to_string(),
            OutputState {
                id: id.to_string(),
                pipeline_id: "pipe".to_string(),
                name: id.to_string(),
                desired: DesiredOutputState::Running,
                actual: None,
                convergence: *convergence,
                flow: Flow::Idle,
                backpressure: None,
                retry: None,
                evidence: None,
                diagnosis: None,
            },
        );
    }
    projection
}

#[tokio::test]
async fn a_change_advances_the_revision_with_only_the_changed_entities() {
    let hub = StateHub::new();
    let first = hub
        .publish(projection_with(&[
            ("a", Convergence::Starting),
            ("b", Convergence::Starting),
        ]))
        .await
        .expect("first projection is a change");
    assert_eq!(first.revision, 1);
    assert_eq!(first.outputs.len(), 2);

    assert!(
        hub.publish(projection_with(&[
            ("a", Convergence::Starting),
            ("b", Convergence::Starting),
        ]))
        .await
        .is_none(),
        "an identical projection is not a revision"
    );

    let second = hub
        .publish(projection_with(&[("a", Convergence::Converged)]))
        .await
        .unwrap();
    assert_eq!(second.revision, 2);
    assert_eq!(second.outputs.len(), 1, "only the changed output");
    assert_eq!(second.outputs[0].id, "a");
    assert_eq!(second.removed_outputs, vec!["b".to_string()]);
    assert_eq!(hub.snapshot().await.revision, 2);
}

/// Applying every delta after a snapshot's revision reproduces the current
/// snapshot: a client that keeps up never needs another fetch.
#[tokio::test]
async fn deltas_after_a_revision_rebuild_the_current_state() {
    let hub = StateHub::new();
    hub.publish(projection_with(&[("a", Convergence::Starting)]))
        .await;
    let seen = hub.snapshot().await;
    hub.publish(projection_with(&[
        ("a", Convergence::Converged),
        ("b", Convergence::Starting),
    ]))
    .await;
    hub.publish(projection_with(&[("b", Convergence::Retrying)]))
        .await;

    let Since::Deltas(deltas) = hub
        .since(&format!("{}.{}", seen.epoch, seen.revision))
        .await
    else {
        panic!("the gap is in history");
    };
    let mut outputs: BTreeMap<String, OutputState> = seen
        .outputs
        .into_iter()
        .map(|output| (output.id.clone(), output))
        .collect();
    let mut revision = seen.revision;
    for delta in deltas {
        assert_eq!(delta.revision, revision + 1, "contiguous revisions");
        revision = delta.revision;
        for output in &delta.outputs {
            outputs.insert(output.id.clone(), output.clone());
        }
        for id in &delta.removed_outputs {
            outputs.remove(id);
        }
    }
    let current = hub.snapshot().await;
    assert_eq!(revision, current.revision);
    assert_eq!(outputs.into_values().collect::<Vec<_>>(), current.outputs);
}

#[tokio::test]
async fn a_client_behind_the_history_or_from_another_run_resyncs() {
    let hub = StateHub::new();
    for round in 0..(DELTA_HISTORY + 2) {
        let convergence = if round % 2 == 0 {
            Convergence::Starting
        } else {
            Convergence::Converged
        };
        hub.publish(projection_with(&[("a", convergence)])).await;
    }
    let snapshot = hub.snapshot().await;
    let (epoch, current) = (snapshot.epoch, snapshot.revision);
    let token = |revision: u64| format!("{epoch}.{revision}");
    assert_eq!(
        hub.since(&token(1)).await,
        Since::Resync,
        "gap older than history"
    );
    assert_eq!(
        hub.since(&token(current + 5)).await,
        Since::Resync,
        "unknown revision"
    );
    assert_eq!(hub.since("garbage").await, Since::Resync);
    assert_eq!(hub.since(&token(current)).await, Since::Deltas(Vec::new()));
    let Since::Deltas(kept) = hub.since(&token(current - DELTA_HISTORY as u64)).await else {
        panic!("the oldest kept gap is replayable");
    };
    assert_eq!(kept.len(), DELTA_HISTORY);
}

/// Review of #285: after a restart the new run can pass the client's old
/// revision before it reconnects. Its deltas must not apply to the old
/// run's state: the epoch in the token forces a snapshot.
#[tokio::test]
async fn a_resume_from_another_server_run_resyncs_even_below_its_revision() {
    let before = StateHub::with_epoch("run-a".to_string());
    before
        .publish(projection_with(&[("a", Convergence::Starting)]))
        .await;
    let held = before.snapshot().await;
    assert_eq!(held.revision, 1);

    let after = StateHub::with_epoch("run-b".to_string());
    for convergence in [
        Convergence::Starting,
        Convergence::Converged,
        Convergence::Retrying,
    ] {
        after.publish(projection_with(&[("a", convergence)])).await;
    }
    assert!(after.snapshot().await.revision > held.revision);
    assert_eq!(
        after
            .since(&format!("{}.{}", held.epoch, held.revision))
            .await,
        Since::Resync
    );
}

#[tokio::test]
async fn projection_derives_convergence_from_desired_and_runtime() {
    let engine = MediaEngine::new();
    let mut exhausted = output("exhausted", DesiredOutputState::Running);
    exhausted.retries_exhausted = true;
    let outputs = [
        output("stopped", DesiredOutputState::Stopped),
        output("waiting", DesiredOutputState::Running),
        exhausted,
    ];
    let projection = project(&engine, &[], &outputs).await;
    let convergence = |id: &str| projection.outputs[id].convergence;
    assert_eq!(convergence("stopped"), Convergence::Converged);
    assert_eq!(convergence("waiting"), Convergence::WaitingForIngest);
    assert_eq!(convergence("exhausted"), Convergence::RetriesExhausted);
    let diagnosis = |projection: &Projection, id: &str| projection.outputs[id].diagnosis.clone();
    assert_eq!(diagnosis(&projection, "stopped"), None);
    assert_eq!(
        diagnosis(&projection, "waiting").map(|d| d.cause),
        Some(crate::application::diagnosis::Cause::NoPublisher)
    );

    engine
        .try_register_ingest("pipe", "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .update_egress_retry_state("waiting", 2, 1_000, 1_000)
        .await;
    let projection = project(&engine, &[], &outputs).await;
    let waiting = &projection.outputs["waiting"];
    assert_eq!(waiting.convergence, Convergence::Retrying);
    assert_eq!(waiting.retry.as_ref().map(|retry| retry.attempts), Some(2));
    let rejected = waiting
        .diagnosis
        .clone()
        .expect("a retrying output is degraded");
    assert_eq!(
        rejected.layer,
        crate::application::diagnosis::Layer::Destination
    );
    assert!(matches!(
        rejected.cause,
        crate::application::diagnosis::Cause::Rejected { .. }
    ));
    assert_eq!(
        rejected.ruled_out,
        [
            crate::application::diagnosis::Layer::Ingest,
            crate::application::diagnosis::Layer::Feed
        ]
    );
    assert_eq!(
        diagnosis(&projection, "exhausted").map(|d| d.cause),
        Some(crate::application::diagnosis::Cause::RetriesExhausted)
    );

    engine
        .register_egress_attempt_with_meta(
            "stopped",
            "pipe",
            "rtmp://127.0.0.1:9/live/k",
            Some("stopped"),
            Some("source"),
            None,
        )
        .await;
    let projection = project(&engine, &[], &outputs).await;
    assert_eq!(
        projection.outputs["stopped"].convergence,
        Convergence::Stopping,
        "a stopped output whose egress still runs is converging"
    );
    assert_eq!(projection.outputs["stopped"].flow, Flow::Idle);
}

/// The projector rebuilds only while a stream client is connected: on a
/// marked change and every interval. With no client, changes cost nothing.
#[tokio::test]
async fn the_projector_rebuilds_only_while_a_client_listens() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    let engine = std::sync::Arc::new(MediaEngine::new());
    let hub = std::sync::Arc::new(StateHub::new());
    let stop = tokio_util::sync::CancellationToken::new();
    let projector = tokio::spawn(hub::run_projector(
        hub.clone(),
        pool.clone(),
        engine,
        std::time::Duration::from_millis(20),
        stop.clone(),
    ));
    let revision_after = |hub: std::sync::Arc<StateHub>| async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        hub.snapshot().await.revision
    };

    crate::db::create_pipeline(&pool, "pipe", "P", "key", None, None)
        .await
        .unwrap();
    hub.mark_changed();
    assert_eq!(
        revision_after(hub.clone()).await,
        0,
        "no client: no rebuild"
    );

    let client = hub.subscribe();
    hub.mark_changed();
    assert_eq!(
        revision_after(hub.clone()).await,
        1,
        "a client gets the change"
    );
    crate::db::create_pipeline(&pool, "pipe-2", "P2", "key-2", None, None)
        .await
        .unwrap();
    assert_eq!(
        revision_after(hub.clone()).await,
        2,
        "with a client, unreported changes arrive on the interval"
    );

    drop(client);
    crate::db::create_pipeline(&pool, "pipe-3", "P3", "key-3", None, None)
        .await
        .unwrap();
    hub.mark_changed();
    assert_eq!(revision_after(hub.clone()).await, 2, "the last client left");
    stop.cancel();
    projector.await.unwrap();
}
