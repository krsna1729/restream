use restream::db;
use restream::domain::pipeline_input::PipelineInputRole;
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
    let backup_a = db::create_pipeline_input(&pool, "input-a", "p1", "Encoder A", "backup-a-key")
        .await
        .expect("first backup");
    db::create_pipeline_input(&pool, "input-b", "p1", "Encoder B", "backup-b-key")
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
