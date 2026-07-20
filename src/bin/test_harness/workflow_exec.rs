//! Generic JSON-workflow step engine for the fault-recovery DSL, and its
//! two production entrypoints (`fault-retry-budget`, `fault-recovery-transient`).
//!
//! Every op executor here delegates to the same helpers the original
//! `fault_runner.rs` functions call directly; this module only adds the
//! step-sequencing, `${...}` template resolution, and expression evaluation
//! needed to drive those helpers from the on-disk JSON workflow manifests.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::process::Child;

use super::{
    GeneralizedSinkMetrics, GeneralizedSinkServer, HarnessPublisherProtocol, RampApi,
    RecoveryTransientCase, RetryBudgetCase, TestPorts, catalog, create_output,
    create_pipeline_with_stream_key, disconnect_grace_remaining_bounded, harness_catalog_root,
    health_input_snapshot, input_disconnect_cleared, spawn_publisher,
    start_generalized_sink_server, start_output, stop_child, stop_generalized_sink_server,
    stop_mixed_outputs, wait_for_api_input_live, wait_for_api_input_media_ready,
    wait_for_api_input_off, wait_for_sink_video_above,
};

#[path = "workflow_exec/expression.rs"]
mod expression;

// ── Workflow execution context ──────────────────────────────────────────────

struct WorkflowCtx<'a> {
    api: &'a RampApi,
    ports: &'a TestPorts,
    fixture_h264: &'a Path,
    protocol: HarnessPublisherProtocol,
    vars: serde_json::Map<String, Value>,
    publishers: HashMap<String, Child>,
    sinks: HashMap<String, (GeneralizedSinkServer, Arc<GeneralizedSinkMetrics>)>,
    pipeline_id: Option<String>,
    output_ids: Vec<String>,
    assert_id: Option<String>,
    assert_passed: Option<bool>,
    assert_failures: Vec<String>,
}

impl<'a> WorkflowCtx<'a> {
    fn new(
        api: &'a RampApi,
        ports: &'a TestPorts,
        fixture_h264: &'a Path,
        sink_port: u16,
        protocol: HarnessPublisherProtocol,
        case: Value,
    ) -> Self {
        let mut vars = serde_json::Map::new();
        vars.insert(
            "ports".to_string(),
            json!({ "http": ports.http, "rtmp": ports.rtmp, "srt": ports.srt, "sink": sink_port }),
        );
        vars.insert("case".to_string(), case);
        Self {
            api,
            ports,
            fixture_h264,
            protocol,
            vars,
            publishers: HashMap::new(),
            sinks: HashMap::new(),
            pipeline_id: None,
            output_ids: Vec::new(),
            assert_id: None,
            assert_passed: None,
            assert_failures: Vec::new(),
        }
    }

    fn set_var(&mut self, name: &str, value: Value) {
        self.vars.insert(name.to_string(), value);
    }

    fn resolve_path(&self, path: &str) -> Value {
        let mut parts = path.split('.');
        let Some(head) = parts.next() else {
            return Value::Null;
        };
        let Some(mut current) = self.vars.get(head).cloned() else {
            return Value::Null;
        };
        for part in parts {
            current = current.get(part).cloned().unwrap_or(Value::Null);
        }
        current
    }

    fn eval_expr(&self, expr: &str) -> Result<Value, String> {
        expression::evaluate(expr, &self.vars)
    }

    fn eval_bool(&self, expr: &str) -> Result<bool, String> {
        Ok(expression::truthy(&self.eval_expr(expr)?))
    }

    fn resolve_string(&self, s: &str) -> Result<Value, String> {
        if let Some(inner) = s.strip_prefix("${").and_then(|rest| rest.strip_suffix('}'))
            && !inner.contains("${")
        {
            return self.eval_expr(inner);
        }
        if !s.contains("${") {
            return Ok(Value::String(s.to_string()));
        }
        let mut result = String::new();
        let mut rest = s;
        while let Some(start) = rest.find("${") {
            result.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let end = after
                .find('}')
                .ok_or_else(|| format!("unterminated template in '{s}'"))?;
            let value = self.eval_expr(&after[..end])?;
            result.push_str(&match &value {
                Value::String(v) => v.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            });
            rest = &after[end + 1..];
        }
        result.push_str(rest);
        Ok(Value::String(result))
    }

    fn resolve_template(&self, raw: &Value) -> Result<Value, String> {
        match raw {
            Value::String(s) => self.resolve_string(s),
            Value::Array(items) => Ok(Value::Array(
                items
                    .iter()
                    .map(|item| self.resolve_template(item))
                    .collect::<Result<_, _>>()?,
            )),
            Value::Object(map) => {
                let mut out = serde_json::Map::with_capacity(map.len());
                for (k, v) in map {
                    out.insert(k.clone(), self.resolve_template(v)?);
                }
                Ok(Value::Object(out))
            }
            other => Ok(other.clone()),
        }
    }

    fn field(&self, obj: &Value, field: &str) -> Result<Value, String> {
        self.resolve_template(&obj[field])
    }

    fn field_str(&self, obj: &Value, field: &str) -> Result<String, String> {
        self.field(obj, field)?
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("field '{field}' did not resolve to a string"))
    }

    fn field_u64(&self, obj: &Value, field: &str) -> Result<u64, String> {
        let resolved = self.field(obj, field)?;
        resolved
            .as_u64()
            .or_else(|| resolved.as_f64().map(|f| f as u64))
            .ok_or_else(|| format!("field '{field}' did not resolve to a number"))
    }

    fn field_bool(&self, obj: &Value, field: &str) -> Result<bool, String> {
        Ok(expression::truthy(&self.field(obj, field)?))
    }

    fn into_result(self, test_name: &str) -> Value {
        let mut result = self.vars;
        result.insert("test".to_string(), json!(test_name));
        result.insert(
            "passed".to_string(),
            json!(self.assert_passed.unwrap_or(false)),
        );
        if let Some(id) = self.assert_id {
            result.insert("assertId".to_string(), json!(id));
        }
        if !self.assert_failures.is_empty() {
            result.insert("failedConditions".to_string(), json!(self.assert_failures));
        }
        Value::Object(result)
    }
}

// ── Step dispatch ────────────────────────────────────────────────────────

async fn run_step(ctx: &mut WorkflowCtx<'_>, step: &Value) -> Result<(), String> {
    if let Some(when) = step.get("when").and_then(Value::as_str)
        && !ctx.eval_bool(when)?
    {
        return Ok(());
    }
    let op = step["op"]
        .as_str()
        .ok_or("workflow step missing 'op'")?
        .to_string();
    let as_name = step.get("as").and_then(Value::as_str).map(str::to_string);

    match op.as_str() {
        "createPipeline" => {
            let name = ctx.field_str(step, "name")?;
            let stream_key = ctx.field_str(step, "streamKey")?;
            let pid = create_pipeline_with_stream_key(ctx.api, &name, &stream_key).await?;
            ctx.pipeline_id = Some(pid.clone());
            ctx.set_var(
                as_name.as_deref().unwrap_or("pipeline"),
                json!({ "id": pid }),
            );
        }
        "derive" => {
            let value = ctx.field(step, "value")?;
            ctx.set_var(as_name.as_deref().unwrap_or("derived"), value);
        }
        "startGeneralizedSink" => {
            let port = ctx.field_u64(step, "port")? as u16;
            let metrics = Arc::new(GeneralizedSinkMetrics::default());
            let server = start_generalized_sink_server(port, metrics.clone()).await?;
            let name = as_name.unwrap_or_else(|| "sink".to_string());
            ctx.sinks.insert(name, (server, metrics));
        }
        "createOutput" => {
            let pipeline_id = ctx.field_str(step, "pipeline")?;
            let name = ctx.field_str(step, "name")?;
            let url = if step.get("url").is_some() {
                ctx.field_str(step, "url")?
            } else {
                let dead_sink_port = ctx.field_u64(step, "deadSinkPort")? as u16;
                ctx.protocol.retry_limit_output_url(dead_sink_port)
            };
            let encoding = ctx.field_str(step, "encoding")?;
            let oid = create_output(ctx.api, &pipeline_id, &name, &url, &encoding).await?;
            ctx.output_ids.push(oid.clone());
            ctx.set_var(as_name.as_deref().unwrap_or("output"), json!({ "id": oid }));
        }
        "spawnPublisher" => {
            let pipeline_key = ctx.resolve_path("case.pipeline");
            let stream_key = pipeline_key
                .as_str()
                .ok_or("case.pipeline is not a string")?;
            let url = ctx.protocol.publish_url(ctx.ports, stream_key);
            let child = spawn_publisher(
                ctx.fixture_h264,
                &url,
                ctx.protocol.ffmpeg_format(),
                ctx.protocol.map_all_streams(),
            )
            .await?;
            let name = as_name.unwrap_or_else(|| "publisher".to_string());
            ctx.publishers.insert(name, child);
        }
        "waitApiInputLive" => {
            let pipeline_id = ctx.field_str(step, "pipeline")?;
            let timeout_secs = ctx.field_u64(step, "timeoutSecs")?;
            wait_for_api_input_live(ctx.api, &pipeline_id, Duration::from_secs(timeout_secs))
                .await?;
        }
        "startOutput" => {
            let pipeline_id = ctx.field_str(step, "pipeline")?;
            let output_id = ctx.field_str(step, "output")?;
            start_output(ctx.api, &pipeline_id, &output_id).await?;
        }
        "waitSinkVideoAbove" => {
            let sink_name = ctx.field_str(step, "sink")?;
            let threshold = ctx.field_u64(step, "threshold")?;
            let timeout_secs = ctx.field_u64(step, "timeoutSecs")?;
            let (_, metrics) = ctx
                .sinks
                .get(&sink_name)
                .ok_or_else(|| format!("unknown sink '{sink_name}'"))?;
            let _ =
                wait_for_sink_video_above(metrics, threshold, Duration::from_secs(timeout_secs))
                    .await;
        }
        "captureSinkMetrics" => {
            let sink_name = ctx.field_str(step, "sink")?;
            let (video, connections) = {
                let (_, metrics) = ctx
                    .sinks
                    .get(&sink_name)
                    .ok_or_else(|| format!("unknown sink '{sink_name}'"))?;
                (
                    metrics.video_count.load(Ordering::Relaxed),
                    metrics.connections.load(Ordering::Relaxed),
                )
            };
            ctx.set_var(
                as_name.as_deref().unwrap_or("metrics"),
                json!({ "video": video, "connections": connections }),
            );
        }
        "pollOutputStatus" => {
            let pipeline_id = ctx.field_str(step, "pipeline")?;
            let output_id = ctx.field_str(step, "output")?;
            let timeout_secs = ctx.field_u64(step, "timeoutSecs")?;
            let until = step["until"].clone();
            let until_field = ctx.field_str(&until, "field")?;
            let until_equals = ctx.field_str(&until, "equals")?;
            let capture: Vec<String> = step["capture"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();

            let deadline = Instant::now() + Duration::from_secs(timeout_secs);
            let mut saw_retrying = false;
            let mut saw_failed = false;
            let mut final_status = Value::Null;
            while Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let Some((status, status_json)) = ctx
                    .api
                    .get_output_status_or_not_found(&pipeline_id, &output_id)
                    .await?
                else {
                    continue;
                };
                if status.status == "retrying" {
                    saw_retrying = true;
                }
                if status.status == "failed" {
                    saw_failed = true;
                }
                let matched = status_json.get(&until_field).and_then(Value::as_str)
                    == Some(until_equals.as_str());
                final_status = status_json;
                if matched {
                    break;
                }
            }

            let mut captured = serde_json::Map::new();
            for key in capture {
                let value = match key.as_str() {
                    "sawRetrying" => json!(saw_retrying),
                    "sawFailed" => json!(saw_failed),
                    other => final_status[other].clone(),
                };
                captured.insert(key, value);
            }
            ctx.set_var(
                as_name.as_deref().unwrap_or("pollResult"),
                Value::Object(captured),
            );
        }
        "sleep" => {
            let millis = ctx.field_u64(step, "millis")?;
            tokio::time::sleep(Duration::from_millis(millis)).await;
        }
        "getOutputStatus" => {
            let pipeline_id = ctx.field_str(step, "pipeline")?;
            let output_id = ctx.field_str(step, "output")?;
            let timeout_secs = ctx.field_u64(step, "timeoutSecs")?;
            let deadline = Instant::now() + Duration::from_secs(timeout_secs);
            let status = loop {
                if let Some((_status, status_json)) = ctx
                    .api
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
            ctx.set_var(as_name.as_deref().unwrap_or("status"), status);
        }
        "getEngineHealth" => {
            let pipeline_id = ctx.field_str(step, "pipeline")?;
            let output_id = ctx.field_str(step, "output")?;
            let health = ctx.api.get_json("/api/v1/engine/health").await?;
            let output_health = health["pipelines"][&pipeline_id]["outputs"][&output_id].clone();
            ctx.set_var(
                as_name.as_deref().unwrap_or("health"),
                json!({ "output": output_health }),
            );
        }
        "waitOrSleepForInputDrop" => {
            let pipeline_id = ctx.field_str(step, "pipeline")?;
            let wait_input_off = ctx.field_bool(step, "waitInputOff")?;
            let timeout_secs = ctx.field_u64(step, "timeoutSecs")?;
            let sleep_millis = ctx.field_u64(step, "sleepMillis")?;
            let (gap_input, gap_off_error) = if wait_input_off {
                let off_result = wait_for_api_input_off(
                    ctx.api,
                    &pipeline_id,
                    Duration::from_secs(timeout_secs),
                )
                .await;
                let gap_off_error = off_result.err();
                let off_health = ctx.api.get_json("/api/v1/engine/health").await.ok();
                (
                    health_input_snapshot(off_health.as_ref(), &pipeline_id),
                    gap_off_error,
                )
            } else {
                tokio::time::sleep(Duration::from_millis(sleep_millis)).await;
                let gap_health = ctx.api.get_json("/api/v1/engine/health").await.ok();
                (
                    health_input_snapshot(gap_health.as_ref(), &pipeline_id),
                    None,
                )
            };
            ctx.set_var(
                as_name.as_deref().unwrap_or("gapWait"),
                json!({ "gapInput": gap_input, "gapOffError": gap_off_error }),
            );
        }
        "captureGapState" => {
            let pipeline_id = ctx.field_str(step, "pipeline")?;
            let output_id = ctx.field_str(step, "output")?;
            let sink_name = ctx.field_str(step, "sink")?;
            let gap_wait_name = ctx.field_str(step, "gapWait")?;
            let wait_input_off_after_drop = ctx.field_bool(step, "waitInputOffAfterDrop")?;
            let gap_wait = ctx.resolve_path(&gap_wait_name);
            let gap_input = gap_wait["gapInput"].clone();
            let gap_off_error_present = !gap_wait["gapOffError"].is_null();

            let gap_status = ctx
                .api
                .get_output_status(&pipeline_id, &output_id)
                .await
                .ok();
            let gap_connections = {
                let (_, metrics) = ctx
                    .sinks
                    .get(&sink_name)
                    .ok_or_else(|| format!("unknown sink '{sink_name}'"))?;
                metrics.connections.load(Ordering::Relaxed)
            };
            let gap_status_running =
                gap_status.as_ref().map(|(s, _)| s.status.as_str()) == Some("running");
            let gap_retrying = gap_status.as_ref().is_some_and(|(s, _)| s.retrying);
            let gap_has_error = gap_status
                .as_ref()
                .and_then(|(s, _)| s.last_error.as_deref())
                .is_some_and(|m| !m.is_empty());
            let gap_grace_active = gap_input["disconnectGraceActive"] == true;
            let gap_grace_remaining = disconnect_grace_remaining_bounded(&gap_input);
            let gap_input_off = !wait_input_off_after_drop
                || (!gap_off_error_present && gap_input["status"] == "off");

            ctx.set_var(
                as_name.as_deref().unwrap_or("gap"),
                json!({
                    "inputOff": gap_input_off,
                    "statusExists": gap_status.is_some(),
                    "connections": gap_connections,
                    "statusRunning": gap_status_running,
                    "retrying": gap_retrying,
                    "hasError": gap_has_error,
                    "graceActive": gap_grace_active,
                    "graceRemainingBounded": gap_grace_remaining,
                }),
            );
        }
        "waitResume" => {
            let pipeline_id = ctx.field_str(step, "pipeline")?;
            let sink_name = ctx.field_str(step, "sink")?;
            let media_ready_required = ctx.field_bool(step, "mediaReady")?;
            let video_above = ctx.field_u64(step, "videoAbove")?;
            let timeout_secs = ctx.field_u64(step, "timeoutSecs")?;

            let media_ready_ok = if media_ready_required {
                wait_for_api_input_media_ready(
                    ctx.api,
                    &pipeline_id,
                    Duration::from_secs(timeout_secs),
                )
                .await
                .is_ok()
            } else {
                wait_for_api_input_live(ctx.api, &pipeline_id, Duration::from_secs(timeout_secs))
                    .await?;
                true
            };
            let resumed_ok = {
                let (_, metrics) = ctx
                    .sinks
                    .get(&sink_name)
                    .ok_or_else(|| format!("unknown sink '{sink_name}'"))?;
                wait_for_sink_video_above(metrics, video_above, Duration::from_secs(15)).await
            };

            ctx.set_var(
                as_name.as_deref().unwrap_or("resumed"),
                json!({ "ok": resumed_ok, "mediaReadyOk": media_ready_ok }),
            );
        }
        "optionalSecondReconnect" => {
            let pipeline_id = ctx.field_str(step, "pipeline")?;
            let sink_name = ctx.field_str(step, "sink")?;
            let publisher_name = ctx.field_str(step, "publisher")?;
            let baseline_name = ctx.field_str(step, "baseline")?;
            let baseline_video = ctx.resolve_path(&baseline_name)["video"]
                .as_u64()
                .unwrap_or(0);

            if let Some(mut child) = ctx.publishers.remove(&publisher_name) {
                stop_child(&mut child).await;
            }
            tokio::time::sleep(Duration::from_millis(2_500)).await;
            let second_gap_health = ctx.api.get_json("/api/v1/engine/health").await.ok();
            let second_gap_input = health_input_snapshot(second_gap_health.as_ref(), &pipeline_id);
            let second_gap_grace_active = second_gap_input["disconnectGraceActive"] == true;
            let second_gap_grace_remaining = disconnect_grace_remaining_bounded(&second_gap_input);

            let pipeline_key = ctx.resolve_path("case.pipeline");
            let stream_key = pipeline_key
                .as_str()
                .ok_or("case.pipeline is not a string")?
                .to_string();
            let url = ctx.protocol.publish_url(ctx.ports, &stream_key);
            let new_child = spawn_publisher(
                ctx.fixture_h264,
                &url,
                ctx.protocol.ffmpeg_format(),
                ctx.protocol.map_all_streams(),
            )
            .await?;
            wait_for_api_input_live(ctx.api, &pipeline_id, Duration::from_secs(30)).await?;
            let second_resumed = {
                let (_, metrics) = ctx
                    .sinks
                    .get(&sink_name)
                    .ok_or_else(|| format!("unknown sink '{sink_name}'"))?;
                wait_for_sink_video_above(metrics, baseline_video + 20, Duration::from_secs(15))
                    .await
            };
            ctx.publishers.insert(publisher_name, new_child);

            ctx.set_var(
                as_name.as_deref().unwrap_or("secondReconnect"),
                json!({
                    "graceActive": second_gap_grace_active,
                    "graceRemainingBounded": second_gap_grace_remaining,
                    "resumed": second_resumed,
                }),
            );
        }
        "captureFinalState" => {
            let pipeline_id = ctx.field_str(step, "pipeline")?;
            let output_id = ctx.field_str(step, "output")?;
            let sink_name = ctx.field_str(step, "sink")?;
            let final_connections = {
                let (_, metrics) = ctx
                    .sinks
                    .get(&sink_name)
                    .ok_or_else(|| format!("unknown sink '{sink_name}'"))?;
                metrics.connections.load(Ordering::Relaxed)
            };
            let final_status = ctx
                .api
                .get_output_status(&pipeline_id, &output_id)
                .await
                .ok();
            let final_status_running =
                final_status.as_ref().map(|(s, _)| s.status.as_str()) == Some("running");
            let final_retrying = final_status.as_ref().is_some_and(|(s, _)| s.retrying);
            let final_health = ctx.api.get_json("/api/v1/engine/health").await.ok();
            let final_input = health_input_snapshot(final_health.as_ref(), &pipeline_id);
            let final_disconnect_cleared = input_disconnect_cleared(&final_input);
            let final_flapping = final_input["flapping"] == true;
            let final_recent_disconnect_count =
                final_input["recentDisconnectCount"].as_u64().unwrap_or(0);

            ctx.set_var(
                as_name.as_deref().unwrap_or("final"),
                json!({
                    "connections": final_connections,
                    "statusRunning": final_status_running,
                    "retrying": final_retrying,
                    "disconnectCleared": final_disconnect_cleared,
                    "flapping": final_flapping,
                    "recentDisconnectCount": final_recent_disconnect_count,
                }),
            );
        }
        "assert" => {
            let id = step["id"].as_str().unwrap_or_default().to_string();
            let mut failures = Vec::new();
            for condition in step["all"].as_array().cloned().unwrap_or_default() {
                let Some(condition) = condition.as_str() else {
                    continue;
                };
                if !ctx.eval_bool(condition)? {
                    failures.push(condition.to_string());
                }
            }
            ctx.assert_id = Some(id);
            ctx.assert_passed = Some(failures.is_empty());
            ctx.assert_failures = failures;
        }
        "stop" => {
            for target in step["targets"].as_array().cloned().unwrap_or_default() {
                let Some(target) = target.as_str() else {
                    continue;
                };
                if target == "outputs" {
                    if let Some(pipeline_id) = ctx.pipeline_id.clone() {
                        stop_mixed_outputs(ctx.api, &pipeline_id, &ctx.output_ids).await;
                    }
                } else if let Some((server, _)) = ctx.sinks.remove(target) {
                    stop_generalized_sink_server(server);
                } else if let Some(mut child) = ctx.publishers.remove(target) {
                    stop_child(&mut child).await;
                }
            }
        }
        other => return Err(format!("unsupported workflow op '{other}'")),
    }
    Ok(())
}

async fn run_workflow(ctx: &mut WorkflowCtx<'_>, steps: &[Value]) -> Result<(), String> {
    for step in steps {
        run_step(ctx, step).await?;
    }
    Ok(())
}

fn load_workflow_steps(workflow_key: &str) -> Result<Vec<Value>, String> {
    let root = harness_catalog_root();
    let catalog = catalog::read_json_file(&root.join("catalog.json"))?;
    let rel_path = catalog
        .pointer(&format!("/workflows/{workflow_key}"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("catalog.json missing workflows.{workflow_key}"))?;
    let workflow = catalog::read_json_file(&root.join(rel_path))?;
    workflow["steps"]
        .as_array()
        .cloned()
        .ok_or_else(|| format!("workflow {workflow_key} missing a 'steps' array"))
}

// ── Production entrypoints ──────────────────────────────────────────────────

pub(super) async fn run_retry_budget_case_via_workflow(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    case: &RetryBudgetCase,
) -> Result<Value, String> {
    let steps = load_workflow_steps("faultRetryBudget")?;
    let case_value = serde_json::to_value(case).map_err(|e| e.to_string())?;
    let mut ctx = WorkflowCtx::new(
        api,
        ports,
        fixture_h264,
        sink_port,
        case.protocol,
        case_value,
    );
    run_workflow(&mut ctx, &steps).await?;
    Ok(ctx.into_result(&case.test_name))
}

pub(super) async fn run_recovery_transient_case_via_workflow(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    case: &RecoveryTransientCase,
) -> Result<Value, String> {
    let steps = load_workflow_steps("faultRecoveryTransient")?;
    let case_value = serde_json::to_value(case).map_err(|e| e.to_string())?;
    let mut ctx = WorkflowCtx::new(
        api,
        ports,
        fixture_h264,
        sink_port,
        case.protocol,
        case_value,
    );
    run_workflow(&mut ctx, &steps).await?;
    Ok(ctx.into_result(&case.test_name))
}
