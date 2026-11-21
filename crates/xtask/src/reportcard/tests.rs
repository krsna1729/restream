use serde_json::json;

use super::Summary;
use super::gate::{self, Baseline, Finding};
use super::units::{self, Unit};

const LIMIT: u64 = 20;

fn unit(key: &str, cyclomatic: u64) -> Unit {
    Unit {
        key: key.to_string(),
        name: key.to_string(),
        path: "src/a.rs".to_string(),
        start_line: 1,
        end_line: 2,
        cyclomatic,
        cognitive: 0,
    }
}

fn baseline(entries: &[(&str, u64)]) -> Baseline {
    entries
        .iter()
        .map(|&(key, value)| (key.to_string(), value))
        .collect()
}

#[test]
fn at_limit_and_recorded_offenders_pass() {
    let units = [unit("at", LIMIT), unit("old", 30)];
    assert_eq!(gate::check(&units, LIMIT, &baseline(&[("old", 30)])), []);
}

#[test]
fn an_unrecorded_offender_fails() {
    let units = [unit("new", LIMIT + 1)];
    assert_eq!(
        gate::check(&units, LIMIT, &Baseline::new()),
        [Finding::New {
            key: "new".into(),
            actual: LIMIT + 1,
            limit: LIMIT
        }]
    );
}

#[test]
fn a_recorded_offender_that_grew_fails() {
    let units = [unit("old", 31)];
    assert_eq!(
        gate::check(&units, LIMIT, &baseline(&[("old", 30)])),
        [Finding::Grew {
            key: "old".into(),
            recorded: 30,
            actual: 31
        }]
    );
}

#[test]
fn a_recorded_offender_that_shrank_must_lower_its_entry() {
    let units = [unit("old", 25)];
    assert_eq!(
        gate::check(&units, LIMIT, &baseline(&[("old", 30)])),
        [Finding::Loose {
            key: "old".into(),
            recorded: 30,
            actual: 25
        }]
    );
}

#[test]
fn entries_for_gone_or_fixed_functions_are_stale() {
    let units = [unit("fixed", LIMIT)];
    let recorded = baseline(&[("fixed", 30), ("gone", 30)]);
    assert_eq!(
        gate::check(&units, LIMIT, &recorded),
        [
            Finding::Stale {
                key: "fixed".into(),
                recorded: 30,
                actual: Some(LIMIT)
            },
            Finding::Stale {
                key: "gone".into(),
                recorded: 30,
                actual: None
            },
        ]
    );
}

#[test]
fn update_removes_and_lowers_entries() {
    let units = [unit("fixed", 3), unit("shrank", 25), unit("same", 40)];
    let recorded = baseline(&[("fixed", 30), ("gone", 30), ("shrank", 30), ("same", 40)]);
    let updated = gate::updated(&units, LIMIT, &recorded).expect("only removals and lowerings");
    assert_eq!(updated, baseline(&[("same", 40), ("shrank", 25)]));
    assert_eq!(gate::check(&units, LIMIT, &updated), []);
}

#[test]
fn update_refuses_to_add_or_raise_entries() {
    let units = [unit("new", 21), unit("grew", 31), unit("fixed", 1)];
    let recorded = baseline(&[("grew", 30), ("fixed", 30)]);
    assert_eq!(
        gate::updated(&units, LIMIT, &recorded),
        Err(vec![
            Finding::Grew {
                key: "grew".into(),
                recorded: 30,
                actual: 31
            },
            Finding::New {
                key: "new".into(),
                actual: 21,
                limit: LIMIT
            },
        ])
    );
}

fn function(name: &str, cyclomatic: f64, spaces: serde_json::Value) -> serde_json::Value {
    json!({
        "kind": "function",
        "name": name,
        "metrics": {"cyclomatic": {"sum": cyclomatic}, "cognitive": {"sum": 0.0}},
        "spaces": spaces,
    })
}

#[test]
fn keys_qualify_by_impl_and_parent_and_count_duplicates_without_lines() {
    let closure = function("<anonymous>", 2.0, json!([]));
    let file = json!({
        "name": "src/a.rs",
        "kind": "unit",
        "spaces": [
            {"kind": "impl", "name": "X", "spaces": [function("fmt", 1.0, json!([]))]},
            {"kind": "impl", "name": "X", "spaces": [function("fmt", 5.0, json!([closure]))]},
            function("fmt", 1.0, json!([])),
        ],
    });
    let mut summary = Summary::default();
    units::add_file(&file, &mut summary).expect("measured file");
    let keys: Vec<(&str, u64)> = summary
        .units
        .iter()
        .map(|unit| (unit.key.as_str(), unit.cyclomatic))
        .collect();
    assert_eq!(
        keys,
        [
            ("src/a.rs::X::fmt#0", 1),
            ("src/a.rs::X::fmt#1", 3),
            ("src/a.rs::X::fmt#1::<anonymous>#0", 2),
            ("src/a.rs::fmt#0", 1),
        ]
    );
}

#[test]
fn a_function_without_complexity_is_an_error_not_zero() {
    let file = json!({
        "name": "src/a.rs",
        "spaces": [{"kind": "function", "name": "unmeasured", "metrics": {}}],
    });
    let error = units::add_file(&file, &mut Summary::default()).unwrap_err();
    assert!(error.contains("src/a.rs::unmeasured#0"), "{error}");
}
