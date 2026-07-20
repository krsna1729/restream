//! Manifest catalog loader for the `test/harness` DSL: parses, validates,
//! resolves, and plans the JSON mode/suite/scenario/workflow manifests.
//!
//! Shared by the runtime runner and its `test_harness catalog ...` read-only
//! inspection commands. The runtime also reuses the mode index for dispatch.

#[path = "catalog/helpers.rs"]
mod helpers;
#[path = "catalog/resolution.rs"]
mod resolution;
#[path = "catalog/validation.rs"]
mod validation;

pub(crate) use helpers::{
    ensure_file, read_json_file, required_str, row_id, select_value, slice_axes,
    workflow_ref_from_name, workflow_steps, workflow_summary,
};
use serde_json::{Value, json};
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_select_dot_paths() {
        let value = json!({
            "a": {"b": [10, 20]},
            "suites": {"mixed.signal": {"id": "mixed.signal"}}
        });
        assert_eq!(select_value(&value, "a.b.1").unwrap(), &json!(20));
        assert_eq!(
            select_value(&value, "suites.mixed.signal").unwrap(),
            &json!({"id": "mixed.signal"})
        );
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

    #[test]
    fn checked_in_catalog_self_check_resolves_every_suite_ref() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test/harness");
        HarnessCatalog::load(&root)
            .expect("load checked-in harness catalog")
            .self_check()
            .expect("all mode suiteRef values should resolve");
    }
}
