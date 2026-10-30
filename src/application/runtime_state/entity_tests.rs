//! Inputs and graphs in the runtime state: what a client reading the
//! state stream instead of polling `/inputs` and `/graph` relies on.

use super::hub::{Since, StateHub};
use super::tests::{max_retries, output, ports, project_rows};
use super::*;
use restream_domain::domain::pipeline_input::PipelineInputSource;

async fn database() -> sqlx::SqlitePool {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    pool
}

/// What a client holds after applying `deltas` to `snapshot`, as the
/// browser's `applyStateDelta` does.
fn replay(
    snapshot: RuntimeState,
    deltas: &[std::sync::Arc<StateDelta>],
) -> (Vec<InputState>, Vec<PipelineGraph>) {
    let mut inputs: BTreeMap<String, InputState> = snapshot
        .inputs
        .into_iter()
        .map(|input| (input.id.clone(), input))
        .collect();
    let mut graphs: BTreeMap<String, PipelineGraph> = snapshot
        .graphs
        .into_iter()
        .map(|graph| (graph.pipeline_id.clone(), graph))
        .collect();
    let mut revision = snapshot.revision;
    for delta in deltas {
        assert_eq!(delta.revision, revision + 1, "contiguous revisions");
        revision = delta.revision;
        for id in &delta.removed_inputs {
            inputs.remove(id);
        }
        for id in &delta.removed_graphs {
            graphs.remove(id);
        }
        for input in &delta.inputs {
            inputs.insert(input.id.clone(), input.clone());
        }
        for graph in &delta.graphs {
            graphs.insert(graph.pipeline_id.clone(), graph.clone());
        }
    }
    (
        inputs.into_values().collect(),
        graphs.into_values().collect(),
    )
}

async fn deltas_after(hub: &StateHub, held: &RuntimeState) -> Vec<std::sync::Arc<StateDelta>> {
    match hub
        .since(&format!("{}.{}", held.epoch, held.revision))
        .await
    {
        Since::Deltas(deltas) => deltas,
        Since::Resync => panic!("the gap is in history"),
    }
}

/// Inputs follow their rows and their sessions: a connect, an edit and a
/// removal each reach a client as a delta, and a client that replays the
/// deltas holds what a fresh snapshot holds. A session's counters (bytes,
/// transport quality) are not state: they never make a delta, or every
/// rebuild would be one.
#[tokio::test]
async fn inputs_follow_rows_and_sessions_but_not_counters() {
    let pool = database().await;
    let engine = MediaEngine::new();
    let hub = StateHub::new(ports(), max_retries());
    crate::db::create_pipeline(&pool, "pipe", "P", "key-main", None, None)
        .await
        .unwrap();
    crate::db::create_pipeline_input(
        &pool,
        "backup",
        "pipe",
        "Backup",
        "key-backup",
        &PipelineInputSource::keyed(),
    )
    .await
    .unwrap();
    hub.refresh(&pool, &engine).await;
    let held = hub.snapshot().await;
    assert_eq!(held.inputs.len(), 2);
    let backup = held
        .inputs
        .iter()
        .find(|input| input.id == "backup")
        .unwrap();
    let primary = held
        .inputs
        .iter()
        .find(|input| input.id != "backup")
        .unwrap();
    let primary_id = primary.id.clone();
    assert_eq!(
        (primary.position, primary.role, primary.selected),
        (
            0,
            restream_domain::domain::pipeline_input::PipelineInputRole::Primary,
            true
        )
    );
    assert_eq!(
        (backup.position, backup.role, backup.selected),
        (
            1,
            restream_domain::domain::pipeline_input::PipelineInputRole::Backup,
            false
        )
    );
    assert_eq!(
        primary.ingest_urls.as_ref().map(|urls| urls.rtmp.as_str()),
        Some(format!("rtmp://localhost:{}/live/key-main", ports().rtmp).as_str())
    );
    assert!(!primary.runtime.connected);
    assert_eq!(held.graphs.len(), 1, "one graph per pipeline");

    // A publisher connects on the primary: only that input changes.
    engine
        .try_register_pipeline_input_attempt("pipe", &primary_id, "key-main", "srt", true)
        .await
        .expect("the primary registers");
    hub.refresh(&pool, &engine).await;
    let connected = hub.snapshot().await;
    assert_eq!(connected.revision, held.revision + 1);
    let deltas = deltas_after(&hub, &held).await;
    let changed: Vec<&str> = deltas[0]
        .inputs
        .iter()
        .map(|input| input.id.as_str())
        .collect();
    assert_eq!(changed, [primary_id.as_str()]);
    let runtime = &deltas[0].inputs[0].runtime;
    assert!(runtime.connected);
    assert_eq!(runtime.protocol.as_deref(), Some("srt"));
    assert!(runtime.forwarding_state.is_some());

    // Counters advance: no revision.
    let session = engine.ingests.sessions.read().await[&primary_id].clone();
    session
        .bytes_received
        .fetch_add(4096, std::sync::atomic::Ordering::Relaxed);
    engine
        .update_publisher_quality(
            "pipe",
            restream_engine::snapshots::PublisherQuality {
                ms_rtt: Some(42.0),
                ..Default::default()
            },
        )
        .await;
    hub.mark_changed();
    hub.refresh(&pool, &engine).await;
    assert_eq!(
        hub.snapshot().await.revision,
        connected.revision,
        "counters are sampled, not projected"
    );

    // An input added with nothing else changing is a revision of its own.
    crate::db::create_pipeline_input(
        &pool,
        "spare",
        "pipe",
        "Spare",
        "key-spare",
        &PipelineInputSource::keyed(),
    )
    .await
    .unwrap();
    hub.refresh(&pool, &engine).await;
    let deltas = deltas_after(&hub, &connected).await;
    assert_eq!(deltas.len(), 1, "an input-only change is published");
    assert_eq!(deltas[0].inputs.len(), 1);
    assert_eq!(deltas[0].inputs[0].id, "spare");
    assert_eq!(deltas[0].inputs[0].position, 2);
    let spared = hub.snapshot().await;

    // The backup is removed: its id goes.
    assert!(
        crate::db::delete_pipeline_input(&pool, "pipe", "backup")
            .await
            .unwrap()
    );
    hub.refresh(&pool, &engine).await;
    let deltas = deltas_after(&hub, &spared).await;
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].removed_inputs, ["backup"]);

    let current = hub.snapshot().await;
    let (inputs, graphs) = replay(held.clone(), &deltas_after(&hub, &held).await);
    assert_eq!(
        inputs, current.inputs,
        "a client that keeps up holds the snapshot"
    );
    assert_eq!(graphs, current.graphs);
}

/// Deleting a pipeline removes its inputs and its graph from every client:
/// without `removedInputs`/`removedGraphs` a client keeps orphans.
#[tokio::test]
async fn a_deleted_pipeline_takes_its_inputs_and_graph_with_it() {
    let pool = database().await;
    let engine = MediaEngine::new();
    let hub = StateHub::new(ports(), max_retries());
    for (id, key) in [("pipe", "key-a"), ("other", "key-b")] {
        crate::db::create_pipeline(&pool, id, id, key, None, None)
            .await
            .unwrap();
    }
    hub.refresh(&pool, &engine).await;
    let held = hub.snapshot().await;
    let gone: Vec<String> = held
        .inputs
        .iter()
        .filter(|input| input.pipeline_id == "pipe")
        .map(|input| input.id.clone())
        .collect();
    assert_eq!(gone.len(), 1);

    assert!(crate::db::delete_pipeline(&pool, "pipe").await.unwrap());
    hub.refresh(&pool, &engine).await;
    let deltas = deltas_after(&hub, &held).await;
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].removed_inputs, gone);
    assert_eq!(deltas[0].removed_graphs, ["pipe"]);

    let current = hub.snapshot().await;
    let (inputs, graphs) = replay(held, &deltas);
    assert_eq!(inputs, current.inputs);
    assert_eq!(graphs, current.graphs);
    assert!(inputs.iter().all(|input| input.pipeline_id == "other"));
}

/// The graph a client draws comes from the state: the live ingest, the
/// source ring, and each output's egress node joined on its output id and
/// fed from the stage it reads. Media moving changes counters only, which
/// leaves the graph as it was.
#[tokio::test]
async fn the_graph_is_the_running_structure_without_counters() {
    let engine = MediaEngine::new();
    let pipelines = [crate::db::PipelineRecord {
        id: "pipe".to_string(),
        name: "pipe".to_string(),
        stream_key: "k".to_string(),
        input_source: None,
        srt_ingest_policy: None,
    }];
    let outputs = [output(
        "out",
        restream_domain::domain::state::DesiredOutputState::Running,
    )];
    let idle = project_rows(&engine, &pipelines, &outputs).await;
    let graph = &idle.graphs["pipe"];
    let node = |graph: &PipelineGraph, id: &str| {
        graph
            .nodes
            .iter()
            .find(|node| node.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("node {id} in {graph:?}"))
    };
    assert_eq!(node(graph, "pipe_ingest").label, "No ingest");
    assert!(!node(graph, "pipe_ingest").active);
    let egress = node(graph, "pipe_output_out");
    assert_eq!(egress.output_id.as_deref(), Some("out"));
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.from == "pipe_source_rb" && edge.to == "pipe_output_out")
    );

    engine
        .try_register_ingest("pipe", "k", "rtmp")
        .await
        .expect("the ingest registers");
    let live = project_rows(&engine, &pipelines, &outputs).await;
    let ingest = node(&live.graphs["pipe"], "pipe_ingest");
    assert_eq!(
        (ingest.label.as_str(), ingest.active),
        ("RTMP ingest", true)
    );
    assert_eq!(diff(&idle, &live).graphs.len(), 1, "the ingest is a change");

    let active = engine.ingests.active.read().await["pipe"].clone();
    active
        .bytes_received
        .fetch_add(1 << 20, std::sync::atomic::Ordering::Relaxed);
    active.metrics.record_in(1316);
    let moving = project_rows(&engine, &pipelines, &outputs).await;
    assert_eq!(moving.graphs, live.graphs, "counters are not graph state");
}

/// A keyed input advertises a MoQ publish URL only while the MoQ ingest
/// listener is configured; the other URLs do not depend on it.
#[tokio::test]
async fn a_keyed_input_names_its_moq_url_only_while_moq_ingest_is_on() {
    async fn urls(
        pool: &sqlx::SqlitePool,
        engine: &MediaEngine,
        ports: crate::config::ServerPorts,
    ) -> IngestUrls {
        let hub = StateHub::new(ports, max_retries());
        hub.refresh(pool, engine).await;
        hub.snapshot().await.inputs[0].ingest_urls.clone().unwrap()
    }
    let pool = database().await;
    let engine = MediaEngine::new();
    crate::db::create_pipeline(&pool, "pipe", "P", "key-main", None, None)
        .await
        .unwrap();

    let off = urls(&pool, &engine, ports()).await;
    assert_eq!(off.moq, None);

    let moq_ports = crate::config::ServerPorts {
        moq: Some(4443),
        ..ports()
    };
    let on = urls(&pool, &engine, moq_ports).await;
    assert_eq!(on.moq.as_deref(), Some("https://localhost:4443/key-main"));
    assert_eq!((on.rtmp, on.srt, on.hls), (off.rtmp, off.srt, off.hls));
}

#[test]
fn since_stamps_carry_while_nothing_changes_and_reset_when_something_does() {
    let output = OutputState {
        id: "o1".into(),
        pipeline_id: "p1".into(),
        name: "YouTube".into(),
        protocol: EgressProtocol::Rtmp,
        shard: None,
        stage: None,
        desired: DesiredOutputState::Running,
        actual: None,
        convergence: Convergence::Converged,
        flow: Flow::Flowing,
        backpressure: None,
        retry: None,
        evidence: None,
        diagnosis: None,
        state_since_ms: 0,
    };
    let mut before = Projection::default();
    before.outputs.insert("o1".into(), output.clone());
    stamp_since(&Projection::default(), &mut before, 1_000);
    assert_eq!(
        before.outputs["o1"].state_since_ms, 1_000,
        "a new output starts now"
    );

    let mut same = Projection::default();
    same.outputs.insert("o1".into(), output.clone());
    stamp_since(&before, &mut same, 5_000);
    assert_eq!(
        same.outputs["o1"].state_since_ms, 1_000,
        "unchanged: the stamp carries"
    );

    let mut changed = Projection::default();
    let mut flowing = output.clone();
    flowing.flow = Flow::Stalled;
    changed.outputs.insert("o1".into(), flowing);
    stamp_since(&same, &mut changed, 9_000);
    assert_eq!(
        changed.outputs["o1"].state_since_ms, 9_000,
        "a new flow restarts it"
    );
}
