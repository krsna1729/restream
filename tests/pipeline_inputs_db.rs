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
