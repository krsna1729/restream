use super::core::test_pool;
use restream::{
    db,
    domain::output_spec::OutputConfig,
    domain::state::DesiredOutputState,
    logging::types::{AppLogEntry, AppLogFilters},
};

fn app_log_entry(
    ts: &str,
    pipeline_id: Option<&str>,
    output_id: Option<&str>,
    event_type: Option<&str>,
    event_class: Option<&str>,
    message: &str,
    fields: Option<&str>,
) -> AppLogEntry {
    AppLogEntry {
        ts: ts.to_string(),
        level: "INFO".to_string(),
        target: "restream::tests".to_string(),
        message: message.to_string(),
        fields: fields.map(str::to_string),
        pipeline_id: pipeline_id.map(str::to_string),
        output_id: output_id.map(str::to_string),
        event_type: event_type.map(str::to_string),
        event_class: event_class.map(str::to_string),
    }
}

#[tokio::test]
async fn app_logs_can_be_queried_by_output_scope() {
    let pool = test_pool().await;
    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();
    db::create_output(
        &pool,
        "o1",
        "p1",
        "Out",
        "rtmp://x",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();

    db::append_app_log_batch(
        &pool,
        &[
            app_log_entry(
                "2024-01-01T00:00:00Z",
                Some("p1"),
                Some("o1"),
                Some("lifecycle.start"),
                Some("lifecycle"),
                "Started",
                Some(r#"{"jobId":"j1"}"#),
            ),
            app_log_entry(
                "2024-01-01T00:01:00Z",
                Some("p1"),
                Some("o1"),
                Some("lifecycle.stop"),
                Some("lifecycle"),
                "Stopped",
                Some(r#"{"jobId":"j1"}"#),
            ),
        ],
    )
    .await
    .unwrap();

    let logs = db::list_app_logs(
        &pool,
        &AppLogFilters {
            after_id: None,
            level: Some("info".to_string()),
            since: None,
            until: None,
            target: None,
            scope: None,
            pipeline_id: Some("p1".to_string()),
            output_id: Some("o1".to_string()),
            event_class: None,
            prefix: None,
            limit: Some(10),
            order: Some("asc".to_string()),
        },
    )
    .await
    .unwrap();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].message, "Started");

    let lifecycle_only = db::list_app_logs(
        &pool,
        &AppLogFilters {
            after_id: None,
            level: Some("info".to_string()),
            since: None,
            until: None,
            target: None,
            scope: None,
            pipeline_id: Some("p1".to_string()),
            output_id: Some("o1".to_string()),
            event_class: Some("lifecycle".to_string()),
            prefix: None,
            limit: Some(10),
            order: Some("asc".to_string()),
        },
    )
    .await
    .unwrap();
    assert_eq!(lifecycle_only.len(), 2);
    assert_eq!(
        lifecycle_only[1].event_type.as_deref(),
        Some("lifecycle.stop")
    );
}

#[tokio::test]
async fn filtered_app_logs_honor_prefix_and_event_class_filters() {
    let pool = test_pool().await;
    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();
    db::create_output(
        &pool,
        "o1",
        "p1",
        "Out",
        "rtmp://x",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();

    db::append_app_log_batch(
        &pool,
        &[
            app_log_entry(
                "2024-01-01T00:00:00Z",
                Some("p1"),
                Some("o1"),
                Some("lifecycle.start"),
                Some("lifecycle"),
                "[lifecycle] started",
                None,
            ),
            app_log_entry(
                "2024-01-01T00:00:01Z",
                Some("p1"),
                Some("o1"),
                Some("output"),
                Some("status"),
                "frame=100",
                None,
            ),
            app_log_entry(
                "2024-01-01T00:00:02Z",
                Some("p1"),
                Some("o1"),
                Some("lifecycle.stop"),
                Some("lifecycle"),
                "[lifecycle] stopped",
                None,
            ),
        ],
    )
    .await
    .unwrap();

    let logs = db::list_app_logs(
        &pool,
        &AppLogFilters {
            after_id: None,
            level: Some("info".to_string()),
            since: None,
            until: None,
            target: None,
            scope: None,
            pipeline_id: Some("p1".to_string()),
            output_id: Some("o1".to_string()),
            event_class: None,
            prefix: Some("[lifecycle]".to_string()),
            limit: Some(2),
            order: Some("asc".to_string()),
        },
    )
    .await
    .unwrap();
    assert_eq!(logs.len(), 2);
    assert!(logs[0].message.contains("[lifecycle]"));

    let lifecycle_logs = db::list_app_logs(
        &pool,
        &AppLogFilters {
            after_id: None,
            level: Some("info".to_string()),
            since: None,
            until: None,
            target: None,
            scope: None,
            pipeline_id: Some("p1".to_string()),
            output_id: Some("o1".to_string()),
            event_class: Some("lifecycle".to_string()),
            prefix: None,
            limit: Some(10),
            order: Some("asc".to_string()),
        },
    )
    .await
    .unwrap();
    assert_eq!(lifecycle_logs.len(), 2);
}

#[tokio::test]
async fn scoped_app_logs_can_be_limited_to_restream_only_entries() {
    let pool = test_pool().await;

    db::append_app_log_batch(
        &pool,
        &[
            app_log_entry(
                "2024-01-01T00:00:00Z",
                None,
                None,
                Some("restream.http.ready"),
                Some("lifecycle"),
                "dashboard API server listening",
                None,
            ),
            app_log_entry(
                "2024-01-01T00:00:01Z",
                Some("p1"),
                None,
                Some("ingest.connected"),
                Some("lifecycle"),
                "publisher connected",
                None,
            ),
            app_log_entry(
                "2024-01-01T00:00:02Z",
                Some("p1"),
                Some("o1"),
                Some("egress.failed"),
                Some("lifecycle"),
                "output failed",
                None,
            ),
        ],
    )
    .await
    .unwrap();

    let restream_logs = db::list_app_logs(
        &pool,
        &AppLogFilters {
            after_id: None,
            level: Some("info".to_string()),
            since: None,
            until: None,
            target: None,
            scope: Some("restream".to_string()),
            pipeline_id: None,
            output_id: None,
            event_class: None,
            prefix: None,
            limit: Some(10),
            order: Some("asc".to_string()),
        },
    )
    .await
    .unwrap();
    assert_eq!(restream_logs.len(), 1);
    assert_eq!(
        restream_logs[0].event_type.as_deref(),
        Some("restream.http.ready")
    );
}
