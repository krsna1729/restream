//! Mixed-runner reporting and log helper routines.

use super::*;
use chrono::{DateTime, Duration as ChronoDuration};

pub(crate) fn safe_artifact_stem(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

pub(crate) fn emit_mixed_result(
    env: &MixedEnv,
    cfg: &str,
    id: &str,
    status: &str,
    elapsed: Duration,
    extra: Option<Value>,
) -> Result<(), String> {
    let Some(path) = &env.assertion_log else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut object = serde_json::Map::new();
    object.insert("id".to_string(), json!(id));
    object.insert("suite".to_string(), json!("mixed"));
    object.insert("mode".to_string(), json!(cfg));
    object.insert("scenario".to_string(), json!(cfg));
    object.insert("status".to_string(), json!(status));
    object.insert("ms".to_string(), json!(elapsed.as_millis()));
    if let Some(Value::Object(extra)) = extra {
        object.extend(extra);
    }
    append_line(path, &format!("{}\n", Value::Object(object))).map_err(|e| e.to_string())
}

pub(crate) fn emit_mixed_timing(
    env: &MixedEnv,
    cfg: &str,
    stage: &str,
    status: &str,
    elapsed: Duration,
    extra: Option<Value>,
) -> Result<(), String> {
    let ended_at = Utc::now();
    let started_at = ended_at
        .checked_sub_signed(
            ChronoDuration::from_std(elapsed).unwrap_or_else(|_| ChronoDuration::zero()),
        )
        .unwrap_or(ended_at);
    emit_mixed_timing_window(
        env, cfg, stage, status, started_at, ended_at, elapsed, extra,
    )
}

pub(crate) fn emit_mixed_timing_window(
    env: &MixedEnv,
    cfg: &str,
    stage: &str,
    status: &str,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    elapsed: Duration,
    extra: Option<Value>,
) -> Result<(), String> {
    let object = mixed_timing_record(cfg, stage, status, started_at, ended_at, elapsed, extra);
    append_line(&env.timing_log, &format!("{object}\n")).map_err(|e| e.to_string())
}

pub(crate) fn log_mixed_ok(env: &MixedEnv, message: &str) -> Result<(), String> {
    append_line(&env.summary_log, &format!("ok: {message}\n"))
}

fn mixed_timing_record(
    cfg: &str,
    stage: &str,
    status: &str,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    elapsed: Duration,
    extra: Option<Value>,
) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("scenario".to_string(), json!(cfg));
    object.insert("stage".to_string(), json!(stage));
    object.insert("status".to_string(), json!(status));
    object.insert("ms".to_string(), json!(elapsed.as_millis()));
    object.insert("startedAt".to_string(), json!(started_at.to_rfc3339()));
    object.insert("endedAt".to_string(), json!(ended_at.to_rfc3339()));
    if let Some(Value::Object(extra)) = extra {
        object.extend(extra);
    }
    Value::Object(object)
}

pub(crate) fn effective_log_paths(path: &Path) -> Vec<PathBuf> {
    let Some(parent) = path.parent() else {
        return vec![path.to_path_buf()];
    };
    let logs_dir = parent.join("logs");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&logs_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("restream.log"))
        })
        .collect();
    entries.sort();
    if entries.is_empty() {
        vec![path.to_path_buf()]
    } else {
        entries
    }
}

pub(crate) fn count_log_matches(path: &Path, needle: &str) -> usize {
    effective_log_paths(path)
        .into_iter()
        .filter_map(|candidate| std::fs::read_to_string(candidate).ok())
        .map(|content| content.matches(needle).count())
        .sum()
}

pub(crate) fn file_tail_lines(path: &Path, lines: usize) -> Vec<String> {
    let Some(target) = effective_log_paths(path).into_iter().last() else {
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(target) else {
        return Vec::new();
    };
    let mut tail = content.lines().rev().take(lines).collect::<Vec<_>>();
    tail.reverse();
    tail.into_iter().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn mixed_timing_record_carries_explicit_window_and_extra_fields() {
        let started_at = Utc.with_ymd_and_hms(2026, 7, 6, 19, 22, 21).unwrap();
        let ended_at = Utc.with_ymd_and_hms(2026, 7, 6, 19, 24, 24).unwrap();
        let record = mixed_timing_record(
            "mixed.asset.file.h264.a2.bf0",
            "output.cell.rtmp.src.a0",
            "pass",
            started_at,
            ended_at,
            Duration::from_secs(123),
            Some(json!({
                "cellId": "rtmp.src.a0",
                "label": "rtmp.src.a0 out2",
            })),
        );

        assert_eq!(record["scenario"], "mixed.asset.file.h264.a2.bf0");
        assert_eq!(record["stage"], "output.cell.rtmp.src.a0");
        assert_eq!(record["status"], "pass");
        assert_eq!(record["ms"], 123000);
        assert_eq!(record["startedAt"], started_at.to_rfc3339());
        assert_eq!(record["endedAt"], ended_at.to_rfc3339());
        assert_eq!(record["cellId"], "rtmp.src.a0");
        assert_eq!(record["label"], "rtmp.src.a0 out2");
    }

    #[test]
    fn emit_mixed_result_keeps_canonical_status_when_extra_has_status_like_fields() {
        let temp = std::env::temp_dir().join(format!(
            "restream-mixed-reporting-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).expect("temp dir");
        let assertion_log = temp.join("assertions.jsonl");
        let env = MixedEnv {
            assertion_log: Some(assertion_log.clone()),
            ..MixedEnv::from_env_with_default_work_dir("mixed.test", temp.clone())
        };

        emit_mixed_result(
            &env,
            "mixed.test",
            "mixed.test.output.rtmp.src.a0.decode_scan",
            "pass",
            Duration::from_millis(42),
            Some(json!({
                "decodeExitStatus": 0,
                "matchedPattern": Value::Null,
            })),
        )
        .expect("assertion row");

        let line = std::fs::read_to_string(&assertion_log)
            .expect("assertion log")
            .lines()
            .next()
            .expect("one line")
            .to_string();
        let row: Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(row["status"], "pass");
        assert_eq!(row["decodeExitStatus"], 0);

        std::fs::remove_file(&assertion_log).ok();
        std::fs::remove_dir_all(&temp).ok();
    }
}
