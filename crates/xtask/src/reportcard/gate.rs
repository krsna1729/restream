//! The cyclomatic ratchet: no function above the limit except the recorded
//! offenders, none of those growing, and no entry outliving its offence.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use super::units::Unit;

/// Recorded offenders: unit key to the cyclomatic value it may not exceed.
pub(super) type Baseline = BTreeMap<String, u64>;

/// Why the gate, or a baseline update, fails.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Finding {
    /// Above the limit and not recorded: fix it, never record it.
    New {
        key: String,
        actual: u64,
        limit: u64,
    },
    /// A recorded offender grew past its recorded value.
    Grew {
        key: String,
        recorded: u64,
        actual: u64,
    },
    /// A recorded offender shrank but is still above the limit: the slack
    /// would let it grow back unnoticed.
    Loose {
        key: String,
        recorded: u64,
        actual: u64,
    },
    /// The recorded function is gone (`actual` is `None`) or at or below
    /// the limit, like an unfulfilled `#[expect]`.
    Stale {
        key: String,
        recorded: u64,
        actual: Option<u64>,
    },
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::New { key, actual, limit } => write!(
                f,
                "`{key}` cyclomatic {actual} exceeds {limit}; split it (new offenders are fixed, never baselined)"
            ),
            Self::Grew {
                key,
                recorded,
                actual,
            } => write!(
                f,
                "`{key}` cyclomatic grew from its baselined {recorded} to {actual}; reduce it"
            ),
            Self::Loose {
                key,
                recorded,
                actual,
            } => write!(
                f,
                "`{key}` cyclomatic fell from {recorded} to {actual}; lower its baseline with `cargo xtask reportcard --update-baseline`"
            ),
            Self::Stale {
                key,
                recorded,
                actual,
            } => {
                let now = actual.map_or_else(|| "is gone".to_string(), |n| format!("is now {n}"));
                write!(
                    f,
                    "stale baseline entry `{key}` (recorded {recorded}) {now}; remove it with `cargo xtask reportcard --update-baseline`"
                )
            }
        }
    }
}

/// Every unit above `limit`, keyed like the baseline.
pub(super) fn offenders(units: &[Unit], limit: u64) -> Baseline {
    units
        .iter()
        .filter(|unit| unit.cyclomatic > limit)
        .map(|unit| (unit.key.clone(), unit.cyclomatic))
        .collect()
}

/// The gate: each offender recorded at exactly its value, and each entry
/// still an offender.
pub(super) fn check(units: &[Unit], limit: u64, baseline: &Baseline) -> Vec<Finding> {
    let current: HashMap<&str, u64> = units
        .iter()
        .map(|unit| (unit.key.as_str(), unit.cyclomatic))
        .collect();
    let mut findings = refusals(&offenders(units, limit), limit, baseline);
    for (key, &recorded) in baseline {
        let actual = current.get(key.as_str()).copied();
        let key = key.clone();
        match actual {
            Some(actual) if actual > limit && actual < recorded => findings.push(Finding::Loose {
                key,
                recorded,
                actual,
            }),
            Some(actual) if actual > limit => {}
            _ => findings.push(Finding::Stale {
                key,
                recorded,
                actual,
            }),
        }
    }
    findings
}

/// The baseline rewritten to exactly the current offenders, or the
/// offenders it would have to add or raise. An update only removes and
/// lowers entries, so it cannot launder a regression.
pub(super) fn updated(
    units: &[Unit],
    limit: u64,
    baseline: &Baseline,
) -> Result<Baseline, Vec<Finding>> {
    let offenders = offenders(units, limit);
    let refused = refusals(&offenders, limit, baseline);
    if refused.is_empty() {
        Ok(offenders)
    } else {
        Err(refused)
    }
}

/// Offenders missing from the baseline or above their recorded value.
fn refusals(offenders: &Baseline, limit: u64, baseline: &Baseline) -> Vec<Finding> {
    offenders
        .iter()
        .filter_map(|(key, &actual)| match baseline.get(key) {
            None => Some(Finding::New {
                key: key.clone(),
                actual,
                limit,
            }),
            Some(&recorded) if actual > recorded => Some(Finding::Grew {
                key: key.clone(),
                recorded,
                actual,
            }),
            Some(_) => None,
        })
        .collect()
}
