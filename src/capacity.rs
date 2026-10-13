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
        Self::from_parallelism(HostCapacity::get().cpus as u32)
    }
}

/// The host's CPUs, read once at boot (`effective_cpu_count`: affinity mask
/// and root cgroup `cpu.max`), and the default size of every resident
/// execution class derived from them. Every sizing law takes its CPU count
/// from here, so no two laws see different hosts and none re-reads `/proc` or
/// `/sys` after boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostCapacity {
    pub cpus: usize,
}

impl HostCapacity {
    pub fn get() -> Self {
        static HOST: std::sync::OnceLock<HostCapacity> = std::sync::OnceLock::new();
        *HOST.get_or_init(|| Self {
            cpus: crate::system_sampling::effective_cpu_count(),
        })
    }

    /// CONTROL (Tokio) workers: all CPUs up to 2, else `ceil(C/3)` in `[2, 8]`.
    pub fn tokio_workers(self) -> usize {
        let cpus = self.cpus.max(1);
        if cpus <= 2 {
            cpus
        } else {
            cpus.div_ceil(3).clamp(2, 8)
        }
    }

    /// MEDIA (`restream-media`) workers: `clamp(C, 1, 4)`.
    pub fn media_workers(self) -> usize {
        self.cpus.clamp(1, 4)
    }

    /// Egress fabric shard threads scale with host cores instead of a flat
    /// constant. A live legacy-vs-fabric `perf` comparison at a combined
    /// 1,140 RTMP + 60 SRT workload (the egress migration record (git
    /// history) Phase 5) measured the previous flat default of 4 costing
    /// fabric ~12% more avg CPU and ~23% more peak CPU than legacy on a
    /// 6-CPU host; raising the shard count toward the host's own core count
    /// closed the avg-CPU gap entirely (fabric ~2% *below* legacy) and shrank
    /// the peak gap to ~3.5%, across three repeated live captures at each of
    /// shards=2, 4, and 6 — while preserving fabric's ~12-15% lower RSS
    /// throughout. This is the ceiling of one shard pool: `clamp(C, 2, 8)`.
    pub fn egress_shards_per_pool(self) -> u32 {
        self.cpus.clamp(2, 8) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
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

/// One projection of the observed workload onto the calibrated service
/// centers: the `*Util` fields are ratios against `CapacityLimits` priors,
/// not measurements. `flow` is measured from the egress shards when any run.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
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
    /// `flow` comes from running egress shards; otherwise it is the
    /// projection's hottest center, priced from the priors.
    pub flow_measured: bool,
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
            flow_measured: false,
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
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum FlowStatus {
    Healthy,
    Saturated,
    Overloaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct FlowDiagnosis {
    pub center: ServiceCenter,
    pub utilization: f64,
    pub queue: f64,
    pub backlog_slope: f64,
    /// Infinite (serialized `null`) when no deadline applies.
    #[cfg_attr(test, ts(type = "number | null"))]
    pub deadline_slack_ms: f64,
    pub delay_ms: f64,
    #[cfg_attr(test, ts(type = "number"))]
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
    /// The observed workload: ingest rates, outputs by protocol, stages and
    /// the egress shard count.
    async fn capacity_workload(&self) -> Workload {
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
        workload
    }

    /// Observe the current runtime topology and counters without making an
    /// admission decision. The snapshot is intentionally a control-plane
    /// operation; no native shard hot path reads these locks.
    pub async fn capacity_snapshot(&self) -> CapacitySnapshot {
        let mut snapshot = CapacityModel {
            limits: self.config.capacity_limits,
            ..CapacityModel::default()
        }
        .project(self.capacity_workload().await);
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
            snapshot.flow_measured = true;
        }
        snapshot
    }

    /// One sample of the current workload projected onto the calibrated
    /// priors twice: as it is (`baseline`) and with `outputs` more outputs
    /// and `stages` more shared stages (`projected`). Both are priors only,
    /// so they compare like with like; the measured flow of running shards
    /// is [`Self::capacity_snapshot`]'s, and nothing measures a change that
    /// has not happened.
    pub async fn capacity_scenario(
        &self,
        outputs: u32,
        stages: u32,
    ) -> (CapacitySnapshot, CapacitySnapshot) {
        let workload = self.capacity_workload().await;
        let model = CapacityModel {
            limits: self.config.capacity_limits,
            ..CapacityModel::default()
        };
        let changed = Workload {
            outputs: workload.outputs.saturating_add(outputs),
            stage_count: workload.stage_count.saturating_add(stages),
            ..workload
        };
        (model.project(workload), model.project(changed))
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
        assert!(!snapshot.flow_measured, "a projection measures no flow");
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
