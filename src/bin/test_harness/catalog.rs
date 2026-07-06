//! Manifest catalog loader for the `test/harness` DSL: parses, validates,
//! resolves, and plans the JSON mode/suite/scenario/workflow manifests.
//!
//! Shared via `#[path]` inclusion by both `test_harness_dsl.rs` (the
//! standalone planner binary) and `test_harness.rs` (the runtime runner,
//! which reuses the mode index for its dispatch table).

use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) type AppResult<T> = Result<T, String>;

#[derive(Debug, Clone)]
pub(crate) struct HarnessCatalog {
    pub(crate) root: PathBuf,
    catalog: Value,
    modes: Value,
    suites: Value,
    mixed: Value,
    checks: Value,
    bundles: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct ModeEntry {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) group: String,
    pub(crate) spec: Value,
}

impl HarnessCatalog {
    pub(crate) fn load(root: &Path) -> AppResult<Self> {
        let root = root.to_path_buf();
        let catalog = read_json_file(&root.join("catalog.json"))?;
        let modes_path = required_str(&catalog, &["modes"])?;
        let modes = read_json_file(&root.join(modes_path))?;

        let suites_index = catalog
            .pointer("/suites/index")
            .and_then(Value::as_str)
            .ok_or("catalog.json missing suites.index".to_string())?;
        let suites = read_json_file(&root.join(suites_index))?;

        let mixed_path = catalog
            .pointer("/scenarios/mixed")
            .and_then(Value::as_str)
            .ok_or("catalog.json missing scenarios.mixed".to_string())?;
        let mixed = read_json_file(&root.join(mixed_path))?;

        let checks_path = catalog
            .pointer("/checks/registry")
            .and_then(Value::as_str)
            .ok_or("catalog.json missing checks.registry".to_string())?;
        let checks = read_json_file(&root.join(checks_path))?;

        let bundles_path = catalog
            .pointer("/checks/bundles")
            .and_then(Value::as_str)
            .ok_or("catalog.json missing checks.bundles".to_string())?;
        let bundles = read_json_file(&root.join(bundles_path))?;

        Ok(Self {
            root,
            catalog,
            modes,
            suites,
            mixed,
            checks,
            bundles,
        })
    }

    pub(crate) fn summary(&self) -> Value {
        let mode_count = self.mode_index().map(|m| m.len()).unwrap_or_default();
        let dynamic_count = self.dynamic_scenario_ids().len();
        json!({
            "modes": mode_count,
            "dynamicMixedScenarios": dynamic_count,
            "mixedInputs": self.mixed["inputs"].as_array().map(Vec::len).unwrap_or(0),
            "coverageSliceFamilies": self.mixed["coverageSlices"].as_object().map(|m| m.len()).unwrap_or(0),
            "suites": self.suites["suites"].as_object().map(|m| m.len()).unwrap_or(0),
            "checks": self.checks["checks"].as_object().map(|m| m.len()).unwrap_or(0),
            "checkBundles": self.bundles["bundles"].as_object().map(|m| m.len()).unwrap_or(0),
        })
    }

    pub(crate) fn self_check(&self) -> AppResult<()> {
        self.parse_all_json()?;
        self.check_catalog_refs()?;
        self.check_modes()?;
        self.check_suites()?;
        self.check_mixed_inputs()?;
        self.check_output_plans()?;
        self.check_check_bundles()?;
        self.check_coverage_slices()?;
        self.check_dynamic_modes()?;
        Ok(())
    }

    fn parse_all_json(&self) -> AppResult<()> {
        let mut stack = vec![self.root.clone()];
        while let Some(path) = stack.pop() {
            for entry in fs::read_dir(&path)
                .map_err(|e| format!("failed to read directory {}: {e}", path.display()))?
            {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "json") {
                    let _: Value = read_json_file(&path)?;
                }
            }
        }
        Ok(())
    }

    fn check_catalog_refs(&self) -> AppResult<()> {
        for path in [
            self.catalog.pointer("/modes"),
            self.catalog.pointer("/runtime/profiles"),
            self.catalog.pointer("/runtime/ports"),
            self.catalog.pointer("/runtime/protocols"),
            self.catalog.pointer("/runtime/services"),
            self.catalog.pointer("/checks/registry"),
            self.catalog.pointer("/checks/ffmpegPatterns"),
            self.catalog.pointer("/checks/bundles"),
            self.catalog.pointer("/suites/index"),
            self.catalog.pointer("/scenarios/mixed"),
            self.catalog.pointer("/scenarios/fault"),
        ]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        {
            ensure_file(&self.root.join(path), &format!("catalog reference {path}"))?;
        }

        if let Some(workflows) = self.catalog.get("workflows").and_then(Value::as_object) {
            for (name, path) in workflows {
                let path = path
                    .as_str()
                    .ok_or_else(|| format!("catalog workflows.{name} must be a string"))?;
                ensure_file(&self.root.join(path), &format!("workflow {name}"))?;
            }
        }

        Ok(())
    }

    fn check_modes(&self) -> AppResult<()> {
        let allowed_kinds = ["suite", "runner", "scenario"];
        let mut seen = HashSet::new();

        for group in self.mode_groups()? {
            let kind = group
                .get("kind")
                .and_then(Value::as_str)
                .ok_or("mode group missing kind".to_string())?;
            if kind == "alias" {
                return Err("alias groups are not allowed in agent command surface".to_string());
            }
            if !allowed_kinds.contains(&kind) {
                return Err(format!("unsupported mode group kind {kind:?}"));
            }
            if let Some(group_name) = group.get("group").and_then(Value::as_str)
                && group_name.contains("alias")
            {
                return Err(format!(
                    "alias-like mode group {group_name:?} is not allowed"
                ));
            }
            if let Some(modes) = group.get("modes").and_then(Value::as_object) {
                for name in modes.keys() {
                    if !seen.insert(name.clone()) {
                        return Err(format!("duplicate mode {name}"));
                    }
                }
            }
            if let Some(names) = group.get("names").and_then(Value::as_array) {
                for item in names {
                    let name = item
                        .as_str()
                        .or_else(|| item.get("name").and_then(Value::as_str))
                        .ok_or(
                            "mode group names must be strings or objects with name".to_string(),
                        )?;
                    if !seen.insert(name.to_string()) {
                        return Err(format!("duplicate mode {name}"));
                    }
                }
            }
        }

        Ok(())
    }

    fn check_suites(&self) -> AppResult<()> {
        let suites = self.suites_obj()?;
        for (key, suite) in suites {
            let id = suite
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("suite {key} missing id"))?;
            if id != key {
                return Err(format!("suite key {key} does not match id {id}"));
            }

            if let Some(workflow) = suite.get("workflow").and_then(Value::as_str) {
                ensure_file(&self.root.join(workflow), &format!("suite {key} workflow"))?;
            }
            if let Some(coverage) = suite.get("coverage").and_then(Value::as_str) {
                self.value_ref(coverage)
                    .map_err(|e| format!("suite {key} coverage {coverage}: {e}"))?;
            }
            if let Some(selector) = suite.get("selector").and_then(Value::as_str) {
                self.value_ref(selector)
                    .map_err(|e| format!("suite {key} selector {selector}: {e}"))?;
            }
        }
        Ok(())
    }

    fn check_mixed_inputs(&self) -> AppResult<()> {
        let inputs = self.mixed_inputs()?;
        let output_plans = self
            .mixed
            .get("outputPlans")
            .and_then(Value::as_object)
            .ok_or("mixed.json missing outputPlans object".to_string())?;

        let mut seen = HashSet::new();
        for row in inputs {
            let id = row_id(row, "mixed input")?;
            if !seen.insert(id.to_string()) {
                return Err(format!("duplicate mixed input {id}"));
            }
            if row.get("workflow").and_then(Value::as_str) != Some("mixed-scenario") {
                return Err(format!("{id} must use workflow=mixed-scenario"));
            }
            let adapter = row
                .pointer("/source/adapter")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{id} missing source.adapter"))?;
            if !matches!(adapter, "file" | "rtmpPublisher" | "srtPublisher") {
                return Err(format!("{id} has invalid source.adapter={adapter}"));
            }
            let plan = row
                .get("outputPlan")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{id} missing outputPlan"))?;
            if !output_plans.contains_key(plan) {
                return Err(format!("{id} references missing outputPlan {plan}"));
            }
        }
        Ok(())
    }

    fn check_output_plans(&self) -> AppResult<()> {
        let output_plans = self
            .mixed
            .get("outputPlans")
            .and_then(Value::as_object)
            .ok_or("mixed.json outputPlans must be an object".to_string())?;
        let checks = self.check_registry()?;

        for (plan_name, plan) in output_plans {
            if let Some(phases) = plan.get("phases").and_then(Value::as_array) {
                for phase in phases {
                    for check in phase
                        .get("checks")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let check = check.as_str().ok_or_else(|| {
                            format!("outputPlan {plan_name} has non-string check")
                        })?;
                        if !checks.contains_key(check) {
                            return Err(format!(
                                "outputPlan {plan_name} phase references missing check {check}"
                            ));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn check_check_bundles(&self) -> AppResult<()> {
        let checks = self.check_registry()?;
        let bundles = self.bundle_registry()?;

        for (bundle_name, bundle) in bundles {
            let row_checks = bundle
                .get("checks")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("bundle {bundle_name} missing checks array"))?;
            for check in row_checks {
                let name = check
                    .as_str()
                    .ok_or_else(|| format!("bundle {bundle_name} contains non-string check"))?;
                if !checks.contains_key(name) && !bundles.contains_key(name) {
                    return Err(format!(
                        "bundle {bundle_name} references missing check or bundle {name}"
                    ));
                }
            }
        }

        Ok(())
    }

    fn check_coverage_slices(&self) -> AppResult<()> {
        let input_ids: HashSet<String> = self
            .mixed_inputs()?
            .iter()
            .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        let checks = self.check_registry()?;
        let bundles = self.bundle_registry()?;
        let families = self
            .mixed
            .get("coverageSlices")
            .and_then(Value::as_object)
            .ok_or("mixed.json missing coverageSlices object".to_string())?;

        for (family, rows) in families {
            let rows = rows
                .as_array()
                .ok_or_else(|| format!("coverageSlices.{family} must be an array"))?;
            for row in rows {
                let id = row_id(row, &format!("coverageSlices.{family}"))?;
                if let Some(source) = row.get("sourceScenario").and_then(Value::as_str)
                    && !input_ids.contains(source)
                {
                    return Err(format!("{id} references missing sourceScenario {source}"));
                }
                if let Some(bundle) = row.get("checkBundle").and_then(Value::as_str)
                    && !bundles.contains_key(bundle)
                {
                    return Err(format!("{id} references missing checkBundle {bundle}"));
                }
                for check in row
                    .get("checks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let name = check
                        .as_str()
                        .ok_or_else(|| format!("{id} contains non-string check"))?;
                    if !checks.contains_key(name) {
                        return Err(format!("{id} references missing check {name}"));
                    }
                }
            }
        }

        Ok(())
    }

    fn check_dynamic_modes(&self) -> AppResult<()> {
        let static_modes: BTreeSet<String> = self.mode_index()?.keys().cloned().collect();
        let mut seen = HashSet::new();

        for id in self.dynamic_scenario_ids() {
            if static_modes.contains(&id) {
                return Err(format!(
                    "dynamic scenario id {id} collides with static mode"
                ));
            }
            if !seen.insert(id.clone()) {
                return Err(format!("duplicate dynamic scenario id {id}"));
            }
        }

        Ok(())
    }

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

    fn value_ref(&self, reference: &str) -> AppResult<Value> {
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

    fn mode_groups(&self) -> AppResult<&Vec<Value>> {
        self.modes
            .get("modeGroups")
            .and_then(Value::as_array)
            .ok_or("modes.json missing modeGroups array".to_string())
    }

    fn suites_obj(&self) -> AppResult<&Map<String, Value>> {
        self.suites
            .get("suites")
            .and_then(Value::as_object)
            .ok_or("suites.json missing suites object".to_string())
    }

    fn mixed_inputs(&self) -> AppResult<&Vec<Value>> {
        self.mixed
            .get("inputs")
            .and_then(Value::as_array)
            .ok_or("mixed.json missing inputs array".to_string())
    }

    fn check_registry(&self) -> AppResult<&Map<String, Value>> {
        self.checks
            .get("checks")
            .and_then(Value::as_object)
            .ok_or("checks.json missing checks object".to_string())
    }

    fn bundle_registry(&self) -> AppResult<&Map<String, Value>> {
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

    let mut current = value;
    for part in selector.split('.').filter(|part| !part.is_empty()) {
        if let Ok(index) = part.parse::<usize>() {
            current = current.as_array()?.get(index)?;
        } else {
            current = current.as_object()?.get(part)?;
        }
    }
    Some(current)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_select_dot_paths() {
        let value = json!({"a": {"b": [10, 20]}});
        assert_eq!(select_value(&value, "a.b.1").unwrap(), &json!(20));
    }

    #[test]
    fn workflow_ref_accepts_short_ids() {
        assert_eq!(
            workflow_ref_from_name("mixed-scenario"),
            "workflows/mixed-scenario.json"
        );
        assert_eq!(
            workflow_ref_from_name("workflows/mixed-slice.json"),
            "workflows/mixed-slice.json"
        );
    }
}
