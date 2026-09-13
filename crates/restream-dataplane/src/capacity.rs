//! Calibrated service-center capacity projection.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Workload {
    pub outputs: u32,
    pub media_bps: u64,
    pub media_pps: u64,
    pub rtmp_outputs: u32,
    pub rtmps_outputs: u32,
    pub srt_outputs: u32,
    pub loss_rate: f64,
    pub stage_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapacityRates {
    pub ingress_pps: f64,
    pub media_bps: f64,
    pub egress_pps: f64,
    pub nic_bps: f64,
    pub memory_bytes: f64,
    pub ffmpeg_stages: f64,
    pub disk_bps: f64,
    pub tx_bytes_per_output: f64,
    pub shards: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapacitySnapshot {
    pub ingress_pps: f64,
    pub media_bps: f64,
    pub egress_pps: f64,
    pub hottest_shard_util: f32,
    pub nic_util: f32,
    pub memory_util: f32,
    pub ffmpeg_util: f32,
    pub disk_util: f32,
    pub active_leaves: u32,
    pub unique_stages: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapacityModel {
    rates: CapacityRates,
}

impl CapacityModel {
    pub fn new(rates: CapacityRates) -> Result<Self, CapacityRates> {
        let valid = rates.shards != 0
            && [
                rates.ingress_pps,
                rates.media_bps,
                rates.egress_pps,
                rates.nic_bps,
                rates.memory_bytes,
                rates.ffmpeg_stages,
                rates.disk_bps,
                rates.tx_bytes_per_output,
            ]
            .into_iter()
            .all(|rate| rate.is_finite() && rate.is_sign_positive());
        valid.then_some(Self { rates }).ok_or(rates)
    }

    pub fn snapshot(&self, workload: Workload) -> CapacitySnapshot {
        let outputs = f64::from(workload.outputs);
        let loss_multiplier = 1.0 + workload.loss_rate.max(0.0);
        let egress_pps = workload.media_pps as f64 * outputs * loss_multiplier;
        let egress_bps = workload.media_bps as f64 * outputs * loss_multiplier;
        let shard_capacity = self.rates.egress_pps * f64::from(self.rates.shards);
        CapacitySnapshot {
            ingress_pps: workload.media_pps as f64,
            media_bps: workload.media_bps as f64,
            egress_pps,
            hottest_shard_util: (egress_pps / shard_capacity).min(f32::MAX as f64) as f32,
            nic_util: (egress_bps / self.rates.nic_bps).min(f32::MAX as f64) as f32,
            memory_util: (outputs * self.rates.tx_bytes_per_output / self.rates.memory_bytes)
                .min(f32::MAX as f64) as f32,
            ffmpeg_util: (f64::from(workload.stage_count) / self.rates.ffmpeg_stages)
                .min(f32::MAX as f64) as f32,
            disk_util: (workload.media_bps as f64 / self.rates.disk_bps).min(f32::MAX as f64)
                as f32,
            active_leaves: workload.outputs,
            unique_stages: workload.stage_count,
        }
    }

    pub fn hottest_utilization(&self, workload: Workload) -> f32 {
        let snapshot = self.snapshot(workload);
        let ingress_util = snapshot.ingress_pps / self.rates.ingress_pps;
        let media_util = snapshot.media_bps / self.rates.media_bps;
        snapshot
            .hottest_shard_util
            .max(snapshot.nic_util)
            .max(snapshot.memory_util)
            .max(snapshot.ffmpeg_util)
            .max(snapshot.disk_util)
            .max(ingress_util.min(f32::MAX as f64) as f32)
            .max(media_util.min(f32::MAX as f64) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> CapacityModel {
        CapacityModel::new(CapacityRates {
            ingress_pps: 10_000.0,
            media_bps: 1_000_000_000.0,
            egress_pps: 20_000.0,
            nic_bps: 10_000_000_000.0,
            memory_bytes: 64.0 * 1024.0 * 1024.0,
            ffmpeg_stages: 16.0,
            disk_bps: 1_000_000_000.0,
            tx_bytes_per_output: 64.0 * 1024.0,
            shards: 2,
        })
        .unwrap()
    }

    #[test]
    fn projection_identifies_the_hottest_service_center() {
        let snapshot = model().snapshot(Workload {
            outputs: 20,
            media_bps: 100_000_000,
            media_pps: 1_000,
            rtmp_outputs: 10,
            rtmps_outputs: 5,
            srt_outputs: 5,
            loss_rate: 0.1,
            stage_count: 4,
        });
        assert_eq!(snapshot.active_leaves, 20);
        assert_eq!(snapshot.unique_stages, 4);
        assert!(snapshot.hottest_shard_util > snapshot.nic_util);
    }

    #[test]
    fn projection_is_monotonic_in_outputs() {
        let base = Workload {
            outputs: 1,
            media_bps: 10_000,
            media_pps: 10,
            rtmp_outputs: 1,
            rtmps_outputs: 0,
            srt_outputs: 0,
            loss_rate: 0.0,
            stage_count: 0,
        };
        let mut larger = base;
        larger.outputs = 10;
        assert!(model().hottest_utilization(larger) > model().hottest_utilization(base));
    }

    #[test]
    fn zero_outputs_have_no_egress_load() {
        let workload = Workload {
            outputs: 0,
            media_bps: 10_000,
            media_pps: 10,
            rtmp_outputs: 0,
            rtmps_outputs: 0,
            srt_outputs: 0,
            loss_rate: 0.0,
            stage_count: 0,
        };
        let snapshot = model().snapshot(workload);
        assert_eq!(snapshot.egress_pps, 0.0);
        assert_eq!(snapshot.active_leaves, 0);
    }

    #[test]
    fn hottest_utilization_includes_ingress_and_media_centers() {
        let workload = Workload {
            outputs: 0,
            media_bps: 1_000_000_000,
            media_pps: 10_000,
            rtmp_outputs: 0,
            rtmps_outputs: 0,
            srt_outputs: 0,
            loss_rate: 0.0,
            stage_count: 0,
        };
        assert_eq!(model().hottest_utilization(workload), 1.0);
    }

    #[test]
    fn invalid_rates_are_rejected() {
        assert!(
            CapacityModel::new(CapacityRates {
                shards: 0,
                ..CapacityRates {
                    ingress_pps: 1.0,
                    media_bps: 1.0,
                    egress_pps: 1.0,
                    nic_bps: 1.0,
                    memory_bytes: 1.0,
                    ffmpeg_stages: 1.0,
                    disk_bps: 1.0,
                    tx_bytes_per_output: 1.0,
                    shards: 1,
                }
            })
            .is_err()
        );
        assert!(
            CapacityModel::new(CapacityRates {
                ingress_pps: f64::INFINITY,
                media_bps: 1.0,
                egress_pps: 1.0,
                nic_bps: 1.0,
                memory_bytes: 1.0,
                ffmpeg_stages: 1.0,
                disk_bps: 1.0,
                tx_bytes_per_output: 1.0,
                shards: 1,
            })
            .is_err()
        );
    }
}
