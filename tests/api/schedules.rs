use super::*;
use restream::application::schedules;

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(auth_req(method, uri, cookie, body))
        .await
        .unwrap();
    let status = resp.status();
    (
        status,
        serde_json::from_slice(&body_bytes(resp).await).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn a_schedule_lists_its_next_time_and_runs_its_action_on_demand() {
    let (app, cookie) = authenticated_app().await;
    let (_, created) = call(
        &app,
        "POST",
        "/api/v1/pipelines",
        &cookie,
        Some(r#"{"name":"Show","streamKey":"key_sched_123456789"}"#),
    )
    .await;
    let pipeline = created["pipeline"]["id"].as_str().unwrap().to_string();
    let mut outputs = Vec::new();
    for name in ["A", "B"] {
        let (status, body) = call(
            &app,
            "POST",
            &format!("/api/v1/pipelines/{pipeline}/outputs"),
            &cookie,
            Some(&format!(r#"{{"name":"{name}","url":"rtmp://127.0.0.1:9/live/{name}","config":{{"video":{{"mode":"source"}},"audio":{{"mode":"all"}}}}}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        outputs.push(body["output"]["id"].as_str().unwrap().to_string());
    }

    let at = chrono::Utc::now().timestamp_millis() + 3_600_000;
    let draft = serde_json::json!({
        "pipelineId": pipeline,
        "name": "Go live on A",
        "action": "startOutputs",
        "outputIds": [outputs[0]],
        "atMs": at,
        "repeat": "daily"
    });
    let (status, schedule) = call(
        &app,
        "POST",
        "/api/v1/schedules",
        &cookie,
        Some(&draft.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{schedule}");
    assert_eq!(schedule["nextAtMs"], at);
    assert_eq!(schedule["lastRunMs"], serde_json::Value::Null);
    let id = schedule["id"].as_str().unwrap().to_string();

    let (status, run) = call(
        &app,
        "POST",
        &format!("/api/v1/schedules/{id}/run"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["result"], "1 of 1 destinations wanted running");
    let (_, settings) = call(&app, "GET", "/api/v1/settings", &cookie, None).await;
    let desired: Vec<_> = settings["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|output| {
            (
                output["name"].as_str().unwrap().to_string(),
                output["desiredState"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        desired.contains(&("A".into(), "running".into())),
        "{desired:?}"
    );
    assert!(
        desired.contains(&("B".into(), "stopped".into())),
        "only the named destination"
    );

    let (_, all) = call(&app, "GET", "/api/v1/schedules", &cookie, None).await;
    assert_eq!(all["schedules"].as_array().unwrap().len(), 1);
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/v1/schedules/{id}"),
            &cookie,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn schedules_are_validated_and_need_an_operator_to_change() {
    let (app, admin) = authenticated_app().await;
    let (_, created) = call(
        &app,
        "POST",
        "/api/v1/pipelines",
        &admin,
        Some(r#"{"name":"P","streamKey":"key_sched_223456789"}"#),
    )
    .await;
    let pipeline = created["pipeline"]["id"].as_str().unwrap().to_string();
    let file_with_outputs = serde_json::json!({
        "pipelineId": pipeline, "name": "x", "action": "startFile", "outputIds": ["o"], "atMs": 1, "repeat": "once"
    });
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/schedules",
            &admin,
            Some(&file_with_outputs.to_string())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let unknown_pipeline = serde_json::json!({
        "pipelineId": "nope", "name": "x", "action": "stopOutputs", "atMs": 1, "repeat": "once"
    });
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/schedules",
            &admin,
            Some(&unknown_pipeline.to_string())
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // As a viewer: read, but not change.
    call(
        &app,
        "POST",
        "/api/v1/auth/view-as",
        &admin,
        Some(r#"{"role":"viewer"}"#),
    )
    .await;
    assert_eq!(
        call(&app, "GET", "/api/v1/schedules", &admin, None).await.0,
        StatusCode::OK
    );
    let ok_draft = serde_json::json!({
        "pipelineId": pipeline, "name": "x", "action": "stopOutputs", "atMs": 1, "repeat": "once"
    });
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/schedules",
            &admin,
            Some(&ok_draft.to_string())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

/// A file database with production's two pools (API and lifecycle).
async fn file_pools() -> (SqlitePool, SqlitePool, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "restream-scheduler-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let url = format!("sqlite://{}", dir.join("db.sqlite").display());
    let api = db::create_pool(&url).await.unwrap();
    db::setup_database_schema(&api).await.unwrap();
    let lifecycle = db::create_lifecycle_pool(&url).await.unwrap();
    (api, lifecycle, dir)
}

fn scheduler_state(pool: &SqlitePool) -> Arc<api::AppState> {
    Arc::new(api::AppState::test_new(
        restream::infrastructure::service_wiring::SqliteServiceFactory::new(pool).compose(),
        pool.clone(),
        Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG)),
        Arc::new(restream_engine::srt::SrtIngestPolicyStore::new(
            SrtGlobalIngestConfig::default(),
            &[],
        )),
        Arc::new(TokioRwLock::new(HashSet::new())),
        Arc::new(MediaEngine::new()),
        broadcast::channel(8).0,
    ))
}

/// The occurrence's "running" record waits behind another writer's lock
/// past its deadline. Once the lock is released the action runs, exactly
/// once, and its outcome replaces "running". Ticks are 1 s apart, so no
/// tick between the deadline and the record landing can re-run it: an
/// occurrence skipped at the deadline is never run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_occurrence_whose_running_record_waits_on_the_write_lock_runs_once() {
    let (pool, lifecycle, dir) = file_pools().await;
    db::create_pipeline(&pool, "pipe", "Show", "key-pipe", None, None)
        .await
        .unwrap();
    db::create_output(
        &pool,
        "out",
        "pipe",
        "A",
        "rtmp://127.0.0.1:9/live/a",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::source(),
    )
    .await
    .unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    schedules::create(
        &pool,
        "sch_lock",
        &schedules::ScheduleDraft {
            pipeline_id: "pipe".to_string(),
            name: "Go live".to_string(),
            action: schedules::ScheduleAction::StartOutputs,
            output_ids: vec!["out".to_string()],
            at_ms: now + 300,
            repeat: schedules::Repeat::Once,
            enabled: true,
        },
        now,
    )
    .await
    .unwrap();

    let mut lock = pool.acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *lock)
        .await
        .unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let scheduler = tokio::spawn(api::schedules::run_scheduler(
        scheduler_state(&pool),
        db::DeadlineDb::new(lifecycle, Duration::from_millis(200)),
        Duration::from_secs(1),
        stop.clone(),
    ));
    // The tick at 1 s finds the occurrence due; its record waits well past
    // its deadline, and the lock is released before the next tick.
    tokio::time::sleep(Duration::from_millis(1_600)).await;
    sqlx::query("ROLLBACK").execute(&mut *lock).await.unwrap();
    drop(lock);

    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let result = schedules::get(&pool, "sch_lock", chrono::Utc::now().timestamp_millis())
                .await
                .unwrap()
                .last_result;
            if result.as_deref().is_some_and(|result| result != "running") {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    // Another tick: a second run would show up as a second audit row, and a
    // late "running" record would replace the outcome.
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let settled = schedules::get(&pool, "sch_lock", chrono::Utc::now().timestamp_millis())
        .await
        .unwrap()
        .last_result;
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE path = ?")
        .bind("/api/v1/schedules/sch_lock")
        .fetch_one(&pool)
        .await
        .unwrap();
    stop.cancel();
    let _ = scheduler.await;
    let _ = std::fs::remove_dir_all(dir);
    assert_eq!(
        finished.ok().flatten().as_deref(),
        Some("ok: 1 of 1 destinations wanted running"),
        "the occurrence's outcome never replaced \"running\""
    );
    assert_eq!(runs, 1, "the action ran exactly once");
    assert_eq!(
        settled.as_deref(),
        Some("ok: 1 of 1 destinations wanted running")
    );
}
