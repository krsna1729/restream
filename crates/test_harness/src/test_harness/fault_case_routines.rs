//! Retry-budget and transient-recovery fault cases, one typed routine each.
//!
//! Each routine returns the case's recorded variables plus `test`, `passed`,
//! `assertId` and, when any check fails, `failedConditions` (the labels of the
//! failed checks).

use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::{
    GeneralizedSinkMetrics, RampApi, RecoveryTransientCase, RetryBudgetCase, TestPorts,
    create_output, create_pipeline_with_stream_key, disconnect_grace_remaining_bounded,
    health_input_snapshot, input_disconnect_cleared, spawn_publisher,
    start_generalized_sink_server, start_output, stop_child, stop_generalized_sink_server,
    stop_mixed_outputs, wait_for_api_input_live, wait_for_api_input_media_ready,
    wait_for_api_input_off, wait_for_sink_video_above,
};

/// Adds the verdict of `checks` to `result`: `passed` is true only when every
/// check holds, and `failedConditions` lists the labels of those that do not.
fn with_verdict(
    mut result: Value,
    test_name: &str,
    assert_id: &str,
    checks: &[(&str, bool)],
) -> Value {
    let failures: Vec<&str> = checks
        .iter()
        .filter(|(_, held)| !held)
        .map(|(label, _)| *label)
        .collect();
    result["test"] = json!(test_name);
    result["passed"] = json!(failures.is_empty());
    result["assertId"] = json!(assert_id);
    if !failures.is_empty() {
        result["failedConditions"] = json!(failures);
    }
    result
}

fn ports_json(ports: &TestPorts, sink_port: u16) -> Value {
    json!({ "http": ports.http, "rtmp": ports.rtmp, "srt": ports.srt, "sink": sink_port })
}

fn at_least(value: &Value, floor: f64) -> bool {
    value.as_f64().is_some_and(|value| value >= floor)
}

/// An output pointed at a dead sink must retry, then settle in `failed` with
/// its retry state cleared.
pub(super) async fn run_retry_budget_case(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    case: &RetryBudgetCase,
) -> Result<Value, String> {
    let protocol = case.protocol;
    let pipeline_id = create_pipeline_with_stream_key(api, &case.pipeline, &case.pipeline).await?;
    let dead_sink_port = sink_port + case.dead_sink_offset;
    let output_id = create_output(
        api,
        &pipeline_id,
        &case.output_name,
        &protocol.retry_limit_output_url(dead_sink_port),
        "source",
    )
    .await?;
    let mut publisher = spawn_publisher(
        fixture_h264,
        &protocol.publish_url(ports, &case.pipeline),
        protocol.ffmpeg_format(),
        protocol.map_all_streams(),
    )
    .await?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(15)).await?;
    start_output(api, &pipeline_id, &output_id).await?;

    // Poll until the output has retried and then failed with no retry pending.
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut saw_retrying = false;
    let mut saw_failed = false;
    let mut final_status = Value::Null;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let Some((status, status_json)) = api
            .get_output_status_or_not_found(&pipeline_id, &output_id)
            .await?
        else {
            continue;
        };
        if status.status == "retrying" || status.retrying || status.retry_attempts.is_some() {
            saw_retrying = true;
        }
        if status.status == "failed" {
            saw_failed = true;
        }
        let exhausted = status.status == "failed"
            && !status.retrying
            && status.retry_attempts.is_none()
            && saw_retrying;
        final_status = status_json;
        if exhausted {
            break;
        }
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    let deadline = Instant::now() + Duration::from_secs(2);
    let stable = loop {
        if let Some((_, status_json)) = api
            .get_output_status_or_not_found(&pipeline_id, &output_id)
            .await?
        {
            break status_json;
        }
        if Instant::now() >= deadline {
            return Err(format!("output {output_id} status never became visible"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let health = api.get_json("/api/v1/engine/health").await?;
    let output_health = health["pipelines"][&pipeline_id]["outputs"][&output_id].clone();

    let checks = [
        (
            "retryStatus.sawRetrying || stableStatus.recentFailureCount >= 2 || health.output.recentFailureCount >= 2",
            saw_retrying
                || at_least(&stable["recentFailureCount"], 2.0)
                || at_least(&output_health["recentFailureCount"], 2.0),
        ),
        ("retryStatus.sawFailed", saw_failed),
        (
            "stableStatus.status == 'failed'",
            stable["status"] == "failed",
        ),
        (
            "health.output.status == 'failed'",
            output_health["status"] == "failed",
        ),
        (
            "stableStatus.retrying == false",
            stable["retrying"] == false,
        ),
        (
            "stableStatus.retryAttempts == null",
            stable["retryAttempts"].is_null(),
        ),
        (
            "stableStatus.retryBackoffMs == null",
            stable["retryBackoffMs"].is_null(),
        ),
        (
            "stableStatus.retryRemainingMs == null",
            stable["retryRemainingMs"].is_null(),
        ),
    ];
    let result = json!({
        "ports": ports_json(ports, sink_port),
        "case": serde_json::to_value(case).map_err(|e| e.to_string())?,
        "pipeline": { "id": pipeline_id },
        "deadSinkPort": dead_sink_port,
        "output": { "id": output_id },
        "retryStatus": {
            "sawRetrying": saw_retrying,
            "sawFailed": saw_failed,
            "retryAttempts": final_status["retryAttempts"],
            "retryBackoffMs": final_status["retryBackoffMs"],
        },
        "stableStatus": stable,
        "health": { "output": output_health },
    });
    let result = with_verdict(result, &case.test_name, "retry-budget-exhausted", &checks);

    stop_child(&mut publisher).await;
    stop_mixed_outputs(api, &pipeline_id, &[output_id]).await;
    Ok(result)
}

/// A brief publisher drop must keep the egress connection running through the
/// disconnect grace and resume delivery when the publisher returns.
pub(super) async fn run_recovery_transient_case(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    case: &RecoveryTransientCase,
) -> Result<Value, String> {
    let protocol = case.protocol;
    let publish_url = protocol.publish_url(ports, &case.pipeline);
    let spawn = || {
        spawn_publisher(
            fixture_h264,
            &publish_url,
            protocol.ffmpeg_format(),
            protocol.map_all_streams(),
        )
    };
    let pipeline_id = create_pipeline_with_stream_key(api, &case.pipeline, &case.pipeline).await?;
    let metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink = start_generalized_sink_server(sink_port, metrics.clone()).await?;
    let output_id = create_output(
        api,
        &pipeline_id,
        &case.output_name,
        &format!("rtmp://127.0.0.1:{sink_port}/live/{}", case.sink_stream),
        "source",
    )
    .await?;
    let mut publisher = spawn().await?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(15)).await?;
    start_output(api, &pipeline_id, &output_id).await?;
    let _ = wait_for_sink_video_above(&metrics, 9, Duration::from_secs(15)).await;
    let baseline_video = metrics.video_count.load(Ordering::Relaxed);
    let baseline_connections = metrics.connections.load(Ordering::Relaxed);
    stop_child(&mut publisher).await;

    // The input-drop gap: egress must stay connected and running.
    let (gap_input, gap_off_error) = if case.wait_input_off_after_drop {
        let gap_off_error = wait_for_api_input_off(api, &pipeline_id, Duration::from_secs(10))
            .await
            .err();
        let health = api.get_json("/api/v1/engine/health").await.ok();
        (
            health_input_snapshot(health.as_ref(), &pipeline_id),
            gap_off_error,
        )
    } else {
        tokio::time::sleep(Duration::from_millis(2_500)).await;
        let health = api.get_json("/api/v1/engine/health").await.ok();
        (health_input_snapshot(health.as_ref(), &pipeline_id), None)
    };
    let gap_status = api.get_output_status(&pipeline_id, &output_id).await.ok();
    let gap_connections = metrics.connections.load(Ordering::Relaxed);
    let gap_status_running = gap_status.as_ref().map(|(s, _)| s.status.as_str()) == Some("running");
    let gap_retrying = gap_status.as_ref().is_some_and(|(s, _)| s.retrying);
    let gap_has_error = gap_status
        .as_ref()
        .and_then(|(s, _)| s.last_error.as_deref())
        .is_some_and(|m| !m.is_empty());
    let gap_grace_active = gap_input["disconnectGraceActive"] == true;
    let gap_grace_remaining = disconnect_grace_remaining_bounded(&gap_input);
    let gap_input_off = !case.wait_input_off_after_drop
        || (gap_off_error.is_none() && gap_input["status"] == "off");

    let mut publisher = spawn().await?;
    let timeout = Duration::from_secs(30);
    let media_ready_ok = if case.require_media_ready_on_resume {
        wait_for_api_input_media_ready(api, &pipeline_id, timeout)
            .await
            .is_ok()
    } else {
        wait_for_api_input_live(api, &pipeline_id, timeout).await?;
        true
    };
    let resumed_ok =
        wait_for_sink_video_above(&metrics, baseline_video + 10, Duration::from_secs(15)).await;

    // A second drop inside the flap window must mark the input flapping.
    let second_reconnect = if case.second_reconnect_checks_flapping {
        stop_child(&mut publisher).await;
        tokio::time::sleep(Duration::from_millis(2_500)).await;
        let health = api.get_json("/api/v1/engine/health").await.ok();
        let input = health_input_snapshot(health.as_ref(), &pipeline_id);
        let grace_active = input["disconnectGraceActive"] == true;
        let grace_remaining = disconnect_grace_remaining_bounded(&input);
        publisher = spawn().await?;
        wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(30)).await?;
        let resumed =
            wait_for_sink_video_above(&metrics, baseline_video + 20, Duration::from_secs(15)).await;
        Some((grace_active, grace_remaining, resumed))
    } else {
        None
    };

    let final_connections = metrics.connections.load(Ordering::Relaxed);
    let final_status = api.get_output_status(&pipeline_id, &output_id).await.ok();
    let final_status_running =
        final_status.as_ref().map(|(s, _)| s.status.as_str()) == Some("running");
    let final_retrying = final_status.as_ref().is_some_and(|(s, _)| s.retrying);
    let final_health = api.get_json("/api/v1/engine/health").await.ok();
    let final_input = health_input_snapshot(final_health.as_ref(), &pipeline_id);
    let final_disconnect_cleared = input_disconnect_cleared(&final_input);
    let final_flapping = final_input["flapping"] == true;
    let final_recent_disconnect_count = final_input["recentDisconnectCount"].as_u64().unwrap_or(0);

    let checks = [
        ("baseline.video >= 10", baseline_video >= 10),
        ("baseline.connections == 1", baseline_connections == 1),
        ("gap.inputOff == true", gap_input_off),
        ("gap.statusExists == true", gap_status.is_some()),
        (
            "gap.connections == baseline.connections",
            gap_connections == baseline_connections,
        ),
        ("gap.statusRunning == true", gap_status_running),
        ("gap.retrying == false", !gap_retrying),
        ("gap.hasError == false", !gap_has_error),
        ("gap.graceActive == true", gap_grace_active),
        ("gap.graceRemainingBounded == true", gap_grace_remaining),
        ("resumed.ok == true", resumed_ok),
        ("resumed.mediaReadyOk == true", media_ready_ok),
        (
            "final.connections == baseline.connections",
            final_connections == baseline_connections,
        ),
        ("final.statusRunning == true", final_status_running),
        ("final.retrying == false", !final_retrying),
        ("final.disconnectCleared == true", final_disconnect_cleared),
        (
            "!case.secondReconnectChecksFlapping || (secondReconnect.graceActive == true && secondReconnect.graceRemainingBounded == true && secondReconnect.resumed == true && final.flapping == true && final.recentDisconnectCount >= 2)",
            second_reconnect.is_none_or(|(grace_active, grace_remaining, resumed)| {
                grace_active
                    && grace_remaining
                    && resumed
                    && final_flapping
                    && final_recent_disconnect_count >= 2
            }),
        ),
    ];
    let mut result = json!({
        "ports": ports_json(ports, sink_port),
        "case": serde_json::to_value(case).map_err(|e| e.to_string())?,
        "pipeline": { "id": pipeline_id },
        "output": { "id": output_id },
        "baseline": { "video": baseline_video, "connections": baseline_connections },
        "gapWait": { "gapInput": gap_input, "gapOffError": gap_off_error },
        "gap": {
            "inputOff": gap_input_off,
            "statusExists": gap_status.is_some(),
            "connections": gap_connections,
            "statusRunning": gap_status_running,
            "retrying": gap_retrying,
            "hasError": gap_has_error,
            "graceActive": gap_grace_active,
            "graceRemainingBounded": gap_grace_remaining,
        },
        "resumed": { "ok": resumed_ok, "mediaReadyOk": media_ready_ok },
        "final": {
            "connections": final_connections,
            "statusRunning": final_status_running,
            "retrying": final_retrying,
            "disconnectCleared": final_disconnect_cleared,
            "flapping": final_flapping,
            "recentDisconnectCount": final_recent_disconnect_count,
        },
    });
    if let Some((grace_active, grace_remaining, resumed)) = second_reconnect {
        result["secondReconnect"] = json!({
            "graceActive": grace_active,
            "graceRemainingBounded": grace_remaining,
            "resumed": resumed,
        });
    }
    let result = with_verdict(
        result,
        &case.test_name,
        "transient-recovery-preserved-egress",
        &checks,
    );

    stop_child(&mut publisher).await;
    stop_generalized_sink_server(sink);
    stop_mixed_outputs(api, &pipeline_id, &[output_id]).await;
    Ok(result)
}
