use super::{AppResult, HarnessCatalog, ensure_file, read_json_file, row_id};
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};
use std::fs;

impl HarnessCatalog {
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
                if kind == "suite" {
                    for (name, spec) in modes {
                        let suite_ref = spec
                            .get("suiteRef")
                            .and_then(Value::as_str)
                            .ok_or_else(|| format!("suite mode {name} missing suiteRef"))?;
                        let suite = self.value_ref(suite_ref).map_err(|error| {
                            format!("suite mode {name} has invalid suiteRef {suite_ref}: {error}")
                        })?;
                        if !suite.is_object() {
                            return Err(format!(
                                "suite mode {name} suiteRef {suite_ref} must resolve to an object"
                            ));
                        }
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
}
