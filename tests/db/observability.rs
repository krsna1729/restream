use super::core::test_pool;
use restream::{
    db,
    db::log_types::{AppLogEntry, AppLogFilters},
};
use restream_domain::domain::{output_spec::OutputConfig, state::DesiredOutputState};

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

/// Every filter at once: each placeholder must receive its own value, so a
/// value bound to the wrong condition returns the wrong rows.
#[tokio::test]
async fn app_logs_apply_every_filter_together() {
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
    let entry = |ts, class, message| {
        app_log_entry(ts, Some("p1"), Some("o1"), None, Some(class), message, None)
    };
    db::append_app_log_batch(
        &pool,
        &[
            entry(
                "2024-01-01T00:01:00Z",
                "lifecycle",
                "[lifecycle] before cursor",
            ),
            entry("2024-01-01T00:01:00Z", "lifecycle", "[lifecycle] match"),
            entry("2024-01-01T00:01:00Z", "metrics", "[lifecycle] other class"),
            entry("2024-01-01T00:01:00Z", "lifecycle", "other prefix"),
            entry(
                "2024-01-01T00:05:00Z",
                "lifecycle",
                "[lifecycle] after until",
            ),
            entry(
                "2023-12-31T23:59:00Z",
                "lifecycle",
                "[lifecycle] before since",
            ),
            entry(
                "2024-01-01T00:02:00Z",
                "lifecycle",
                "[restart] second prefix",
            ),
        ],
    )
    .await
    .unwrap();
    let first_id = db::list_app_logs(
        &pool,
        // A cursor read orders by id: the first row inserted.
        &AppLogFilters {
            after_id: Some(0),
            level: None,
            since: None,
            until: None,
            target: None,
            scope: None,
            pipeline_id: None,
            output_id: None,
            event_class: None,
            prefix: None,
            limit: Some(1),
            order: None,
        },
    )
    .await
    .unwrap()[0]
        .id;

    let logs = db::list_app_logs(
        &pool,
        &AppLogFilters {
            after_id: Some(first_id),
            level: Some("info".to_string()),
            since: Some("2024-01-01T00:00:00Z".to_string()),
            until: Some("2024-01-01T00:03:00Z".to_string()),
            target: Some("restream::".to_string()),
            scope: Some("output".to_string()),
            pipeline_id: Some("p1".to_string()),
            output_id: Some("o1".to_string()),
            event_class: Some("lifecycle".to_string()),
            prefix: Some("[lifecycle], [restart]".to_string()),
            limit: Some(10),
            order: None,
        },
    )
    .await
    .unwrap();
    let messages: Vec<_> = logs.iter().map(|log| log.message.as_str()).collect();
    assert_eq!(messages, ["[lifecycle] match", "[restart] second prefix"]);
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
