use super::{AppResult, HarnessCatalog};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::Path;

impl HarnessCatalog {
    pub(super) fn mode_groups(&self) -> AppResult<&Vec<Value>> {
        self.modes
            .get("modeGroups")
            .and_then(Value::as_array)
            .ok_or("modes.json missing modeGroups array".to_string())
    }

    pub(super) fn suites_obj(&self) -> AppResult<&Map<String, Value>> {
        self.suites
            .get("suites")
            .and_then(Value::as_object)
            .ok_or("suites.json missing suites object".to_string())
    }

    pub(super) fn mixed_inputs(&self) -> AppResult<&Vec<Value>> {
        self.mixed
            .get("inputs")
            .and_then(Value::as_array)
            .ok_or("mixed.json missing inputs array".to_string())
    }

    pub(super) fn check_registry(&self) -> AppResult<&Map<String, Value>> {
        self.checks
            .get("checks")
            .and_then(Value::as_object)
            .ok_or("checks.json missing checks object".to_string())
    }

    pub(super) fn bundle_registry(&self) -> AppResult<&Map<String, Value>> {
        self.bundles
            .get("bundles")
            .and_then(Value::as_object)
            .ok_or("bundles.json missing bundles object".to_string())
    }

    pub(crate) fn dynamic_scenario_ids(&self) -> Vec<String> {
        self.mixed
            .get("inputs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_string))
            .collect()
    }
}

pub(crate) fn workflow_summary(workflow: &Value) -> Value {
    json!({
        "id": workflow.get("id").cloned().unwrap_or(Value::Null),
        "runtimeProfile": workflow.get("runtimeProfile").cloned().unwrap_or(Value::Null),
        "stack": workflow.get("stack").cloned().unwrap_or(Value::Null),
        "stepCount": workflow.get("steps").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
    })
}

pub(crate) fn workflow_steps(workflow: &Value) -> Vec<Value> {
    workflow
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|step| {
            let mut out = Map::new();
            out.insert(
                "op".to_string(),
                step.get("op").cloned().unwrap_or(Value::Null),
            );
            if let Some(as_name) = step.get("as") {
                out.insert("as".to_string(), as_name.clone());
            }
            if let Some(when) = step.get("when") {
                out.insert("when".to_string(), when.clone());
            }
            if let Some(when_selected) = step.get("whenSelected") {
                out.insert("whenSelected".to_string(), when_selected.clone());
            }
            Value::Object(out)
        })
        .collect()
}

pub(crate) fn slice_axes(row: &Value) -> Value {
    let mut out = Map::new();
    for key in ["axes", "bitrateAxis", "branchAxis", "cryptoAxis"] {
        if let Some(value) = row.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(out)
}

pub(crate) fn workflow_ref_from_name(workflow_name: &str) -> String {
    if workflow_name.ends_with(".json") || workflow_name.starts_with("workflows/") {
        workflow_name.to_string()
    } else {
        format!("workflows/{workflow_name}.json")
    }
}

pub(crate) fn select_value<'a>(value: &'a Value, selector: &str) -> Option<&'a Value> {
    if selector.starts_with('/') {
        return value.pointer(selector);
    }

    fn select_parts<'a>(current: &'a Value, parts: &[&str]) -> Option<&'a Value> {
        if parts.is_empty() {
            return Some(current);
        }

        if let Some(array) = current.as_array() {
            let index = parts[0].parse::<usize>().ok()?;
            return select_parts(array.get(index)?, &parts[1..]);
        }

        let object = current.as_object()?;
        // Prefer the longest matching object key. This keeps ordinary dot
        // selectors working while allowing catalog keys such as
        // `mixed.signal` to be addressed by `suites.mixed.signal`.
        for end in (1..=parts.len()).rev() {
            let key = parts[..end].join(".");
            if let Some(next) = object.get(&key)
                && let Some(selected) = select_parts(next, &parts[end..])
            {
                return Some(selected);
            }
        }
        None
    }

    let parts: Vec<&str> = selector
        .split('.')
        .filter(|part| !part.is_empty())
        .collect();
    select_parts(value, &parts)
}

pub(crate) fn required_str<'a>(value: &'a Value, path: &[&str]) -> AppResult<&'a str> {
    let mut current = value;
    for key in path {
        current = current
            .get(*key)
            .ok_or_else(|| format!("missing field {}", path.join(".")))?;
    }
    current
        .as_str()
        .ok_or_else(|| format!("field {} must be a string", path.join(".")))
}

pub(crate) fn row_id<'a>(row: &'a Value, label: &str) -> AppResult<&'a str> {
    row.get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{label} row missing id"))
}

pub(crate) fn read_json_file(path: &Path) -> AppResult<Value> {
    let body =
        fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_json::from_str(&body).map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

pub(crate) fn ensure_file(path: &Path, label: &str) -> AppResult<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("{label} does not exist: {}", path.display()))
    }
}
