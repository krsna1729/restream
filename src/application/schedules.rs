//! Scheduled events: start or stop a pipeline's destinations or its file
//! source at a set time, once or every day or week. Times are UTC
//! milliseconds; a daily event repeats every 24 h of UTC, so it keeps its
//! UTC time across a daylight-saving change.
//!
//! The rules of when an event runs are pure ([`due`]); persistence is
//! `db::*` (no port), as for outputs; the scheduler task runs the action.
//!
//! An occurrence runs if the scheduler sees it within [`RUN_GRACE_MS`] of its
//! time. One missed by more (the engine was down) is recorded as skipped and
//! never fired late: a "go live at 9:00" must not go live at 11:40.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::application::services::error::{ServiceError, ServiceResult};

/// How late an occurrence may still run.
pub const RUN_GRACE_MS: i64 = 60_000;
const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScheduleAction {
    /// Want the listed destinations (all, when none listed) running.
    StartOutputs,
    StopOutputs,
    /// Play the pipeline's configured file or playlist.
    StartFile,
    StopFile,
}

impl ScheduleAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StartOutputs => "startOutputs",
            Self::StopOutputs => "stopOutputs",
            Self::StartFile => "startFile",
            Self::StopFile => "stopFile",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "startOutputs" => Some(Self::StartOutputs),
            "stopOutputs" => Some(Self::StopOutputs),
            "startFile" => Some(Self::StartFile),
            "stopFile" => Some(Self::StopFile),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Repeat {
    Once,
    Daily,
    Weekly,
}

impl Repeat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "once" => Some(Self::Once),
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            _ => None,
        }
    }

    fn period_ms(self) -> Option<i64> {
        match self {
            Self::Once => None,
            Self::Daily => Some(DAY_MS),
            Self::Weekly => Some(7 * DAY_MS),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: String,
    pub pipeline_id: String,
    pub name: String,
    pub action: ScheduleAction,
    /// For output actions: the destinations; empty means all of them.
    pub output_ids: Vec<String>,
    /// The first occurrence.
    pub at_ms: i64,
    pub repeat: Repeat,
    pub enabled: bool,
    /// The occurrence last run or skipped.
    pub last_run_ms: Option<i64>,
    pub last_result: Option<String>,
    pub created_at_ms: i64,
    /// Derived: the next occurrence after now, if any.
    pub next_at_ms: Option<i64>,
}

/// The latest occurrence at or before `now_ms`, if one has come.
pub fn latest_occurrence(at_ms: i64, repeat: Repeat, now_ms: i64) -> Option<i64> {
    if now_ms < at_ms {
        return None;
    }
    match repeat.period_ms() {
        None => Some(at_ms),
        Some(period) => Some(at_ms + (now_ms - at_ms) / period * period),
    }
}

/// The first occurrence strictly after `now_ms`.
pub fn next_occurrence(at_ms: i64, repeat: Repeat, now_ms: i64) -> Option<i64> {
    if now_ms < at_ms {
        return Some(at_ms);
    }
    repeat
        .period_ms()
        .map(|period| at_ms + ((now_ms - at_ms) / period + 1) * period)
}

/// What the scheduler does with one schedule now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    Nothing,
    /// Run the occurrence at this time.
    Run(i64),
    /// This occurrence passed while nothing could run it: record it, do not
    /// fire it.
    Skip(i64),
}

pub fn due(
    at_ms: i64,
    repeat: Repeat,
    enabled: bool,
    last_run_ms: Option<i64>,
    now_ms: i64,
) -> Due {
    if !enabled {
        return Due::Nothing;
    }
    let Some(occurrence) = latest_occurrence(at_ms, repeat, now_ms) else {
        return Due::Nothing;
    };
    if last_run_ms.is_some_and(|last| last >= occurrence) {
        return Due::Nothing;
    }
    if now_ms - occurrence <= RUN_GRACE_MS {
        Due::Run(occurrence)
    } else {
        Due::Skip(occurrence)
    }
}

/// A schedule as created or edited.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleDraft {
    pub pipeline_id: String,
    pub name: String,
    pub action: ScheduleAction,
    #[serde(default)]
    pub output_ids: Vec<String>,
    pub at_ms: i64,
    pub repeat: Repeat,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

fn check(draft: &ScheduleDraft) -> ServiceResult<()> {
    if draft.name.trim().is_empty() || draft.name.len() > 120 {
        return Err(ServiceError::conflict(
            "A schedule needs a name of 1-120 characters.",
        ));
    }
    if draft.output_ids.len() > 1000 {
        return Err(ServiceError::conflict(
            "A schedule names at most 1000 destinations.",
        ));
    }
    if matches!(
        draft.action,
        ScheduleAction::StartFile | ScheduleAction::StopFile
    ) && !draft.output_ids.is_empty()
    {
        return Err(ServiceError::conflict(
            "A file action names no destinations.",
        ));
    }
    Ok(())
}

fn from_record(record: crate::db::schedule_repo::ScheduleRecord, now_ms: i64) -> Option<Schedule> {
    let action = ScheduleAction::parse(&record.action)?;
    let repeat = Repeat::parse(&record.repeat)?;
    Some(Schedule {
        next_at_ms: if record.enabled {
            next_occurrence(record.at_ms, repeat, now_ms)
        } else {
            None
        },
        id: record.id,
        pipeline_id: record.pipeline_id,
        name: record.name,
        action,
        output_ids: serde_json::from_str(&record.output_ids).unwrap_or_default(),
        at_ms: record.at_ms,
        repeat,
        enabled: record.enabled,
        last_run_ms: record.last_run_ms,
        last_result: record.last_result,
        created_at_ms: record.created_at_ms,
    })
}

fn internal(context: &'static str) -> impl Fn(sqlx::Error) -> ServiceError {
    move |err| ServiceError::internal(format!("{context}: {err}"))
}

pub async fn list(pool: &SqlitePool, now_ms: i64) -> ServiceResult<Vec<Schedule>> {
    Ok(crate::db::schedule_repo::list(pool)
        .await
        .map_err(internal("list schedules"))?
        .into_iter()
        .filter_map(|record| from_record(record, now_ms))
        .collect())
}

pub async fn get(pool: &SqlitePool, id: &str, now_ms: i64) -> ServiceResult<Schedule> {
    crate::db::schedule_repo::get(pool, id)
        .await
        .map_err(internal("get schedule"))?
        .and_then(|record| from_record(record, now_ms))
        .ok_or_else(|| ServiceError::not_found(format!("no schedule {id}")))
}

fn record_of(
    id: &str,
    draft: &ScheduleDraft,
    created_at_ms: i64,
) -> crate::db::schedule_repo::ScheduleRecord {
    crate::db::schedule_repo::ScheduleRecord {
        id: id.to_string(),
        pipeline_id: draft.pipeline_id.clone(),
        name: draft.name.trim().to_string(),
        action: draft.action.as_str().to_string(),
        output_ids: serde_json::to_string(&draft.output_ids).unwrap_or_else(|_| "[]".into()),
        at_ms: draft.at_ms,
        repeat: draft.repeat.as_str().to_string(),
        enabled: draft.enabled,
        last_run_ms: None,
        last_result: None,
        created_at_ms,
    }
}

/// Adds a schedule. Its occurrences before now are not run: the first to
/// run is the next one.
pub async fn create(
    pool: &SqlitePool,
    id: &str,
    draft: &ScheduleDraft,
    now_ms: i64,
) -> ServiceResult<Schedule> {
    check(draft)?;
    let mut record = record_of(id, draft, now_ms);
    record.last_run_ms = latest_occurrence(draft.at_ms, draft.repeat, now_ms);
    crate::db::schedule_repo::save(pool, &record)
        .await
        .map_err(internal("save schedule"))?;
    get(pool, id, now_ms).await
}

/// Replaces a schedule's settings. As on create, occurrences already past
/// are not run.
pub async fn update(
    pool: &SqlitePool,
    id: &str,
    draft: &ScheduleDraft,
    now_ms: i64,
) -> ServiceResult<Schedule> {
    check(draft)?;
    let current = get(pool, id, now_ms).await?;
    let mut record = record_of(id, draft, current.created_at_ms);
    record.last_run_ms =
        latest_occurrence(draft.at_ms, draft.repeat, now_ms).max(current.last_run_ms);
    record.last_result = current.last_result;
    crate::db::schedule_repo::save(pool, &record)
        .await
        .map_err(internal("save schedule"))?;
    get(pool, id, now_ms).await
}

pub async fn delete(pool: &SqlitePool, id: &str) -> ServiceResult<()> {
    if !crate::db::schedule_repo::delete(pool, id)
        .await
        .map_err(internal("delete schedule"))?
    {
        return Err(ServiceError::not_found(format!("no schedule {id}")));
    }
    Ok(())
}

/// Records that an occurrence ran (or was skipped) and how.
pub async fn record_run(
    pool: &SqlitePool,
    id: &str,
    occurrence_ms: i64,
    result: &str,
) -> ServiceResult<()> {
    crate::db::schedule_repo::record_run(pool, id, occurrence_ms, result)
        .await
        .map_err(internal("record schedule run"))
}

#[cfg(test)]
#[path = "schedules_tests.rs"]
mod tests;
