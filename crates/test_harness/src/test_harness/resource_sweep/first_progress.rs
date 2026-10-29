//! Per-output first-progress latency for the resource sweep: from the
//! output-start request to the first positive `bytesOut`, polled at 100 ms.
//! Written next to the sweep artifacts so a run's startup distribution (p50 /
//! p95 / max) is comparable across commits without scraping logs.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{ApiOutputStatus, RampApi};

const POLL: Duration = Duration::from_millis(100);

fn percentile(sorted: &[u64], pct: usize) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let index = ((sorted.len() * pct).div_ceil(100)).clamp(1, sorted.len()) - 1;
    Some(sorted[index])
}

/// Poll until every started output has positive `bytesOut` (or `timeout`),
/// then write `path`. Never fails the run: the ordinary progress gate that
/// follows enforces liveness; this only measures.
pub(super) async fn record_first_progress(
    api: &RampApi,
    pipeline_id: &str,
    starts: &[(String, Instant)],
    timeout: Duration,
    path: &Path,
) {
    let deadline = Instant::now() + timeout;
    let mut first: Vec<Option<u64>> = vec![None; starts.len()];
    loop {
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            for (index, (output_id, started)) in starts.iter().enumerate() {
                if first[index].is_some() {
                    continue;
                }
                let entry = &health["pipelines"][pipeline_id]["outputs"][output_id.as_str()];
                if let Ok(status) = ApiOutputStatus::from_value(output_id, entry)
                    && status.has_progress()
                {
                    first[index] = Some(started.elapsed().as_millis() as u64);
                }
            }
        }
        if first.iter().all(Option::is_some) || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    let mut done: Vec<u64> = first.iter().flatten().copied().collect();
    done.sort_unstable();
    let report: Value = json!({
        "outputs": starts.len(),
        "withProgress": done.len(),
        "firstProgressMs": {
            "min": done.first(),
            "p50": percentile(&done, 50),
            "p95": percentile(&done, 95),
            "max": done.last(),
        },
        "perOutputMs": first,
    });
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(&report).unwrap_or_default(),
    );
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn percentiles_use_nearest_rank() {
        let values: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&values, 50), Some(50));
        assert_eq!(percentile(&values, 95), Some(95));
        assert_eq!(percentile(&values, 100), Some(100));
        assert_eq!(percentile(&[7], 95), Some(7));
        assert_eq!(percentile(&[], 50), None);
    }
}
