//! Run-level metadata and build provenance for the packet-rate contract.
//!
//! Split from `packet_contract.rs`: this is what the artifact says about the
//! tree, the workload and the binaries that produced it, not what was measured.

use serde_json::{Value, json};

use super::ResourceSweepEnv;

use super::packet_contract_peers::PeerStateConfig;

/// Run-level metadata every artifact carries, so a result can never be read
/// without its tree, workload and environment.
pub(super) struct RunMetadata {
    pub(super) git_sha: Option<String>,
    pub(super) git_dirty: Option<bool>,
    /// Which tree the bench binaries were built from, read from the stamp
    /// `scripts/build/bench-harness.sh` writes next to them. A clean SHA at run
    /// time does not prove the executed binary came from it.
    pub(super) build: Option<BuildProvenance>,
    /// Resource-sweep lifecycle (`isolated` is the contractual one).
    pub(super) lifecycle: String,
    pub(super) restream_binary: String,
    /// Whether `RESTREAM_BIN` was overridden. The provenance stamp covers the
    /// default sibling binary, so an explicit override cannot be proven from
    /// the stamp and is rejected for baseline eligibility.
    pub(super) restream_bin_explicit: bool,
    pub(super) bitrate_label: String,
    pub(super) peer_mode: String,
    pub(super) peer_targets: Vec<String>,
    /// Present only when the rung points at remote sink peers, in which case
    /// their state endpoints gate the rung's validity.
    pub(super) peer_state: Option<PeerStateConfig>,
    pub(super) sample_secs: u64,
    pub(super) settle_secs: u64,
    pub(super) sample_interval_ms: u64,
    pub(super) egress_counts: Vec<usize>,
    pub(super) scenario_filter: Vec<String>,
}

impl RunMetadata {
    pub(super) fn from_env(env: &ResourceSweepEnv) -> Self {
        Self {
            git_sha: git_output(&["rev-parse", "HEAD"]),
            // Untracked-but-not-ignored files count as dirty: a "clean tree"
            // claim must mean the whole work tree, not just tracked files.
            git_dirty: git_output(&["status", "--porcelain"])
                .map(|status| !status.trim().is_empty()),
            build: read_build_provenance(),
            lifecycle: env.lifecycle.as_str().to_string(),
            restream_binary: env.restream_bin.display().to_string(),
            restream_bin_explicit: std::env::var_os("RESTREAM_BIN").is_some(),
            bitrate_label: env.bitrate.clone(),
            peer_mode: env.peer_mode.as_str().to_string(),
            peer_targets: env.srt_peer_targets(),
            peer_state: env.peer_state_config(),
            sample_secs: env.sample_secs,
            settle_secs: env.settle_secs,
            sample_interval_ms: env.sample_interval_ms,
            egress_counts: env.egress_counts.clone(),
            scenario_filter: env
                .scenario_filter
                .as_ref()
                .map(|set| {
                    let mut filter: Vec<String> = set.iter().cloned().collect();
                    filter.sort();
                    filter
                })
                .unwrap_or_default(),
        }
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "generatedAt": chrono::Utc::now().to_rfc3339(),
            "gitSha": self.git_sha,
            "gitDirty": self.git_dirty,
            "restreamBinary": self.restream_binary,
            "restreamBinExplicit": self.restream_bin_explicit,
            "bitrateLabel": self.bitrate_label,
            "peerMode": self.peer_mode,
            "buildProvenance": self.build.as_ref().map(|build| json!({
                "gitSha": build.git_sha,
                "gitDirty": build.git_dirty,
                "builtAt": build.built_at,
            })),
            "lifecycle": self.lifecycle,
            "peerTargets": self.peer_targets,
            "peerStateEndpoint": self.peer_state.as_ref().map(|state| json!({
                "hosts": state.hosts,
                "port": state.port,
            })),
            "sampleSecs": self.sample_secs,
            "settleSecs": self.settle_secs,
            "sampleIntervalMs": self.sample_interval_ms,
            "egressCounts": self.egress_counts,
            "scenarioFilter": self.scenario_filter,
        })
    }
}

/// Build provenance of the running bench binaries, from the stamp the bench
/// build writes beside them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BuildProvenance {
    pub(super) git_sha: String,
    pub(super) git_dirty: bool,
    pub(super) built_at: Option<String>,
}

/// Read `<dir-of-this-binary>/build-provenance.json`. `None` when the stamp is
/// absent or unreadable, which makes a rung ineligible for a baseline rather
/// than silently trusted.
pub(super) fn read_build_provenance() -> Option<BuildProvenance> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.join("build-provenance.json");
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    Some(BuildProvenance {
        git_sha: value["gitSha"].as_str()?.to_string(),
        git_dirty: value["gitDirty"].as_bool().unwrap_or(true),
        built_at: value["builtAt"].as_str().map(str::to_string),
    })
}

/// One `git` probe; `None` when git is unavailable or the call fails.
fn git_output(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
