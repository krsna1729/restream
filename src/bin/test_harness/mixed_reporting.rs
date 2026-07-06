//! Mixed-runner reporting and log helper routines.

use super::*;

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
    let mut object = serde_json::Map::new();
    object.insert("scenario".to_string(), json!(cfg));
    object.insert("stage".to_string(), json!(stage));
    object.insert("status".to_string(), json!(status));
    object.insert("ms".to_string(), json!(elapsed.as_millis()));
    if let Some(Value::Object(extra)) = extra {
        object.extend(extra);
    }
    append_line(&env.timing_log, &format!("{}\n", Value::Object(object))).map_err(|e| e.to_string())
}

pub(crate) fn log_mixed_ok(env: &MixedEnv, message: &str) -> Result<(), String> {
    append_line(&env.summary_log, &format!("ok: {message}\n"))
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
