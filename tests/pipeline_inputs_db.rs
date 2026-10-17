#![allow(clippy::disallowed_methods)] // test code: raw std locks are fine
use restream::db;
use restream::domain::pipeline_input::{
    PipelineInputRole, PipelineInputSource, PipelineInputSourceKind, PullAuth, VariantRule,
};
use sqlx::SqlitePool;

async fn memory_pool() -> SqlitePool {
    SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite")
}

#[tokio::test]
async fn pipeline_creation_atomically_creates_its_selected_primary_input() {
    let pool = memory_pool().await;
    db::setup_database_schema(&pool)
        .await
        .expect("schema setup");
    db::create_pipeline(&pool, "p1", "Main", "first-key", None, None)
        .await
        .expect("pipeline create");

    let inputs = db::list_pipeline_inputs(&pool, "p1")
        .await
        .expect("pipeline inputs");

    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].stream_key, "first-key");
    assert_eq!(inputs[0].role, PipelineInputRole::Primary);
    assert!(inputs[0].enabled);
    assert!(inputs[0].selected);
}

#[tokio::test]
async fn promotion_selects_exactly_one_enabled_input() {
    let pool = memory_pool().await;
    db::setup_database_schema(&pool)
        .await
        .expect("schema setup");
    db::create_pipeline(&pool, "p1", "Main", "primary-key", None, None)
        .await
        .expect("pipeline create");
    let backup_a = db::create_pipeline_input(
        &pool,
        "input-a",
        "p1",
        "Encoder A",
        "backup-a-key",
        &PipelineInputSource::keyed(),
    )
    .await
    .expect("first backup");
    db::create_pipeline_input(
        &pool,
        "input-b",
        "p1",
        "Encoder B",
        "backup-b-key",
        &PipelineInputSource::keyed(),
    )
    .await
    .expect("second backup");

    let promoted = db::promote_pipeline_input(&pool, "p1", &backup_a.id)
        .await
        .expect("promotion")
        .expect("promoted input");

    assert!(promoted.selected);
    assert_eq!(promoted.role, PipelineInputRole::Backup);
    let inputs = db::list_pipeline_inputs(&pool, "p1")
        .await
        .expect("pipeline inputs");
    assert_eq!(inputs.iter().filter(|input| input.selected).count(), 1);
    assert_eq!(
        inputs
            .iter()
            .find(|input| input.selected)
            .map(|input| input.id.as_str()),
        Some("input-a")
    );
}

#[tokio::test]
async fn concurrent_creates_cannot_exceed_pipeline_input_limit() {
    let pool = memory_pool().await;
    db::setup_database_schema(&pool)
        .await
        .expect("schema setup");
    db::create_pipeline(&pool, "pipeline", "Event", "key-primary", None, None)
        .await
        .expect("pipeline");

    let mut tasks = Vec::new();
    for index in 0..8 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            db::create_pipeline_input(
                &pool,
                &format!("input-{index}"),
                "pipeline",
                &format!("Encoder {index}"),
                &format!("key-{index}"),
                &PipelineInputSource::keyed(),
            )
            .await
        }));
    }
    let mut created = 0;
    for task in tasks {
        if task.await.expect("create task").is_ok() {
            created += 1;
        }
    }

    assert_eq!(created, 3);
    assert_eq!(
        db::list_pipeline_inputs(&pool, "pipeline")
            .await
            .expect("inputs")
            .len(),
        4
    );
}

async fn schema_pool() -> SqlitePool {
    let pool = memory_pool().await;
    db::setup_database_schema(&pool)
        .await
        .expect("schema setup");
    pool
}

#[tokio::test]
async fn keyed_inputs_carry_no_pull_source() {
    let pool = schema_pool().await;
    db::create_pipeline(&pool, "p1", "Main", "primary-key", None, None)
        .await
        .expect("pipeline create");
    db::create_pipeline_input(
        &pool,
        "input-backup",
        "p1",
        "Backup",
        "backup-key",
        &PipelineInputSource::keyed(),
    )
    .await
    .expect("backup create");

    let inputs = db::list_pipeline_inputs(&pool, "p1")
        .await
        .expect("pipeline inputs");
    assert_eq!(inputs.len(), 2);
    for input in &inputs {
        assert_eq!(input.source_kind, PipelineInputSourceKind::Keyed);
        assert_eq!(input.source_url, None);
        assert_eq!(input.variant_rule, VariantRule::HighestBandwidth);
        assert_eq!(input.source_auth, PullAuth::None);
    }
}

#[tokio::test]
async fn pull_input_round_trips_every_source_field() {
    let pool = schema_pool().await;
    db::create_pipeline(&pool, "p1", "Main", "primary-key", None, None)
        .await
        .expect("pipeline create");

    let created = db::create_pipeline_input(
        &pool,
        "input-pull",
        "p1",
        "Origin",
        "pull-key",
        &PipelineInputSource {
            kind: PipelineInputSourceKind::HlsPull,
            url: Some("https://origin.example/live/master.m3u8".to_string()),
            variant_rule: VariantRule::LowestBandwidth,
            auth: PullAuth::Headers {
                headers: vec![restream::domain::pipeline_input::HeaderPair {
                    name: "x-api-token".to_string(),
                    value: "shh".to_string(),
                }],
            },
        },
    )
    .await
    .expect("pull create");

    assert_eq!(created.source_kind, PipelineInputSourceKind::HlsPull);
    assert_eq!(
        created.source_url.as_deref(),
        Some("https://origin.example/live/master.m3u8")
    );
    assert_eq!(created.variant_rule, VariantRule::LowestBandwidth);
    assert_eq!(
        created.source_auth,
        PullAuth::Headers {
            headers: vec![restream::domain::pipeline_input::HeaderPair {
                name: "x-api-token".to_string(),
                value: "shh".to_string(),
            }],
        }
    );

    let reloaded = db::get_pipeline_input(&pool, "p1", "input-pull")
        .await
        .expect("read pull input")
        .expect("pull input exists");
    assert_eq!(reloaded, created);

    let updated = db::update_pipeline_input(
        &pool,
        "p1",
        "input-pull",
        "Origin renamed",
        true,
        &PipelineInputSource {
            kind: PipelineInputSourceKind::HlsPull,
            url: Some("https://other.example/playlist.m3u8".to_string()),
            variant_rule: VariantRule::HighestBandwidth,
            auth: PullAuth::Basic {
                username: "operator".to_string(),
                password: "hunter2".to_string(),
            },
        },
    )
    .await
    .expect("update pull input")
    .expect("pull input exists");

    assert_eq!(updated.label, "Origin renamed");
    assert_eq!(
        updated.source_url.as_deref(),
        Some("https://other.example/playlist.m3u8")
    );
    assert_eq!(updated.variant_rule, VariantRule::HighestBandwidth);
    assert_eq!(
        updated.source_auth,
        PullAuth::Basic {
            username: "operator".to_string(),
            password: "hunter2".to_string(),
        }
    );
}

#[tokio::test]
async fn list_pull_inputs_returns_only_pull_inputs_in_pipeline_order() {
    let pool = schema_pool().await;
    db::create_pipeline(&pool, "p2", "Second", "p2-primary", None, None)
        .await
        .expect("pipeline p2");
    db::create_pipeline(&pool, "p1", "First", "p1-primary", None, None)
        .await
        .expect("pipeline p1");

    let pull = |url: &str| PipelineInputSource {
        kind: PipelineInputSourceKind::HlsPull,
        url: Some(url.to_string()),
        ..PipelineInputSource::default()
    };

    db::create_pipeline_input(
        &pool,
        "pull-b",
        "p2",
        "Origin B",
        "sk-b",
        &pull("https://b"),
    )
    .await
    .expect("pull b");
    db::create_pipeline_input(
        &pool,
        "pull-a",
        "p1",
        "Origin A",
        "sk-a",
        &pull("https://a"),
    )
    .await
    .expect("pull a");
    // A disabled pull input is still returned: the reconciler needs it to
    // cancel a puller that is already running.
    db::update_pipeline_input(&pool, "p1", "pull-a", "Origin A", false, &pull("https://a"))
        .await
        .expect("disable pull a")
        .expect("pull a exists");
    // A keyed backup must not appear.
    db::create_pipeline_input(
        &pool,
        "keyed-a",
        "p1",
        "Keyed A",
        "sk-keyed",
        &PipelineInputSource::keyed(),
    )
    .await
    .expect("keyed a");

    let pulls = db::list_pull_inputs(&pool).await.expect("pull inputs");
    let ids: Vec<&str> = pulls.iter().map(|input| input.id.as_str()).collect();
    assert_eq!(ids, vec!["pull-a", "pull-b"]);
    assert!(
        pulls
            .iter()
            .any(|input| input.id == "pull-a" && !input.enabled),
        "disabled pull input must still be listed"
    );
    assert!(
        pulls
            .iter()
            .all(|input| input.source_kind == PipelineInputSourceKind::HlsPull)
    );
}

/// A row whose `source_auth` passes the column's CHECK (valid JSON, known kind)
/// but does not parse as that variant — `{"kind":"bearer"}` with no token —
/// must not fail the list it is in. The inputs list is what the SRT policy
/// store is built from, so one bad row failing the query would take SRT
/// admission down for every pipeline, not just this input.
#[tokio::test]
async fn a_row_whose_auth_does_not_parse_does_not_fail_the_list() {
    let pool = schema_pool().await;
    db::create_pipeline(&pool, "p1", "Main", "primary-key", None, None)
        .await
        .expect("pipeline create");
    db::create_pipeline(&pool, "p2", "Other", "other-key", None, None)
        .await
        .expect("second pipeline");
    // Written the way only a hand-edited database can: the API validates the
    // credential before it stores it.
    sqlx::query(
        "INSERT INTO pipeline_inputs
         (id, pipeline_id, label, stream_key, role, enabled, selected, source_kind, source_url, variant_rule, source_auth)
         VALUES ('input-bad', 'p1', 'Broken', 'bad-key', 'backup', 1, 0, 'hls_pull',
                 'https://origin.example/live/master.m3u8', 'highest_bandwidth', '{\"kind\":\"bearer\"}')",
    )
    .execute(&pool)
    .await
    .expect("a row the CHECK accepts");

    let inputs = db::list_pipeline_inputs(&pool, "p1")
        .await
        .expect("one bad credential must not fail the list");
    let bad = inputs
        .iter()
        .find(|input| input.id == "input-bad")
        .expect("the row is still listed");
    assert_eq!(
        bad.source_auth,
        PullAuth::None,
        "its unusable credential reads as none, so only this input is affected"
    );

    let pulls = db::list_pull_inputs(&pool)
        .await
        .expect("the pull list is not failed by it either");
    assert!(pulls.iter().any(|input| input.id == "input-bad"));

    let other = db::list_pipeline_inputs(&pool, "p2")
        .await
        .expect("another pipeline's inputs list");
    // The SRT policy store is built from this same list, so another pipeline
    // still has its keyed input to admit with.
    assert_eq!(other.len(), 1, "and it still carries that pipeline's input");
    assert_eq!(other[0].stream_key, "other-key");
}
