//! Scheduled events over HTTP, and the scheduler task that runs them.
//!
//! The rules of when an event runs live in `application::schedules`; this
//! module runs the action (desired state for destinations, or the file
//! source) and records how it went on the schedule and in the audit log.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use std::path::Path as FsPath;
use std::sync::Arc;
use tracing::{info, warn};

use super::error::ApiError;
use super::state::{AppState, require_authenticated, to_hex};
use crate::application::schedules::{self, Due, Schedule, ScheduleAction, ScheduleDraft};
use crate::application::team::{AuditEntry, Role};

/// How often the scheduler looks for due events: well inside the grace.
pub const SCHEDULER_TICK: std::time::Duration = std::time::Duration::from_secs(5);

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Runs one schedule's action now; the summary says what it did.
pub async fn execute(state: &AppState, schedule: &Schedule) -> Result<String, String> {
    match schedule.action {
        ScheduleAction::StartOutputs | ScheduleAction::StopOutputs => {
            let start = schedule.action == ScheduleAction::StartOutputs;
            let outputs =
                crate::application::outputs::list_for_pipeline(&state.db, &schedule.pipeline_id)
                    .await
                    .map_err(|err| err.to_string())?;
            let targets: Vec<_> = outputs
                .iter()
                .filter(|output| {
                    schedule.output_ids.is_empty() || schedule.output_ids.contains(&output.id)
                })
                .collect();
            if targets.is_empty() {
                return Err("no destinations to act on".to_string());
            }
            let mut failed = 0usize;
            for output in &targets {
                let result = if start {
                    crate::application::outputs::request_start(
                        &state.db,
                        &schedule.pipeline_id,
                        &output.id,
                    )
                    .await
                } else {
                    crate::application::outputs::request_stop(
                        &state.db,
                        &schedule.pipeline_id,
                        &output.id,
                    )
                    .await
                };
                if result.is_err() {
                    failed += 1;
                }
            }
            state.reconcile.wake();
            state.runtime_state.mark_changed();
            let verb = if start { "wanted running" } else { "stopped" };
            if failed == targets.len() {
                Err(format!("0 of {} destinations {verb}", targets.len()))
            } else {
                Ok(format!(
                    "{} of {} destinations {verb}",
                    targets.len() - failed,
                    targets.len()
                ))
            }
        }
        ScheduleAction::StartFile | ScheduleAction::StopFile => {
            let pipeline = state
                .file_ingest_service
                .get_pipeline(&schedule.pipeline_id)
                .await
                .map_err(|err| err.to_string())?;
            let file = state
                .file_ingest_service
                .load_pipeline_file_ingest_state(&state.engine, &pipeline)
                .await
                .map_err(|err| err.to_string())?;
            let Some(ingest) = file.ingest else {
                return Err("the pipeline has no file source".to_string());
            };
            if schedule.action == ScheduleAction::StartFile {
                if file.running {
                    return Ok("the file source was already playing".to_string());
                }
                state
                    .file_ingest_service
                    .start_ingest(
                        state.engine.clone(),
                        FsPath::new(&state.media_dir),
                        &ingest.id,
                    )
                    .await
                    .map_err(|err| format!("could not start the file source: {err:?}"))?;
                Ok(format!("started {}", ingest.files().join(", ")))
            } else {
                state
                    .file_ingest_service
                    .stop_ingest_with_runtime_cleanup(&state.engine, &ingest.id)
                    .await
                    .map_err(|err| err.to_string())?;
                Ok("stopped the file source".to_string())
            }
        }
    }
}

async fn audit_run(state: &AppState, schedule: &Schedule, ok: bool) {
    let entry = AuditEntry {
        id: 0,
        at_ms: now_ms(),
        username: "scheduler".to_string(),
        role: Role::Operator,
        method: "RUN".to_string(),
        path: format!("/api/v1/schedules/{}", schedule.id),
        status: if ok { 200 } else { 500 },
    };
    if let Err(error) = state.team_service.record(&entry).await {
        warn!(err = %error, "failed to audit a schedule run");
    }
}

/// Runs, records and audits one occurrence.
async fn run_occurrence(state: &AppState, schedule: &Schedule, occurrence_ms: i64) {
    // Recorded first: a slow action must not be started twice.
    let _ = schedules::record_run(&state.db, &schedule.id, occurrence_ms, "running").await;
    let outcome = execute(state, schedule).await;
    let (ok, result) = match &outcome {
        Ok(summary) => (true, format!("ok: {summary}")),
        Err(error) => (false, format!("failed: {error}")),
    };
    info!(schedule = %schedule.id, name = %schedule.name, result = %result, "scheduled event ran");
    if let Err(error) = schedules::record_run(&state.db, &schedule.id, occurrence_ms, &result).await
    {
        warn!(err = %error, "failed to record a schedule run");
    }
    audit_run(state, schedule, ok).await;
}

/// Looks for due events every `tick` and runs them; an occurrence missed by
/// more than the grace (the engine was down) is recorded as skipped.
pub async fn run_scheduler(
    state: Arc<AppState>,
    tick: std::time::Duration,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let mut ticks = tokio::time::interval(tick);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = ticks.tick() => {}
        }
        let now = now_ms();
        let Ok(all) = schedules::list(&state.db, now).await else {
            continue;
        };
        for schedule in all {
            match schedules::due(
                schedule.at_ms,
                schedule.repeat,
                schedule.enabled,
                schedule.last_run_ms,
                now,
            ) {
                Due::Nothing => {}
                Due::Run(occurrence) => run_occurrence(&state, &schedule, occurrence).await,
                Due::Skip(occurrence) => {
                    let missed = chrono::DateTime::from_timestamp_millis(occurrence)
                        .map(|at| at.format("%Y-%m-%d %H:%M UTC").to_string())
                        .unwrap_or_default();
                    let _ = schedules::record_run(
                        &state.db,
                        &schedule.id,
                        occurrence,
                        &format!("skipped: the engine was not running at {missed}"),
                    )
                    .await;
                }
            }
        }
    }
}

fn result<T: serde::Serialize>(
    value: Result<T, crate::application::services::ServiceError>,
) -> Response {
    match value {
        Ok(value) => Json(value).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// `GET /api/v1/schedules`: every schedule with its next occurrence.
pub async fn list_schedules_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    result(
        schedules::list(&state.db, now_ms())
            .await
            .map(|list| serde_json::json!({ "schedules": list })),
    )
}

/// `POST /api/v1/schedules`.
pub async fn create_schedule_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(draft): Json<ScheduleDraft>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    if let Err(error) = state
        .file_ingest_service
        .get_pipeline(&draft.pipeline_id)
        .await
    {
        return ApiError::from(error).into_response();
    }
    use rand::RngExt;
    let mut id = [0u8; 8];
    rand::rng().fill(&mut id);
    match schedules::create(&state.db, &format!("sch_{}", to_hex(&id)), &draft, now_ms()).await {
        Ok(schedule) => (StatusCode::CREATED, Json(schedule)).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// `PUT /api/v1/schedules/{id}`.
pub async fn update_schedule_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(draft): Json<ScheduleDraft>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    result(schedules::update(&state.db, &id, &draft, now_ms()).await)
}

/// `DELETE /api/v1/schedules/{id}`.
pub async fn delete_schedule_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    result(
        schedules::delete(&state.db, &id)
            .await
            .map(|()| serde_json::json!({ "deleted": true })),
    )
}

/// `POST /api/v1/schedules/{id}/run`: runs the action now, outside its
/// times (they are unchanged), and answers how it went.
pub async fn run_schedule_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    let schedule = match schedules::get(&state.db, &id, now_ms()).await {
        Ok(schedule) => schedule,
        Err(error) => return ApiError::from(error).into_response(),
    };
    match execute(&state, &schedule).await {
        Ok(summary) => Json(serde_json::json!({ "ok": true, "result": summary })).into_response(),
        Err(error) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "ok": false, "error": error })),
        )
            .into_response(),
    }
}
