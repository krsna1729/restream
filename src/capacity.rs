//! Measured service-center capacity and lightweight Flow Doctor primitives.
//!
//! Connection count is deliberately absent from the admission calculation.
//! The model consumes observed work (bytes/packets, stages, and fanout) and
//! reports the hottest calibrated service center. It starts observe-only.

use std::collections::HashMap;

use serde::Serialize;

use crate::media::engine::MediaEngine;

/// The projection's formula. 2: each output carries its own pipeline's
/// ingest, sent as its protocol packetizes it (1: every output carried the
/// sum of all ingests, protocol-blind).
pub const CAPACITY_MODEL_VERSION: u32 = 2;

/// The largest SRT message payload the engine sends: seven 188-byte MPEG-TS
/// packets.
const SRT_DATAGRAM_BYTES: f64 = 1316.0;

/// One pipeline's ingest, as sampled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestRate {
    pub bps: f64,
    pub pps: f64,
}

/// How an output sends its pipeline's media, for packet pricing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputProtocol {
    /// RTMP and every other stream output: one write per media packet.
    #[default]
    Rtmp,
    /// RTMP over TLS: packetized as RTMP. The TLS cost is not priced.
    Rtmps,
    /// MPEG-TS in datagrams of at most 1316 bytes.
    Srt,
}

impl OutputProtocol {
    fn of_egress(protocol: &str) -> Self {
        match protocol {
            "srt" => Self::Srt,
            "rtmps" => Self::Rtmps,
            _ => Self::Rtmp,
        }
    }

    /// Packets per second one output sends of `ingest`. SRT cuts the
    /// stream into datagrams, so it sends at least the bitrate over the
    /// datagram size; a stream output writes each media packet once.
    pub fn packets_per_sec(self, ingest: IngestRate) -> f64 {
        match self {
            Self::Rtmp | Self::Rtmps => ingest.pps,
            Self::Srt => ingest.pps.max(ingest.bps / 8.0 / SRT_DATAGRAM_BYTES),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Workload {
    pub outputs: u32,
    /// Every ingest summed: what the ingress center receives.
    pub media_bps: u64,
    pub media_pps: u64,
    /// Every output's pipeline ingest summed: what the egress center and
    /// the NIC send before retransmission.
    pub fanout_bps: f64,
    pub fanout_pps: f64,
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
            fanout_bps: 0.0,
            fanout_pps: 0.0,
            loss_rate: 0.0,
            stage_count: 0,
            shard_count: 1,
        }
    }
}

impl Workload {
    /// `count` more outputs of `ingest`, sent over `protocol`.
    pub fn with_outputs(self, count: u32, ingest: IngestRate, protocol: OutputProtocol) -> Self {
        let count_f = f64::from(count);
        Self {
            outputs: self.outputs.saturating_add(count),
            fanout_bps: self.fanout_bps + count_f * ingest.bps,
            fanout_pps: self.fanout_pps + count_f * protocol.packets_per_sec(ingest),
            ..self
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
        let egress_pps = workload.fanout_pps * (1.0 + workload.loss_rate.max(0.0));
        let shard_count = f64::from(workload.shard_count.max(1));
        let hottest_shard = ratio(egress_pps / shard_count, self.limits.egress_pps);
        let egress = ratio(egress_pps, self.limits.egress_pps * shard_count);
        let nic_bps = workload.fanout_bps * (1.0 + workload.loss_rate.max(0.0));
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

/// One egress shard's measured flow: its command queue against its bound,
/// with a non-healthy shard counted as a growing backlog and progress older
/// than 1 s as a missed deadline. The engine's measured flow is the deepest
/// shard's (`MediaEngine::capacity_snapshot`).
pub fn shard_flow(depth: f64, capacity: f64, progress_age_ms: f64, stalled: bool) -> FlowDiagnosis {
    diagnose(FlowSample {
        center: ServiceCenter::Egress,
        arrival_rate: depth,
        service_rate: capacity.max(1.0),
        queue: depth,
        backlog_slope: if stalled { 1.0 } else { 0.0 },
        delay_ms: progress_age_ms,
        deadline_slack_ms: 1_000.0 - progress_age_ms,
        errors: u64::from(stalled),
        amplification: 1.0,
    })
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

/// Added outputs by how they send.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputsByProtocol {
    pub rtmp: u32,
    pub rtmps: u32,
    pub srt: u32,
}

/// A topology change to price: more outputs by protocol, carrying
/// `pipeline_id`'s ingest (the highest-bitrate live one when `None`), and
/// `stages` more shared stages.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScenarioChange {
    pub outputs: OutputsByProtocol,
    pub stages: u32,
    pub pipeline_id: Option<String>,
}

/// One scenario priced: `baseline` and `projected` share one workload
/// sample; `rated_by` is the pipeline whose ingest the added outputs carry
/// (`None`: no ingest is live, so they carry nothing) at `ingest`.
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub baseline: CapacitySnapshot,
    pub projected: CapacitySnapshot,
    pub rated_by: Option<String>,
    pub ingest: IngestRate,
}

/// The scenario named a pipeline with no live ingest: its rate is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoLiveIngest(pub String);

/// Where each limit comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LimitSource {
    /// `CapacityLimits::from_parallelism`: a conservative prior from the CPU
    /// count, not a measurement of this host.
    #[default]
    Prior,
    /// Set by the operator (`RESTREAM_CAPACITY_*`), presumably measured.
    Configured,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitSources {
    pub ingress_pps: LimitSource,
    pub egress_pps: LimitSource,
    pub nic_bps: LimitSource,
    pub memory_bytes: LimitSource,
    pub ffmpeg_stages: LimitSource,
    pub disk_bps: LimitSource,
}

/// What a projection was priced on: the formula, each limit and where it
/// came from, the CPU count the priors scale with, and the safe threshold.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    pub model_version: u32,
    pub limits: CapacityLimits,
    pub sources: LimitSources,
    pub prior_cpus: u32,
    pub safe_utilization: f64,
}

impl MediaEngine {
    /// Each live pipeline's ingest rate, by pipeline id.
    async fn ingest_rates(&self) -> HashMap<String, IngestRate> {
        let ingests = self.ingests.active.read().await;
        ingests
            .iter()
            .map(|(pipeline_id, ingest)| {
                let bps = MediaEngine::sample_ingest_bitrate_kbps(ingest)
                    .map_or(0.0, |rate| (rate * 1_000.0).max(0.0));
                let pps = ingest.metrics.snapshot().packets_per_sec as f64;
                (pipeline_id.clone(), IngestRate { bps, pps })
            })
            .collect()
    }

    /// The observed workload: ingest rates, each output's fan-out of its
    /// own pipeline, stages and the egress shard count.
    async fn capacity_workload(&self, rates: &HashMap<String, IngestRate>) -> Workload {
        let mut workload = Workload {
            media_bps: rates.values().map(|rate| rate.bps as u64).sum(),
            media_pps: rates.values().map(|rate| rate.pps as u64).sum(),
            ..Workload::default()
        };
        for egress in self.egresses.active.read().await.values() {
            let ingest = rates.get(&egress.pipeline_id).copied().unwrap_or_default();
            workload =
                workload.with_outputs(1, ingest, OutputProtocol::of_egress(&egress.protocol));
        }
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

    fn capacity_model(&self) -> CapacityModel {
        CapacityModel {
            limits: self.config.capacity_limits,
            ..CapacityModel::default()
        }
    }

    /// The calibration every projection is priced on.
    pub fn capacity_calibration(&self) -> Calibration {
        let model = self.capacity_model();
        Calibration {
            model_version: CAPACITY_MODEL_VERSION,
            limits: model.limits,
            sources: self.config.capacity_sources,
            prior_cpus: self.config.capacity_prior_cpus,
            safe_utilization: model.safe_utilization,
        }
    }

    /// The current workload projected onto the calibrated priors alone, with
    /// no measured flow.
    pub async fn capacity_priors(&self) -> CapacitySnapshot {
        let rates = self.ingest_rates().await;
        self.capacity_model()
            .project(self.capacity_workload(&rates).await)
    }

    /// Observe the current runtime topology and counters without making an
    /// admission decision. The snapshot is intentionally a control-plane
    /// operation; no native shard hot path reads these locks.
    pub async fn capacity_snapshot(&self) -> CapacitySnapshot {
        let mut snapshot = self.capacity_priors().await;
        if let Some((depth, capacity, progress_age_ms, stalled)) = self
            .egress_fabric_shard_statuses(std::time::Duration::from_secs(5))
            .await
            .iter()
            .map(|status| {
                (
                    f64::from(status.command_depth),
                    f64::from(status.command_capacity),
                    status.progress_age_ms.unwrap_or_default() as f64,
                    !matches!(
                        status.state,
                        crate::media::egress::shard::EgressShardHealth::Healthy
                    ),
                )
            })
            .max_by(|left, right| left.0.total_cmp(&right.0))
        {
            let flow = shard_flow(depth, capacity, progress_age_ms, stalled);
            snapshot.projected_utilization =
                snapshot.projected_utilization.max(flow.utilization as f32);
            snapshot.flow = flow;
            snapshot.flow_measured = true;
        }
        snapshot
    }

    /// One sample of the current workload projected onto the calibrated
    /// priors twice: as it is (`baseline`) and with the change
    /// (`projected`). Both are priors only, so they compare like with like;
    /// the measured flow of running shards is [`Self::capacity_snapshot`]'s,
    /// and nothing measures a change that has not happened.
    pub async fn capacity_scenario(
        &self,
        change: &ScenarioChange,
    ) -> Result<Scenario, NoLiveIngest> {
        let rates = self.ingest_rates().await;
        let (rated_by, ingest) = scenario_rate(&rates, change.pipeline_id.as_deref())?;
        let workload = self.capacity_workload(&rates).await;
        let model = self.capacity_model();
        let added = change.outputs;
        let changed = Workload {
            stage_count: workload.stage_count.saturating_add(change.stages),
            ..workload
                .with_outputs(added.rtmp, ingest, OutputProtocol::Rtmp)
                .with_outputs(added.rtmps, ingest, OutputProtocol::Rtmps)
                .with_outputs(added.srt, ingest, OutputProtocol::Srt)
        };
        Ok(Scenario {
            baseline: model.project(workload),
            projected: model.project(changed),
            rated_by,
            ingest,
        })
    }
}

/// The ingest the scenario's outputs carry: the named pipeline's, which must
/// be live, or else the highest-bitrate live one.
fn scenario_rate(
    rates: &HashMap<String, IngestRate>,
    pipeline_id: Option<&str>,
) -> Result<(Option<String>, IngestRate), NoLiveIngest> {
    match pipeline_id {
        Some(id) => rates
            .get(id)
            .map(|rate| (Some(id.to_string()), *rate))
            .ok_or_else(|| NoLiveIngest(id.to_string())),
        None => Ok(rates
            .iter()
            .max_by(|left, right| {
                left.1
                    .bps
                    .total_cmp(&right.1.bps)
                    .then_with(|| right.0.cmp(left.0))
            })
            .map_or((None, IngestRate::default()), |(id, rate)| {
                (Some(id.clone()), *rate)
            })),
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
        .project(
            Workload {
                media_bps: 10_000_000,
                media_pps: 100,
                shard_count: 4,
                ..Workload::default()
            }
            .with_outputs(
                100,
                IngestRate {
                    bps: 10_000_000.0,
                    pps: 100.0,
                },
                OutputProtocol::Rtmp,
            ),
        );
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

    fn rate(bps: f64, pps: f64) -> IngestRate {
        IngestRate { bps, pps }
    }

    #[test]
    fn an_output_carries_its_own_pipelines_ingest_not_the_sum() {
        // Two pipelines, 6 and 1 Mb/s; one output on the small one.
        let rates = HashMap::from([
            ("big".to_string(), rate(6_000_000.0, 600.0)),
            ("small".to_string(), rate(1_000_000.0, 100.0)),
        ]);
        let workload = Workload {
            media_bps: 7_000_000,
            media_pps: 700,
            ..Workload::default()
        }
        .with_outputs(1, rates["small"], OutputProtocol::Rtmp);
        let snapshot = CapacityModel::default().project(workload);
        assert_eq!(
            snapshot.egress_pps, 100.0,
            "not 700: the sum of both ingests"
        );
    }

    #[test]
    fn srt_sends_the_bitrate_in_1316_byte_datagrams() {
        // 6 Mb/s in 100 media packets a second: SRT cuts it into
        // 6e6 / 8 / 1316 = 570 datagrams; RTMP writes the 100 packets.
        let ingest = rate(6_000_000.0, 100.0);
        assert_eq!(OutputProtocol::Rtmp.packets_per_sec(ingest), 100.0);
        assert_eq!(OutputProtocol::Rtmps.packets_per_sec(ingest), 100.0);
        let srt = OutputProtocol::Srt.packets_per_sec(ingest);
        assert!((srt - 6_000_000.0 / 8.0 / 1316.0).abs() < 1e-9, "{srt}");
        // A packet rate above the datagram rate stands: never fewer packets.
        assert_eq!(
            OutputProtocol::Srt.packets_per_sec(rate(8_000.0, 50.0)),
            50.0
        );
    }

    #[test]
    fn a_scenario_rates_the_named_pipeline_or_the_highest_bitrate() {
        let rates = HashMap::from([
            ("a".to_string(), rate(1_000_000.0, 100.0)),
            ("b".to_string(), rate(5_000_000.0, 400.0)),
        ]);
        assert_eq!(
            scenario_rate(&rates, Some("a")),
            Ok((Some("a".to_string()), rates["a"]))
        );
        assert_eq!(
            scenario_rate(&rates, None),
            Ok((Some("b".to_string()), rates["b"])),
            "unnamed: the highest-bitrate live ingest"
        );
        assert_eq!(
            scenario_rate(&rates, Some("gone")),
            Err(NoLiveIngest("gone".to_string()))
        );
        assert_eq!(
            scenario_rate(&HashMap::new(), None),
            Ok((None, IngestRate::default())),
            "nothing live: the outputs carry nothing"
        );
    }

    proptest! {
        #[test]
        fn projection_preserves_nonnegative_work(
            outputs in 0u32..10_000,
            media_bps in 0u64..1_000_000_000,
            media_pps in 0u64..1_000_000,
            stages in 0u32..128,
        ) {
            let ingest = IngestRate { bps: media_bps as f64, pps: media_pps as f64 };
            let snapshot = CapacityModel::default().project(
                Workload {
                    media_bps,
                    media_pps,
                    stage_count: stages,
                    ..Workload::default()
                }
                .with_outputs(outputs, ingest, OutputProtocol::Srt),
            );
            prop_assert!(snapshot.ingress_pps >= 0.0);
            prop_assert!(snapshot.media_bps >= 0.0);
            prop_assert!(snapshot.egress_pps >= 0.0);
            prop_assert!(snapshot.active_leaves == outputs);
            prop_assert!(snapshot.unique_stages == stages);
        }
    }
}
