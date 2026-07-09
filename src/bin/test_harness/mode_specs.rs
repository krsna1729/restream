//! Harness mode metadata and command lookup helpers.

use std::path::PathBuf;
use std::sync::OnceLock;

use serde_json::Value;

use crate::catalog::HarnessCatalog;
use crate::mixed_manifest::{MixedInputCase, mixed_input_case_for_command, mixed_input_cases};

/// Metadata describing how a harness mode participates in suite runs,
/// derived from the `test/harness/modes.json` catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessModeSpec {
    pub(crate) name: String,
    pub(crate) suite_default: bool,
    pub(crate) requires_port_namespace: bool,
    pub(crate) requires_bench_profile: bool,
}

pub(crate) fn harness_catalog_root() -> PathBuf {
    std::env::var_os("HARNESS_CATALOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test/harness"))
}

static BUILTIN_MODE_SPECS_FROM_CATALOG: OnceLock<Vec<HarnessModeSpec>> = OnceLock::new();

fn builtin_mode_specs() -> &'static [HarnessModeSpec] {
    BUILTIN_MODE_SPECS_FROM_CATALOG.get_or_init(|| {
        let catalog = HarnessCatalog::load(&harness_catalog_root())
            .expect("test/harness catalog should load");
        let index = catalog
            .mode_index()
            .expect("test/harness modes.json should index cleanly");
        index
            .into_values()
            .map(|entry| {
                let requires = entry.spec.get("requires").cloned().unwrap_or_default();
                HarnessModeSpec {
                    name: entry.name,
                    suite_default: entry
                        .spec
                        .get("suiteDefault")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    requires_port_namespace: requires
                        .get("portNamespace")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    requires_bench_profile: requires
                        .get("benchProfile")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                }
            })
            .collect()
    })
}

fn mixed_input_mode_spec(case: MixedInputCase) -> HarnessModeSpec {
    HarnessModeSpec {
        name: case.scenario_id().to_string(),
        suite_default: false,
        requires_port_namespace: true,
        // Mixed scenarios always emit timing/resource evidence and should run
        // under one harness-level profile policy rather than varying by cell.
        requires_bench_profile: true,
    }
}

pub(crate) fn mode_spec(name: &str) -> Option<HarnessModeSpec> {
    builtin_mode_specs()
        .iter()
        .find(|spec| spec.name == name)
        .cloned()
        .or_else(|| mixed_input_case_for_command(name).map(mixed_input_mode_spec))
}

pub(crate) fn all_mode_specs() -> Vec<HarnessModeSpec> {
    let mut specs = builtin_mode_specs().to_vec();
    specs.extend(
        mixed_input_cases()
            .iter()
            .copied()
            .map(mixed_input_mode_spec),
    );
    specs
}

pub(crate) fn suite_default_modes() -> Vec<String> {
    all_mode_specs()
        .into_iter()
        .filter(|spec| spec.suite_default)
        .map(|spec| spec.name)
        .collect()
}

pub(crate) fn supported_mode_names() -> Vec<String> {
    all_mode_specs().into_iter().map(|spec| spec.name).collect()
}

pub(crate) fn unknown_command_error(other: &str) -> String {
    let supported = supported_mode_names();
    format!("unknown command {other:?}; use {}", supported.join(", "))
}

pub(crate) fn command_requires_port_namespace(command: &str) -> bool {
    mode_spec(command)
        .map(|spec| spec.requires_port_namespace)
        .unwrap_or(false)
}

// Measurement-oriented modes are only meaningful when both binaries come from
// the lightweight bench profile, so we fail fast instead of recording skewed
// numbers from debug or release builds.
pub(crate) fn measurement_mode_requires_bench_profile(mode: &str) -> bool {
    mode_spec(mode)
        .map(|spec| spec.requires_bench_profile)
        .unwrap_or(false)
}

pub(crate) fn suite_modes_require_bench_profile(raw: &[String]) -> Result<bool, String> {
    let mut modes = suite_default_modes();
    let mut preflight_only = false;

    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--only-modes" => {
                i += 1;
                modes = raw
                    .get(i)
                    .ok_or("--only-modes requires a value")?
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "--run-id" | "--work-root" => {
                i += 1;
                raw.get(i)
                    .ok_or_else(|| format!("{} requires a value", raw[i - 1]))?;
            }
            "--no-netns" => {}
            "--continue-on-fail" => {}
            "--preflight-only" => preflight_only = true,
            other => return Err(format!("unknown suite option: {other}")),
        }
        i += 1;
    }

    if modes.is_empty() {
        return Err("--only-modes produced an empty mode list".to_string());
    }

    Ok(preflight_only
        || modes
            .iter()
            .any(|mode| measurement_mode_requires_bench_profile(mode)))
}
