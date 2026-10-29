use super::{
    AppResult, HarnessCatalog, ModeEntry, read_json_file, select_value, slice_axes,
    workflow_ref_from_name, workflow_steps, workflow_summary,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

impl HarnessCatalog {
    pub(crate) fn list_modes(&self) -> AppResult<Value> {
        let mut groups = Vec::new();

        for group in self.mode_groups()? {
            let kind = group
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let group_name = group
                .get("group")
                .and_then(Value::as_str)
                .unwrap_or("default");
            let mut entries = Vec::new();

            if let Some(modes) = group.get("modes").and_then(Value::as_object) {
                for (name, spec) in modes {
                    let mut entry = Map::new();
                    entry.insert("name".to_string(), json!(name));
                    entry.insert("kind".to_string(), json!(kind));
                    entry.insert("group".to_string(), json!(group_name));
                    if let Some(suite_ref) = spec.get("suiteRef") {
                        entry.insert("suiteRef".to_string(), suite_ref.clone());
                    }
                    if let Some(default) = spec.get("suiteDefault") {
                        entry.insert("suiteDefault".to_string(), default.clone());
                    }
                    entries.push(Value::Object(entry));
                }
            }

            entries.sort_by(|a, b| {
                a["name"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(b["name"].as_str().unwrap_or_default())
            });
            groups.push(json!({
                "group": group_name,
                "kind": kind,
                "modes": entries
            }));
        }

        let mut dynamic = self.dynamic_scenario_ids();
        dynamic.sort();
        groups.push(json!({
            "group": "dynamic-mixed-scenarios",
            "kind": "scenario",
            "modes": dynamic.into_iter().map(|name| json!({"name": name, "kind": "scenario"})).collect::<Vec<_>>()
        }));

        Ok(json!({
            "root": self.root,
            "groups": groups,
            "summary": self.summary()
        }))
    }

    pub(crate) fn resolve_mode(&self, mode: &str) -> AppResult<Value> {
        let mut index = self.mode_index()?;
        if let Some(entry) = index.remove(mode) {
            return self.resolve_static_mode(entry);
        }

        if let Some(dynamic) = self.resolve_dynamic_mode(mode)? {
            return Ok(dynamic);
        }

        Err(format!(
            "unknown mode {mode:?}; use list-modes to inspect canonical modes"
        ))
    }

    fn resolve_static_mode(&self, entry: ModeEntry) -> AppResult<Value> {
        match entry.kind.as_str() {
            "suite" => {
                let suite_ref = entry
                    .spec
                    .get("suiteRef")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("suite mode {} missing suiteRef", entry.name))?;
                let suite = self.value_ref(suite_ref)?;
                Ok(json!({
                    "name": entry.name,
                    "kind": "suite",
                    "group": entry.group,
                    "suiteRef": suite_ref,
                    "suite": suite,
                    "requires": entry.spec.get("requires").cloned().unwrap_or_else(|| json!({}))
                }))
            }
            "runner" => Ok(json!({
                "name": entry.name,
                "kind": "runner",
                "group": entry.group,
                "purpose": entry.spec.get("purpose").cloned().unwrap_or(Value::Null),
                "requires": entry.spec.get("requires").cloned().unwrap_or_else(|| json!({}))
            })),
            other => Err(format!("unsupported static mode kind {other}")),
        }
    }

    fn resolve_dynamic_mode(&self, mode: &str) -> AppResult<Option<Value>> {
        for spec in self
            .modes
            .get("dynamicModes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let prefix = spec
                .get("namePrefix")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !mode.starts_with(prefix) {
                continue;
            }
            let scenario_set_ref = spec
                .get("scenarioSet")
                .and_then(Value::as_str)
                .ok_or("dynamic mode spec missing scenarioSet".to_string())?;
            let field = spec
                .get("scenarioIdField")
                .and_then(Value::as_str)
                .unwrap_or("id");
            let workflow_field = spec
                .get("workflowField")
                .and_then(Value::as_str)
                .unwrap_or("workflow");
            let scenarios = self.value_ref(scenario_set_ref)?;
            let rows = scenarios.as_array().ok_or_else(|| {
                format!("dynamic scenario set {scenario_set_ref} is not an array")
            })?;

            for scenario in rows {
                if scenario.get(field).and_then(Value::as_str) == Some(mode) {
                    let workflow_name = scenario
                        .get(workflow_field)
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            format!("dynamic scenario {mode} missing {workflow_field}")
                        })?;
                    let workflow_ref = workflow_ref_from_name(workflow_name);
                    let workflow = self.read_workflow(&workflow_ref)?;
                    return Ok(Some(json!({
                        "name": mode,
                        "kind": "scenario",
                        "scenarioSet": scenario_set_ref,
                        "scenario": scenario,
                        "workflowRef": workflow_ref,
                        "workflow": workflow,
                        "requires": scenario.get("requires").cloned().unwrap_or_else(|| json!({}))
                    })));
                }
            }
        }

        Ok(None)
    }

    pub(crate) fn plan_mode(&self, mode: &str) -> AppResult<Value> {
        let resolved = self.resolve_mode(mode)?;
        let kind = resolved
            .get("kind")
            .and_then(Value::as_str)
            .ok_or("resolved mode missing kind".to_string())?;

        match kind {
            "runner" => Ok(json!({
                "planVersion": 1,
                "mode": mode,
                "kind": "runner",
                "purpose": resolved["purpose"].clone(),
                "steps": [
                    {
                        "op": "runner",
                        "runner": resolved["name"].clone(),
                        "argv": [resolved["name"].clone()]
                    }
                ]
            })),
            "scenario" => {
                let scenario = resolved
                    .get("scenario")
                    .ok_or("scenario resolution missing scenario".to_string())?;
                let workflow = resolved
                    .get("workflow")
                    .ok_or("scenario resolution missing workflow".to_string())?;
                Ok(self.plan_scenario(mode, scenario, workflow))
            }
            "suite" => {
                let suite = resolved
                    .get("suite")
                    .ok_or("suite resolution missing suite".to_string())?;
                self.plan_suite(mode, suite)
            }
            other => Err(format!("unsupported resolved kind {other}")),
        }
    }

    fn plan_scenario(&self, mode: &str, scenario: &Value, workflow: &Value) -> Value {
        let output_ref = scenario.get("outputs").cloned().unwrap_or(Value::Null);
        let checks_ref = scenario.get("checks").cloned().unwrap_or(Value::Null);
        let output_rows = self.resolve_inline_mixed_ref(&output_ref).ok();
        let checks = self.resolve_inline_mixed_ref(&checks_ref).ok();

        json!({
            "planVersion": 1,
            "mode": mode,
            "kind": "mixed-scenario",
            "scenarioId": scenario.get("id").cloned().unwrap_or_else(|| json!(mode)),
            "workflow": workflow_summary(workflow),
            "source": {
                "adapter": scenario.pointer("/source/adapter").cloned().unwrap_or(Value::Null),
                "ingest": scenario.pointer("/source/ingest").cloned().unwrap_or(Value::Null),
                "fixture": scenario.pointer("/source/fixture").cloned().unwrap_or(Value::Null),
                "trackSelection": scenario.pointer("/source/trackSelection").cloned().unwrap_or(Value::Null),
            },
            "expect": scenario.get("expect").cloned().unwrap_or(Value::Null),
            "outputPlan": scenario.get("outputPlan").cloned().unwrap_or(Value::Null),
            "outputs": {
                "reference": output_ref,
                "resolved": output_rows,
            },
            "checks": {
                "reference": checks_ref,
                "resolved": checks,
            },
            "steps": workflow_steps(workflow),
        })
    }

    fn plan_suite(&self, mode: &str, suite: &Value) -> AppResult<Value> {
        let suite_id = suite.get("id").and_then(Value::as_str).unwrap_or(mode);

        if suite.get("selection").is_some() && suite_id == "default" {
            let children = self.default_suite_children()?;
            return Ok(json!({
                "planVersion": 1,
                "mode": mode,
                "kind": "suite",
                "suiteId": suite_id,
                "selection": suite.get("selection").cloned().unwrap_or(Value::Null),
                "children": children,
            }));
        }

        if let Some(scenarios_from) = suite.pointer("/scenarios/from").and_then(Value::as_str) {
            let rows = self.value_ref(scenarios_from)?;
            let rows = rows.as_array().ok_or_else(|| {
                format!("suite {suite_id} scenarios.from did not resolve to array")
            })?;
            let mut scenarios = Vec::new();
            for row in rows {
                let id = row.get("id").and_then(Value::as_str).unwrap_or("<unknown>");
                let workflow_ref = workflow_ref_from_name(
                    row.get("workflow")
                        .and_then(Value::as_str)
                        .unwrap_or("mixed-scenario"),
                );
                let workflow = self.read_workflow(&workflow_ref)?;
                scenarios.push(json!({
                    "id": id,
                    "workflow": workflow_summary(&workflow),
                    "sourceAdapter": row.pointer("/source/adapter").cloned().unwrap_or(Value::Null),
                    "outputPlan": row.get("outputPlan").cloned().unwrap_or(Value::Null),
                    "artifact": row.get("artifact").cloned().unwrap_or(Value::Null),
                }));
            }
            return Ok(json!({
                "planVersion": 1,
                "mode": mode,
                "kind": "suite",
                "suiteId": suite_id,
                "runtimeProfile": suite.get("runtimeProfile").cloned().unwrap_or(Value::Null),
                "execution": suite.get("execution").cloned().unwrap_or(Value::Null),
                "scenarioCount": scenarios.len(),
                "scenarios": scenarios,
            }));
        }

        if let Some(coverage_ref) = suite.get("coverage").and_then(Value::as_str) {
            let rows = self.value_ref(coverage_ref)?;
            let rows = rows
                .as_array()
                .ok_or_else(|| format!("suite {suite_id} coverage did not resolve to array"))?;
            let workflow_ref = suite
                .get("workflow")
                .and_then(Value::as_str)
                .unwrap_or("workflows/mixed-slice.json");
            let workflow = self.read_workflow(workflow_ref)?;
            let slices: Vec<Value> = rows
                .iter()
                .map(|row| {
                    json!({
                        "id": row.get("id").cloned().unwrap_or(Value::Null),
                        "kind": row.get("kind").cloned().unwrap_or(Value::Null),
                        "sourceScenario": row.get("sourceScenario").cloned().unwrap_or(Value::Null),
                        "outputRows": row.get("outputRows").cloned().unwrap_or(Value::Null),
                        "outputFilter": row.get("outputFilter").cloned().unwrap_or(Value::Null),
                        "checkBundle": row.get("checkBundle").cloned().unwrap_or(Value::Null),
                        "resourceProfile": row.get("resourceProfile").cloned().unwrap_or(Value::Null),
                        "axes": slice_axes(row),
                    })
                })
                .collect();
            return Ok(json!({
                "planVersion": 1,
                "mode": mode,
                "kind": "suite",
                "suiteId": suite_id,
                "runtimeProfile": suite.get("runtimeProfile").cloned().unwrap_or(Value::Null),
                "workflow": workflow_summary(&workflow),
                "execution": suite.get("execution").cloned().unwrap_or(Value::Null),
                "coverageRef": coverage_ref,
                "sliceCount": slices.len(),
                "slices": slices,
                "steps": workflow_steps(&workflow),
            }));
        }

        if let Some(batches_ref) = suite.get("batches").and_then(Value::as_str) {
            let batches = self.value_ref(batches_ref)?;
            return Ok(json!({
                "planVersion": 1,
                "mode": mode,
                "kind": "suite",
                "suiteId": suite_id,
                "runtimeProfile": suite.get("runtimeProfile").cloned().unwrap_or(Value::Null),
                "batchesRef": batches_ref,
                "batches": batches,
                "execution": suite.get("execution").cloned().unwrap_or(Value::Null),
            }));
        }

        Ok(json!({
            "planVersion": 1,
            "mode": mode,
            "kind": "suite",
            "suiteId": suite_id,
            "suite": suite,
            "warning": "suite shape is valid JSON but has no recognized coverage/scenario/batch selector"
        }))
    }

    pub(crate) fn default_suite_children(&self) -> AppResult<Vec<Value>> {
        let mut children = Vec::new();
        for (_, entry) in self.mode_index()? {
            if entry.name == "suite" {
                continue;
            }
            if entry
                .spec
                .get("suiteDefault")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let mut child = json!({
                    "mode": entry.name,
                    "kind": entry.kind,
                    "group": entry.group,
                });
                if let Some(suite_ref) = entry.spec.get("suiteRef") {
                    child["suiteRef"] = suite_ref.clone();
                }
                children.push(child);
            }
        }
        children.sort_by(|a, b| {
            a["mode"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["mode"].as_str().unwrap_or_default())
        });
        Ok(children)
    }

    fn read_workflow(&self, workflow_ref: &str) -> AppResult<Value> {
        let path = if workflow_ref.ends_with(".json") {
            workflow_ref.to_string()
        } else {
            workflow_ref_from_name(workflow_ref)
        };
        read_json_file(&self.root.join(path))
    }

    pub(super) fn value_ref(&self, reference: &str) -> AppResult<Value> {
        let reference = reference.trim_start_matches('@');
        let (path, selector) = reference
            .split_once('#')
            .map(|(path, selector)| (path, Some(selector)))
            .unwrap_or((reference, None));
        let value = read_json_file(&self.root.join(path))?;
        match selector {
            None | Some("") => Ok(value),
            Some(selector) => select_value(&value, selector).cloned().ok_or_else(|| {
                format!("reference {reference:?} selector {selector:?} did not resolve")
            }),
        }
    }

    fn resolve_inline_mixed_ref(&self, value: &Value) -> AppResult<Value> {
        let Some(raw) = value.as_str() else {
            return Ok(value.clone());
        };
        let Some(path) = raw.strip_prefix('@') else {
            return Ok(value.clone());
        };

        if let Some(key) = path.strip_prefix("outputs.") {
            return self
                .mixed
                .pointer(&format!("/outputs/{key}"))
                .cloned()
                .ok_or_else(|| format!("missing mixed output set {key}"));
        }
        if let Some(key) = path.strip_prefix("checks.") {
            return self
                .mixed
                .pointer(&format!("/checks/{key}"))
                .cloned()
                .ok_or_else(|| format!("missing mixed check ref {key}"));
        }
        if path.contains('#') || path.ends_with(".json") {
            return self.value_ref(path);
        }

        Ok(value.clone())
    }

    pub(crate) fn mode_index(&self) -> AppResult<BTreeMap<String, ModeEntry>> {
        let mut out = BTreeMap::new();

        for group in self.mode_groups()? {
            let kind = group
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("runner")
                .to_string();
            let group_name = group
                .get("group")
                .and_then(Value::as_str)
                .unwrap_or("default")
                .to_string();

            if let Some(modes) = group.get("modes").and_then(Value::as_object) {
                for (name, spec) in modes {
                    let mut spec = spec.clone();
                    if let Value::Object(ref mut map) = spec {
                        map.insert("name".to_string(), json!(name));
                        map.insert("kind".to_string(), json!(kind));
                        map.insert("group".to_string(), json!(group_name));
                    }
                    out.insert(
                        name.clone(),
                        ModeEntry {
                            name: name.clone(),
                            kind: kind.clone(),
                            group: group_name.clone(),
                            spec,
                        },
                    );
                }
            }

            if let Some(names) = group.get("names").and_then(Value::as_array) {
                for item in names {
                    let (name, spec) = if let Some(name) = item.as_str() {
                        (name.to_string(), json!({}))
                    } else {
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .ok_or("mode group object missing name".to_string())?
                            .to_string();
                        (name, item.clone())
                    };
                    out.insert(
                        name.clone(),
                        ModeEntry {
                            name,
                            kind: kind.clone(),
                            group: group_name.clone(),
                            spec,
                        },
                    );
                }
            }
        }

        Ok(out)
    }
}
