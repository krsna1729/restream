#[path = "fault_recovery/boundary_target.rs"]
mod boundary_target;
#[path = "fault_recovery/destination_stall.rs"]
mod destination_stall;
#[path = "fault_recovery/egress.rs"]
mod egress;
#[path = "fault_recovery/resilience.rs"]
mod resilience;
#[path = "fault_recovery/rtmps_egress.rs"]
mod rtmps_egress;
#[path = "fault_recovery/sibling_sampler.rs"]
mod sibling_sampler;
#[path = "fault_recovery/sibling_wire.rs"]
mod sibling_wire;
#[path = "fault_recovery/siblings.rs"]
mod siblings;
#[path = "fault_recovery/srt_stall.rs"]
mod srt_stall;

pub(crate) use siblings::fault_sibling_isolation;

#[cfg(test)]
pub(crate) use egress::effective_fault_output_stall_siblings;
pub(crate) use egress::{fault_egress_retry, fault_output_stall};
pub(crate) use resilience::{fault_resilience, recovery};
pub(crate) use srt_stall::fault_srt_output_stall;

/// Writes the mode's `{mode}.json` artifact and fails the mode unless every
/// test passed.
fn finish_fault_mode(
    work_dir: &std::path::Path,
    mode: &str,
    passed: bool,
    result: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let result_path = work_dir.join(format!("{mode}.json"));
    std::fs::write(&result_path, serde_json::to_string_pretty(&result).unwrap())
        .map_err(|e| e.to_string())?;
    println!("artifact={}", result_path.display());
    if !passed {
        return Err(format!("{mode}: not all tests passed"));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::egress;
    #[test]
    fn retry_tuning_readback_rejects_a_missing_or_ineffective_override() {
        let path =
            std::env::temp_dir().join(format!("restream-retry-tuning-{}.log", std::process::id()));
        let expected = [(
            "RESTREAM_RECONCILE_INTERVAL_MS",
            "reconcilerIntervalMs",
            100,
        )];
        let cases = [
            ("no effective configuration", false),
            (r#"{"tuning":{"reconcilerIntervalMs":1000}}"#, false),
            (r#"{"tuning":{}}"#, false),
            (r#"{"tuning":{"reconcilerIntervalMs":100}}"#, true),
        ];
        let results: Vec<_> = cases
            .iter()
            .map(|(log, succeeds)| {
                std::fs::write(&path, log).unwrap();
                (egress::verify_effective_tuning(&path, &expected), *succeeds)
            })
            .collect();
        std::fs::remove_file(path).unwrap();
        for (result, succeeds) in results {
            assert_eq!(result.is_ok(), succeeds, "{result:?}");
        }
    }
}
