use serde_json::{Value, json};

use crate::RampApi;

pub(crate) async fn get_logs(api: &RampApi, query: &str) -> Result<Vec<Value>, String> {
    let response = api.get_json(&format!("/api/v1/logs?{query}")).await?;
    response["logs"]
        .as_array()
        .cloned()
        .ok_or_else(|| format!("logs response missing array for query: {query}"))
}

fn log_event_type(log: &Value) -> Option<&str> {
    log["eventType"].as_str()
}

fn log_target(log: &Value) -> Option<&str> {
    log["target"].as_str()
}

fn log_message(log: &Value) -> Option<&str> {
    log["message"].as_str()
}

fn log_pipeline_id(log: &Value) -> Option<&str> {
    log["pipelineId"].as_str()
}

pub(crate) fn parse_log_fields(log: &Value) -> Option<Value> {
    let fields = log.get("fields")?;
    match fields {
        Value::Object(_) => Some(fields.clone()),
        Value::String(raw) if !raw.trim().is_empty() => serde_json::from_str(raw).ok(),
        _ => None,
    }
}

pub(crate) fn log_has_correlation_id(log: &Value) -> bool {
    parse_log_fields(log)
        .and_then(|fields| {
            fields
                .get("correlation_id")
                .and_then(|value| value.as_str())
                .or_else(|| fields.get("correlationId").and_then(|value| value.as_str()))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .is_some()
}

fn logs_contain_event(logs: &[Value], event_type: &str) -> bool {
    logs.iter()
        .any(|log| log_event_type(log) == Some(event_type))
}

pub(crate) async fn verify_api_smoke_history_contract(api: &RampApi) -> Result<Value, String> {
    let lifecycle_logs = get_logs(api, "event_class=lifecycle&limit=50&order=desc").await?;

    Ok(json!({
        "logsEndpointOk": true,
        "logCount": lifecycle_logs.len(),
    }))
}

pub(crate) async fn verify_live_history_contract(
    api: &RampApi,
    expected_event_types: &[&str],
) -> Result<Value, String> {
    let all_logs = get_logs(api, "limit=2000&order=desc").await?;

    let pipeline_logs: Vec<Value> = all_logs
        .iter()
        .filter(|log| log_pipeline_id(log).is_some())
        .cloned()
        .collect();
    if pipeline_logs.is_empty() {
        return Err("live history contract found no pipeline-scoped logs".to_string());
    }

    let missing_event_types: Vec<&str> = expected_event_types
        .iter()
        .copied()
        .filter(|event_type| !logs_contain_event(&pipeline_logs, event_type))
        .collect();
    if !missing_event_types.is_empty() {
        return Err(format!(
            "live history contract missing lifecycle events: {}",
            missing_event_types.join(", ")
        ));
    }

    let correlated_pipeline_log_count = pipeline_logs
        .iter()
        .filter(|log| log_has_correlation_id(log))
        .count();

    let ext_transcoder_logs: Vec<Value> = pipeline_logs
        .iter()
        .filter(|log| {
            log_target(log).is_some_and(|target| target.contains("external_transcoder"))
                || log_message(log).is_some_and(|message| message.contains("[ext-transcoder]"))
        })
        .cloned()
        .collect();
    let ext_transcoder_correlated = ext_transcoder_logs.iter().any(log_has_correlation_id);

    Ok(json!({
        "pipelineLogCount": pipeline_logs.len(),
        "expectedEventTypes": expected_event_types,
        "correlatedPipelineLogCount": correlated_pipeline_log_count,
        "externalTranscoderLogCount": ext_transcoder_logs.len(),
        "externalTranscoderCorrelated": ext_transcoder_correlated,
    }))
}

pub(crate) async fn verify_external_transcoder_history_contract(
    api: &RampApi,
) -> Result<Value, String> {
    let logs = get_logs(
        api,
        "target=restream::media::external_transcoder&limit=200&order=desc",
    )
    .await?;

    if logs.is_empty() {
        return Err(
            "external transcoder history contract found no restream::media::external_transcoder logs"
                .to_string(),
        );
    }

    let correlated_log_count = logs
        .iter()
        .filter(|log| log_has_correlation_id(log))
        .count();
    if correlated_log_count == 0 {
        return Err(
            "external transcoder history contract found no correlated stage logs".to_string(),
        );
    }

    Ok(json!({
        "targetLogCount": logs.len(),
        "correlatedLogCount": correlated_log_count,
    }))
}
