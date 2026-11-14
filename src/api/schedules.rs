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
use futures_util::FutureExt as _;
use std::collections::HashMap;
use std::path::Path as FsPath;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use super::error::ApiError;
use super::state::{AppState, require_authenticated, to_hex};
use crate::application::schedules::{self, Due, Schedule, ScheduleAction, ScheduleDraft};
use crate::application::team::AuditEntry;
use crate::db::DeadlineDb;
use restream_domain::domain::role::Role;

/// How often the scheduler looks for due events: well inside the grace.
pub const SCHEDULER_TICK: Duration = Duration::from_secs(5);

/// How long an occurrence's action may run before its schedule says so:
/// the row then reads "running: …" until the action ends. The action is
/// never cut short; a start or stop abandoned halfway would leave the
/// pipeline in neither state.
pub const ACTION_DEADLINE: Duration = Duration::from_secs(30);

/// How long shutdown waits for occurrences still running; one still running
/// then is aborted and recorded `interrupted: shutdown` (not run again).
pub const SHUTDOWN_DRAIN: Duration = Duration::from_secs(10);

/// A running occurrence's schedule, and its time when it runs an action.
type Running = HashMap<tokio::task::Id, (String, Option<i64>)>;

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
async fn run_occurrence(
    state: Arc<AppState>,
    db: DeadlineDb,
    schedule: Schedule,
    occurrence_ms: i64,
    action_deadline: Duration,
) {
    // Recorded first: a slow action must not be started twice. The record
    // is awaited to its outcome (behind another writer this stalls only
    // this occurrence); only a record that failed skips the action, and then
    // the row still says the occurrence is due, so the next tick runs it.
    if let Err(error) = record_run(&db, &schedule.id, occurrence_ms, "running".to_string()).await {
        warn!(schedule = %schedule.id, err = %error, "could not record a schedule run; retried next tick");
        return;
    }
    // A panicking action is this occurrence's failure, recorded as one.
    let action = std::panic::AssertUnwindSafe(execute(&state, &schedule))
        .catch_unwind()
        .map(|outcome| outcome.unwrap_or_else(|_| Err("the action panicked".to_string())));
    tokio::pin!(action);
    let outcome = match tokio::time::timeout(action_deadline, &mut action).await {
        Ok(outcome) => outcome,
        Err(_) => {
            let note = format!(
                "running: not finished after {} s",
                action_deadline.as_secs()
            );
            warn!(schedule = %schedule.id, name = %schedule.name, "{note}");
            if let Err(error) = record_run(&db, &schedule.id, occurrence_ms, note).await {
                warn!(err = %error, "failed to record a slow schedule run");
            }
            action.await
        }
    };
    let (ok, result) = match &outcome {
        Ok(summary) => (true, format!("ok: {summary}")),
        Err(error) => (false, format!("failed: {error}")),
    };
    info!(schedule = %schedule.id, name = %schedule.name, result = %result, "scheduled event ran");
    if let Err(error) = record_run(&db, &schedule.id, occurrence_ms, result).await {
        warn!(err = %error, "failed to record a schedule run");
    }
    audit_run(&state, &schedule, ok).await;
}

/// Records that an occurrence was missed by more than the grace.
async fn record_skip(db: DeadlineDb, schedule_id: String, occurrence_ms: i64) {
    let missed = chrono::DateTime::from_timestamp_millis(occurrence_ms)
        .map(|at| at.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_default();
    let _ = record_run(
        &db,
        &schedule_id,
        occurrence_ms,
        format!("skipped: the engine was not running at {missed}"),
    )
    .await;
}

/// Records an occurrence's outcome, awaited to its outcome.
async fn record_run(
    db: &DeadlineDb,
    id: &str,
    occurrence_ms: i64,
    result: String,
) -> crate::application::services::ServiceResult<()> {
    let id = id.to_string();
    db.write(
        move |pool| async move { schedules::record_run(&pool, &id, occurrence_ms, &result).await },
    )
    .await
}

/// Looks for due events every `tick` and runs them; an occurrence missed by
/// more than the grace (the engine was down) is recorded as skipped. Its
/// reads and writes go through `db`, the shared loops' executor: reads end
/// at the deadline, writes are awaited to their outcome.
///
/// Each schedule's occurrence runs (or is recorded skipped) in a task of its
/// own, so a slow record or action delays only its own schedule, never
/// another pipeline's occurrence past its grace. A schedule with a task
/// still running is left alone: its rows are read only after its finished
/// tasks are collected, so a tick never sees an occurrence as due that a
/// finished task already recorded, and never starts one twice. At shutdown
/// running occurrences get [`SHUTDOWN_DRAIN`] to finish.
pub async fn run_scheduler(
    state: Arc<AppState>,
    db: DeadlineDb,
    tick: Duration,
    action_deadline: Duration,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let mut ticks = tokio::time::interval(tick);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut occurrences = tokio::task::JoinSet::new();
    let mut running = Running::new();
    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                return interrupt_at_shutdown(occurrences, running, &db).await;
            }
            _ = ticks.tick() => {}
        }
        while let Some(joined) = occurrences.try_join_next_with_id() {
            let task = match joined {
                Ok((task, ())) => task,
                Err(join_error) => {
                    error!(
                        schedule = running
                            .get(&join_error.id())
                            .map_or("", |(id, _)| id.as_str()),
                        event_class = "lifecycle",
                        event_type = "scheduler.occurrence_panicked",
                        "a scheduled occurrence panicked"
                    );
                    join_error.id()
                }
            };
            running.remove(&task);
        }
        let now = now_ms();
        let Ok(all) = db.read(|pool| schedules::list(pool, now)).await else {
            continue;
        };
        for schedule in all {
            if running.values().any(|(id, _)| *id == schedule.id) {
                continue;
            }
            let id = schedule.id.clone();
            let (task, occurrence) = match schedules::due(
                schedule.at_ms,
                schedule.repeat,
                schedule.enabled,
                schedule.last_run_ms,
                now,
            ) {
                Due::Nothing => continue,
                Due::Run(occurrence) => (
                    occurrences.spawn(run_occurrence(
                        state.clone(),
                        db.clone(),
                        schedule,
                        occurrence,
                        action_deadline,
                    )),
                    Some(occurrence),
                ),
                Due::Skip(occurrence) => (
                    occurrences.spawn(record_skip(db.clone(), schedule.id, occurrence)),
                    None,
                ),
            };
            running.insert(task.id(), (id, occurrence));
        }
    }
}

/// Gives the running occurrences [`SHUTDOWN_DRAIN`] to finish, then aborts
/// the rest and records each one that was running an action as
/// `interrupted: shutdown`; its time counts as run, so it is not started
/// again on the next start.
async fn interrupt_at_shutdown(
    mut occurrences: tokio::task::JoinSet<()>,
    mut running: Running,
    db: &DeadlineDb,
) {
    let _ = tokio::time::timeout(SHUTDOWN_DRAIN, async {
        while let Some(joined) = occurrences.join_next_with_id().await {
            let task = joined.map_or_else(|join_error| join_error.id(), |(task, ())| task);
            running.remove(&task);
        }
    })
    .await;
    occurrences.abort_all();
    for (id, occurrence) in running.into_values() {
        let Some(occurrence) = occurrence else {
            continue;
        };
        warn!(schedule = %id, "scheduled action interrupted by shutdown");
        if let Err(error) =
            record_run(db, &id, occurrence, "interrupted: shutdown".to_string()).await
        {
            warn!(schedule = %id, err = %error, "failed to record an interrupted schedule run");
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
