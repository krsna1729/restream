//! `rust-code-analysis-cli` JSON to per-function units with stable keys.
//!
//! A key is the file path, the enclosing `impl`/`trait` type names and
//! functions, the function name and an ordinal among earlier functions with
//! the same qualified path in that file: `src/a.rs::Engine::run#0`, a closure
//! in it `src/a.rs::Engine::run#0::<anonymous>#0`. Line numbers are left out
//! so an edit elsewhere in the file does not rename every function after it;
//! the analyzer names an `impl` after its self type only, so the two `fmt` of
//! `impl Display for X` and `impl Debug for X` are `X::fmt#0` and `X::fmt#1`.

use std::collections::HashMap;

use serde_json::Value;

use super::Summary;

/// One function or closure, with its own complexity (nested functions and
/// closures excluded; they are units of their own).
#[derive(Clone, Debug)]
pub(super) struct Unit {
    pub(super) key: String,
    pub(super) name: String,
    pub(super) path: String,
    pub(super) start_line: u64,
    pub(super) end_line: u64,
    pub(super) cyclomatic: u64,
    pub(super) cognitive: u64,
}

impl Unit {
    pub(super) fn is_closure(&self) -> bool {
        self.name == "<anonymous>"
    }
}

/// Adds one analyzer output file to `summary`. A function without
/// complexity metrics is an error, never a zero: the gate must not pass
/// what the analyzer failed to measure.
pub(super) fn add_file(value: &Value, summary: &mut Summary) -> Result<(), String> {
    let path = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or("analyzer JSON is missing a file name")?;
    collect(value, path, path, &mut HashMap::new(), &mut summary.units)
        .map_err(|key| format!("analyzer JSON is missing complexity metrics for {key}"))?;
    summary.files += 1;
    // SLOC is reported, never gated; a missing count renders as zero.
    summary.sloc += metric_value(value, "loc", "sloc").unwrap_or(0);
    Ok(())
}

/// Walks the child spaces of `space`, whose qualified path is `prefix`.
/// `ordinals` counts functions per qualified path across the whole file.
fn collect(
    space: &Value,
    path: &str,
    prefix: &str,
    ordinals: &mut HashMap<String, usize>,
    units: &mut Vec<Unit>,
) -> Result<(), String> {
    for child in children(space) {
        let name = child
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unnamed>");
        match child.get("kind").and_then(Value::as_str) {
            Some("function") => {
                let qualified = format!("{prefix}::{name}");
                let ordinal = ordinals.entry(qualified.clone()).or_default();
                let key = format!("{qualified}#{ordinal}");
                *ordinal += 1;
                let unit = Unit {
                    cyclomatic: own_metric(child, "cyclomatic").ok_or_else(|| key.clone())?,
                    cognitive: own_metric(child, "cognitive").ok_or_else(|| key.clone())?,
                    name: name.to_string(),
                    path: path.to_string(),
                    start_line: line(child, "start_line"),
                    end_line: line(child, "end_line"),
                    key,
                };
                let nested = unit.key.clone();
                units.push(unit);
                collect(child, path, &nested, ordinals, units)?;
            }
            Some("impl" | "trait") => {
                collect(child, path, &format!("{prefix}::{name}"), ordinals, units)?;
            }
            _ => collect(child, path, prefix, ordinals, units)?,
        }
    }
    Ok(())
}

fn children(space: &Value) -> impl Iterator<Item = &Value> {
    space
        .get("spaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn line(space: &Value, field: &str) -> u64 {
    space.get(field).and_then(Value::as_u64).unwrap_or_default()
}

/// An analyzer metric that is present, finite and non-negative; anything
/// else is `None`, so gated complexity never defaults to zero.
pub(super) fn metric_value(value: &Value, group: &str, metric: &str) -> Option<u64> {
    value
        .get("metrics")?
        .get(group)?
        .get(metric)?
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.round() as u64)
}

/// The space's own `metric`: its sum minus its child spaces' sums.
pub(super) fn own_metric(space: &Value, metric: &str) -> Option<u64> {
    let total = metric_value(space, metric, "sum")?;
    let nested = children(space)
        .map(|child| metric_value(child, metric, "sum"))
        .sum::<Option<u64>>()?;
    Some(total.saturating_sub(nested))
}
