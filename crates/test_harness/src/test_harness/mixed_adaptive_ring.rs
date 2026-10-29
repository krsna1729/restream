use serde_json::{Value, json};

/// Parsed source-ring telemetry used by the mixed adaptive-ring readiness gate.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MixedAdaptiveRingSnapshot {
    pub(crate) capacity: u64,
    pub(crate) depth_secs: f64,
    pub(crate) overflows: u64,
    pub(crate) resized: bool,
    pub(crate) adequate: bool,
    pub(crate) passed: bool,
}

impl MixedAdaptiveRingSnapshot {
    pub(crate) fn to_json(self) -> Value {
        json!({
            "ringCapacity": self.capacity,
            "bufferDepthSecs": self.depth_secs,
            "ringResized": self.resized,
            "adequate": self.adequate,
            "overflows": self.overflows,
        })
    }
}

pub(crate) fn mixed_adaptive_ring_snapshot(telemetry: &Value) -> MixedAdaptiveRingSnapshot {
    let capacity = telemetry["sourceRing"]["capacity"].as_u64().unwrap_or(0);
    let depth_secs = telemetry["sourceRing"]["bufferDepthSecs"]
        .as_f64()
        .unwrap_or(0.0);
    let overflows = telemetry["sourceRing"]["readers"]
        .as_array()
        .map(|readers| {
            readers
                .iter()
                .map(|reader| reader["overflowCount"].as_u64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0);
    // 2 audio tracks x 50 pkt/s + video is roughly 130 pkt/s, so 780 slots is
    // enough for the minimum 5-second depth. A capacity above 1024 additionally
    // proves the adaptive resize path fired.
    let resized = capacity > 1024;
    let adequate = depth_secs >= 5.0 || capacity >= 780;
    let passed = adequate && overflows == 0;
    MixedAdaptiveRingSnapshot {
        capacity,
        depth_secs,
        overflows,
        resized,
        adequate,
        passed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_adaptive_ring_snapshot_accepts_capacity_or_depth_without_overflow() {
        let resized = mixed_adaptive_ring_snapshot(&json!({
            "sourceRing": {
                "capacity": 2048,
                "bufferDepthSecs": 0.5,
                "readers": [{"overflowCount": 0}]
            }
        }));
        assert!(resized.resized);
        assert!(resized.adequate);
        assert!(resized.passed);

        let deep_enough = mixed_adaptive_ring_snapshot(&json!({
            "sourceRing": {
                "capacity": 512,
                "bufferDepthSecs": 5.1,
                "readers": [{"overflowCount": 0}]
            }
        }));
        assert!(!deep_enough.resized);
        assert!(deep_enough.adequate);
        assert!(deep_enough.passed);

        let overflowed = mixed_adaptive_ring_snapshot(&json!({
            "sourceRing": {
                "capacity": 2048,
                "bufferDepthSecs": 5.1,
                "readers": [{"overflowCount": 1}]
            }
        }));
        assert!(overflowed.adequate);
        assert!(!overflowed.passed);
    }
}
