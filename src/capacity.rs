//! Measured service-center capacity and lightweight Flow Doctor primitives.
//!
//! Connection count is deliberately absent from the admission calculation.
//! The model consumes observed work (bytes/packets, stages, and fanout) and
//! reports the hottest calibrated service center. It starts observe-only.

use serde::Serialize;

use crate::media::engine::MediaEngine;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workload {
    pub outputs: u32,
    pub media_bps: u64,
    pub media_pps: u64,
    pub rtmp_outputs: u32,
    pub rtmps_outputs: u32,
    pub srt_outputs: u32,
    pub loss_rate: f64,
    pub stage_count: u32,
    pub shard_count: u32,
}

impl Default for Workload {
    fn default() -> Self {
        Self {
            outputs: 0,
            media_bps: 0,
            media_pps: 0,
            rtmp_outputs: 0,
            rtmps_outputs: 0,
            srt_outputs: 0,
            loss_rate: 0.0,
            stage_count: 0,
            shard_count: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityLimits {
    pub ingress_pps: f64,
    pub egress_pps: f64,
    pub nic_bps: f64,
    pub memory_bytes: f64,
    pub ffmpeg_stages: f64,
    pub disk_bps: f64,
}

impl CapacityLimits {
    /// Conservative starting calibration. Deployments can replace these
    /// values through `AppConfig::from_env` after measuring their host.
    pub fn from_parallelism(parallelism: u32) -> Self {
        let cores = f64::from(parallelism.max(1));
        Self {
            ingress_pps: cores * 100_000.0,
            egress_pps: cores * 250_000.0,
            nic_bps: 10_000_000_000.0,
            memory_bytes: 1_000_000_000.0,
            ffmpeg_stages: cores,
            disk_bps: 500_000_000.0,
        }
    }
}

impl Default for CapacityLimits {
    fn default() -> Self {
        let parallelism = std::thread::available_parallelism()
            .map(|value| value.get() as u32)
            .unwrap_or(1);
        Self::from_parallelism(parallelism)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ServiceCenter {
    Ingress,
    Egress,
    Cpu,
    Nic,
    Memory,
    Ffmpeg,
    Disk,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
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
    pub hottest_center: ServiceCenter,
    pub projected_utilization: f32,
    pub flow: FlowDiagnosis,
    pub observe_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CapacityModel {
    pub limits: CapacityLimits,
    pub safe_utilization: f64,
}

impl Default for CapacityModel {
    fn default() -> Self {
        Self {
            limits: CapacityLimits::default(),
            safe_utilization: 0.8,
        }
    }
}

impl CapacityModel {
    pub fn project(&self, workload: Workload) -> CapacitySnapshot {
        let ingress = ratio(workload.media_pps as f64, self.limits.ingress_pps);
        let egress_pps = (workload.media_pps as f64)
            * f64::from(workload.outputs)
            * (1.0 + workload.loss_rate.max(0.0));
        let shard_count = f64::from(workload.shard_count.max(1));
        let hottest_shard = ratio(egress_pps / shard_count, self.limits.egress_pps);
        let egress = ratio(egress_pps, self.limits.egress_pps * shard_count);
        let nic_bps = (workload.media_bps as f64)
            * f64::from(workload.outputs)
            * (1.0 + workload.loss_rate.max(0.0));
        let nic = ratio(nic_bps, self.limits.nic_bps);
        let memory = ratio(
            f64::from(workload.outputs) * 262_144.0,
            self.limits.memory_bytes,
        );
        let ffmpeg = ratio(f64::from(workload.stage_count), self.limits.ffmpeg_stages);
        let disk = 0.0;
        let cpu = ratio(
            egress_pps + workload.media_pps as f64,
            self.limits.egress_pps * shard_count,
        );
        let candidates = [
            (ServiceCenter::Ingress, ingress),
            (ServiceCenter::Egress, egress.max(cpu)),
            (ServiceCenter::Nic, nic),
            (ServiceCenter::Memory, memory),
            (ServiceCenter::Ffmpeg, ffmpeg),
            (ServiceCenter::Disk, disk),
        ];
        let (hottest_center, projected_utilization) = candidates
            .into_iter()
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .unwrap_or((ServiceCenter::Ingress, 0.0));
        let flow = diagnose(FlowSample {
            center: hottest_center,
            arrival_rate: projected_utilization,
            service_rate: 1.0,
            queue: 0.0,
            backlog_slope: 0.0,
            delay_ms: 0.0,
            deadline_slack_ms: f64::INFINITY,
            errors: 0,
            amplification: 1.0,
        });
        CapacitySnapshot {
            ingress_pps: workload.media_pps as f64,
            media_bps: workload.media_bps as f64,
            egress_pps,
            hottest_shard_util: hottest_shard as f32,
            nic_util: nic as f32,
            memory_util: memory as f32,
            ffmpeg_util: ffmpeg as f32,
            disk_util: disk as f32,
            active_leaves: workload.outputs,
            unique_stages: workload.stage_count,
            hottest_center,
            projected_utilization: projected_utilization as f32,
            flow,
            observe_only: true,
        }
    }

    pub fn safe(&self, snapshot: CapacitySnapshot) -> bool {
        f64::from(snapshot.projected_utilization) < self.safe_utilization
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSample {
    pub center: ServiceCenter,
    pub arrival_rate: f64,
    pub service_rate: f64,
    pub queue: f64,
    pub backlog_slope: f64,
    pub delay_ms: f64,
    pub deadline_slack_ms: f64,
    pub errors: u64,
    pub amplification: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FlowStatus {
    Healthy,
    Saturated,
    Overloaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDiagnosis {
    pub center: ServiceCenter,
    pub utilization: f64,
    pub queue: f64,
    pub backlog_slope: f64,
    pub deadline_slack_ms: f64,
    pub delay_ms: f64,
    pub errors: u64,
    pub amplification: f64,
    pub status: FlowStatus,
}

pub fn diagnose(sample: FlowSample) -> FlowDiagnosis {
    let utilization = ratio(sample.arrival_rate, sample.service_rate);
    let status = if utilization >= 1.0 || sample.backlog_slope > 0.0 {
        FlowStatus::Overloaded
    } else if utilization >= 0.8 || sample.deadline_slack_ms < 0.0 {
        FlowStatus::Saturated
    } else {
        FlowStatus::Healthy
    };
    FlowDiagnosis {
        center: sample.center,
        utilization,
        queue: sample.queue,
        backlog_slope: sample.backlog_slope,
        deadline_slack_ms: sample.deadline_slack_ms,
        delay_ms: sample.delay_ms,
        errors: sample.errors,
        amplification: sample.amplification,
        status,
    }
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator > 0.0 {
        (numerator / denominator).max(0.0)
    } else if numerator > 0.0 {
        f64::INFINITY
    } else {
        0.0
    }
}

impl MediaEngine {
    /// Observe the current runtime topology and counters without making an
    /// admission decision. The snapshot is intentionally a control-plane
    /// operation; no native shard hot path reads these locks.
    pub async fn capacity_snapshot(&self) -> CapacitySnapshot {
        let ingests = self.ingests.active.read().await;
        let mut media_bps = 0_u64;
        let mut media_pps = 0_u64;
        for ingest in ingests.values() {
            if let Some(rate) = MediaEngine::sample_ingest_bitrate_kbps(ingest) {
                media_bps = media_bps.saturating_add((rate * 1_000.0).max(0.0) as u64);
            }
            media_pps = media_pps.saturating_add(ingest.metrics.snapshot().packets_per_sec as u64);
        }
        drop(ingests);

        let egresses = self.egresses.active.read().await;
        let mut workload = Workload {
            outputs: egresses.len().min(u32::MAX as usize) as u32,
            media_bps,
            media_pps,
            ..Workload::default()
        };
        for egress in egresses.values() {
            match egress.protocol.as_str() {
                "rtmp" => workload.rtmp_outputs = workload.rtmp_outputs.saturating_add(1),
                "rtmps" => workload.rtmps_outputs = workload.rtmps_outputs.saturating_add(1),
                "srt" => workload.srt_outputs = workload.srt_outputs.saturating_add(1),
                _ => {}
            }
        }
        drop(egresses);

        workload.stage_count = self
            .stages
            .runtimes
            .read()
            .await
            .len()
            .min(u32::MAX as usize) as u32;
        workload.shard_count = self.config.egress_fabric.shards.max(1);
        let mut snapshot = CapacityModel {
            limits: self.config.capacity_limits,
            safe_utilization: 0.8,
        }
        .project(workload);
        if let Some((queue, capacity, progress_age_ms, stalled)) = self
            .egress_fabric_shard_statuses(std::time::Duration::from_secs(5))
            .await
            .iter()
            .map(|status| {
                (
                    f64::from(status.command_depth),
                    f64::from(status.command_capacity.max(1)),
                    status.progress_age_ms.unwrap_or_default() as f64,
                    !matches!(
                        status.state,
                        crate::media::egress::shard::EgressShardHealth::Healthy
                    ),
                )
            })
            .max_by(|left, right| left.0.total_cmp(&right.0))
        {
            let flow = diagnose(FlowSample {
                center: ServiceCenter::Egress,
                arrival_rate: queue,
                service_rate: capacity,
                queue,
                backlog_slope: if stalled { 1.0 } else { 0.0 },
                delay_ms: progress_age_ms,
                deadline_slack_ms: 1_000.0 - progress_age_ms,
                errors: u64::from(stalled),
                amplification: 1.0,
            });
            snapshot.projected_utilization =
                snapshot.projected_utilization.max(flow.utilization as f32);
            snapshot.flow = flow;
        }
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn fanout_drives_egress_and_nic_work_not_connection_limit() {
        let snapshot = CapacityModel {
            limits: CapacityLimits::from_parallelism(4),
            safe_utilization: 0.8,
        }
        .project(Workload {
            outputs: 100,
            media_bps: 10_000_000,
            media_pps: 100,
            shard_count: 4,
            ..Workload::default()
        });
        assert_eq!(snapshot.active_leaves, 100);
        assert_eq!(snapshot.egress_pps, 10_000.0);
        assert!(snapshot.nic_util > 0.0);
    }

    #[test]
    fn flow_doctor_marks_growing_queue_overloaded() {
        let diagnosis = diagnose(FlowSample {
            center: ServiceCenter::Egress,
            arrival_rate: 90.0,
            service_rate: 100.0,
            queue: 10.0,
            backlog_slope: 1.0,
            delay_ms: 20.0,
            deadline_slack_ms: 50.0,
            errors: 0,
            amplification: 1.0,
        });
        assert_eq!(diagnosis.status, FlowStatus::Overloaded);
    }

    #[test]
    fn zero_work_is_healthy() {
        let diagnosis = diagnose(FlowSample {
            center: ServiceCenter::Ingress,
            arrival_rate: 0.0,
            service_rate: 1.0,
            queue: 0.0,
            backlog_slope: 0.0,
            delay_ms: 0.0,
            deadline_slack_ms: 1.0,
            errors: 0,
            amplification: 1.0,
        });
        assert_eq!(diagnosis.status, FlowStatus::Healthy);
    }

    proptest! {
        #[test]
        fn projection_preserves_nonnegative_work(
            outputs in 0u32..10_000,
            media_bps in 0u64..1_000_000_000,
            media_pps in 0u64..1_000_000,
            stages in 0u32..128,
        ) {
            let snapshot = CapacityModel::default().project(Workload {
                outputs,
                media_bps,
                media_pps,
                stage_count: stages,
                ..Workload::default()
            });
            prop_assert!(snapshot.ingress_pps >= 0.0);
            prop_assert!(snapshot.media_bps >= 0.0);
            prop_assert!(snapshot.egress_pps >= 0.0);
            prop_assert!(snapshot.active_leaves == outputs);
            prop_assert!(snapshot.unique_stages == stages);
        }
    }
}
