use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Deserialize;

use super::super::{
    HarnessSrtCrypto, default_restream_bin, default_work_db_path, env_secs, harness_port_defaults,
    harness_srt_crypto_from_env,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ResourceSweepLifecycle {
    Isolated,
    Continuous,
    Cumulative,
}

impl ResourceSweepLifecycle {
    fn from_env() -> Result<Self, String> {
        match std::env::var("RESOURCE_SWEEP_LIFECYCLE")
            .unwrap_or_else(|_| "isolated".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "isolated" => Ok(Self::Isolated),
            "continuous" => Ok(Self::Continuous),
            "cumulative" => Ok(Self::Cumulative),
            other => Err(format!(
                "RESOURCE_SWEEP_LIFECYCLE must be isolated, continuous, or cumulative (got {other})"
            )),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Isolated => "isolated",
            Self::Continuous => "continuous",
            Self::Cumulative => "cumulative",
        }
    }
}

/// Environment and output paths for resource-sweep measurement runs.
#[derive(Clone)]
pub(super) struct ResourceSweepEnv {
    pub(super) work_dir: PathBuf,
    pub(super) summary_json: PathBuf,
    pub(super) summary_csv: PathBuf,
    pub(super) samples_jsonl: PathBuf,
    pub(super) restream_log: PathBuf,
    pub(super) mediamtx_log: PathBuf,
    pub(super) mediamtx_config: PathBuf,
    pub(super) restream_bin: PathBuf,
    pub(super) restream_db_path: PathBuf,
    pub(super) restream_http: u16,
    pub(super) restream_rtmp: u16,
    pub(super) restream_srt: u16,
    pub(super) mtx_rtmp: u16,
    pub(super) mtx_srt: u16,
    pub(super) mtx_api: u16,
    pub(super) sample_secs: u64,
    pub(super) sample_interval_ms: u64,
    pub(super) settle_secs: u64,
    pub(super) ingest_counts: Vec<usize>,
    pub(super) egress_counts: Vec<usize>,
    pub(super) scenario_filter: Option<HashSet<String>>,
    pub(super) lifecycle: ResourceSweepLifecycle,
    pub(super) no_cleanup: bool,
    pub(super) srt_crypto: HarnessSrtCrypto,
    pub(super) backend_policy_env: Vec<(&'static str, String)>,
}

impl ResourceSweepEnv {
    pub(super) fn from_env() -> Result<Self, String> {
        Self::from_env_with_default_dir(".local/artifacts/resource-sweep")
    }

    pub(super) fn from_env_with_default_dir(default_dir: &str) -> Result<Self, String> {
        let work_dir = std::env::var_os("WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(default_dir));
        let ports = harness_port_defaults();
        Ok(Self {
            summary_json: work_dir.join("resource-sweep-results.json"),
            summary_csv: work_dir.join("resource-sweep-results.csv"),
            samples_jsonl: work_dir.join("resource-sweep-samples.jsonl"),
            restream_log: work_dir.join("restream.log"),
            mediamtx_log: work_dir.join("mediamtx.log"),
            mediamtx_config: work_dir.join("mediamtx.yml"),
            restream_bin: default_restream_bin(),
            restream_db_path: std::env::var_os("RESTREAM_DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_work_db_path(&work_dir, "resource-sweep.db")),
            restream_http: ports.restream_http,
            restream_rtmp: ports.restream_rtmp,
            restream_srt: ports.restream_srt,
            mtx_rtmp: ports.mtx_rtmp,
            mtx_srt: ports.mtx_srt,
            mtx_api: ports.mtx_api,
            sample_secs: env_secs("RESOURCE_SWEEP_SAMPLE_SECS", 6),
            sample_interval_ms: env_secs("RESOURCE_SWEEP_SAMPLE_INTERVAL_MS", 1000),
            settle_secs: env_secs("RESOURCE_SWEEP_SETTLE_SECS", 4),
            ingest_counts: parse_usize_list("RESOURCE_SWEEP_INGEST_COUNTS", "1,3,5"),
            egress_counts: parse_usize_list("RESOURCE_SWEEP_EGRESS_COUNTS", "1,5,10"),
            scenario_filter: parse_string_set("RESOURCE_SWEEP_SCENARIOS"),
            lifecycle: ResourceSweepLifecycle::from_env()?,
            no_cleanup: std::env::var("RESOURCE_SWEEP_NO_CLEANUP")
                .ok()
                .is_some_and(|v| v == "1"),
            srt_crypto: harness_srt_crypto_from_env(),
            backend_policy_env: Vec::new(),
            work_dir,
        })
    }

    pub(super) fn scenario_enabled(&self, scenario: &str) -> bool {
        self.scenario_filter
            .as_ref()
            .is_none_or(|filter| filter.contains(scenario))
    }
}

pub(super) fn parse_usize_list(name: &str, default: &str) -> Vec<usize> {
    std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .filter_map(|part| part.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .collect()
}

pub(super) fn parse_string_set(name: &str) -> Option<HashSet<String>> {
    let values: HashSet<String> = std::env::var(name)
        .ok()?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    (!values.is_empty()).then_some(values)
}

pub(super) fn parse_sweep_configs(name: &str) -> Result<Vec<SweepConfig>, String> {
    let raw = std::env::var(name).unwrap_or_else(|_| {
        sweep_configs()
            .iter()
            .map(|config| config.name)
            .collect::<Vec<_>>()
            .join(",")
    });
    let mut output = Vec::new();
    for part in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let config = sweep_configs()
            .iter()
            .copied()
            .find(|config| config.name == part)
            .ok_or_else(|| format!("unknown sweep config {part:?}"))?;
        output.push(config);
    }
    if output.is_empty() {
        return Err(format!("{name} produced no configs"));
    }
    Ok(output)
}

/// Input fixture shape used by resource and bitrate sweep families.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SweepConfig {
    pub(crate) name: &'static str,
    pub(crate) ingest_proto: &'static str,
    pub(crate) video_codec: &'static str,
    pub(crate) multi_audio: bool,
}

static SWEEP_CONFIGS_FROM_DSL: OnceLock<Vec<SweepConfig>> = OnceLock::new();

pub(super) fn sweep_configs() -> &'static [SweepConfig] {
    SWEEP_CONFIGS_FROM_DSL.get_or_init(|| {
        serde_json::from_str(include_str!("../sweep_configs.json"))
            .expect("embedded sweep_configs.json should define valid sweep rows")
    })
}
