//! Configuration for the `egress-duty` mode: env knobs, CPU-mask parsing
//! and validation, derived defaults.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::super::*;

use super::*;
pub(super) const DEFAULT_RESTREAM_BIN: &str = "target/bench/restream";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

pub(super) struct EgressDutyConfig {
    pub(super) outputs: usize,
    pub(super) dest_base: String,
    pub(super) dest_base_source: &'static str,
    pub(super) port_base: u16,
    pub(super) window_secs: u64,
    pub(super) shard_cpu: u32,
    pub(super) peer_cpus: String,
    pub(super) harness_cpus: String,
    pub(super) restream_cpus: Option<String>,
    pub(super) receiver_bin: PathBuf,
    pub(super) receiver_secs: u64,
    pub(super) receiver_latency_ms: u64,
    pub(super) receiver_queue_horizon_ms: Option<u64>,
    pub(super) restream_bin: PathBuf,
    pub(super) netns: Option<String>,
    pub(super) work_dir: PathBuf,
    pub(super) bitrate: String,
    pub(super) progress_timeout_secs: u64,
    pub(super) drain_settle_secs: u64,
    pub(super) egress_shards: u32,
}

/// Online CPUs on the host, independent of this process's affinity mask.
pub(super) fn system_cpu_count() -> u32 {
    let online = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    let online = if online > 0 {
        online as u32
    } else {
        std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1) as u32
    };
    online.max(1)
}

impl EgressDutyConfig {
    pub(super) fn from_env() -> Result<Self, String> {
        let available = system_cpu_count();
        let shard_mask = std::env::var("EGRESS_DUTY_SHARD_CPU").unwrap_or_else(|_| "0".to_string());
        let shard_cpus = parse_cpu_mask(&shard_mask)?;
        let shard_cpu = single_cpu(&shard_cpus, "EGRESS_DUTY_SHARD_CPU")?;
        if shard_cpu >= available {
            return Err(format!(
                "EGRESS_DUTY_SHARD_CPU={shard_mask} is not a CPU on this host (available_parallelism={available})"
            ));
        }
        let peer_cpus =
            std::env::var("EGRESS_DUTY_PEER_CPUS").unwrap_or_else(|_| "2-5".to_string());
        let harness_cpus =
            std::env::var("EGRESS_DUTY_HARNESS_CPUS").unwrap_or_else(|_| "1".to_string());
        let restream_cpus = std::env::var("EGRESS_DUTY_RESTREAM_CPUS")
            .ok()
            .filter(|mask| !mask.trim().is_empty());
        validate_cpu_masks(
            shard_cpu,
            &parse_cpu_mask(&harness_cpus)?,
            &parse_cpu_mask(&peer_cpus)?,
            restream_cpus.as_deref().map(parse_cpu_mask).transpose()?,
        )?;
        let window_secs = env_secs("EGRESS_DUTY_WINDOW_SECS", 30).max(1);
        // The receiver's own backstop; the rated window is ended by this
        // harness with SIGTERM, so this only has to outlive it while staying
        // inside the per-port listener's 3 x CONNECT_TIMEOUT admission budget.
        let receiver_secs = std::env::var("EGRESS_DUTY_RECEIVER_SECS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or_else(|| (window_secs + 40).min(70))
            .max(window_secs + 5);
        let work_dir = std::env::var_os("EGRESS_DUTY_WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| artifact_path("egress-duty"));
        let port_base = std::env::var("EGRESS_DUTY_PORT_BASE")
            .ok()
            .and_then(|value| value.trim().parse::<u16>().ok())
            .unwrap_or(12_000);
        let outputs = env_usize("EGRESS_DUTY_OUTPUTS", 11).max(1);
        let last_port = u32::from(port_base) + outputs as u32 - 1;
        if last_port > u32::from(u16::MAX) {
            return Err(format!(
                "EGRESS_DUTY_PORT_BASE={port_base} with {outputs} outputs overflows the port range"
            ));
        }
        let (dest_base, dest_base_source) = match std::env::var("EGRESS_DUTY_DEST_BASE")
            .ok()
            .filter(|value| !value.trim().is_empty())
        {
            Some(explicit) => (explicit, "EGRESS_DUTY_DEST_BASE"),
            // SRT needs a *symmetric* destination: the receiver is wildcard-bound
            // inside the lane namespace, so its replies carry the namespace's own
            // address. An SRT caller pointed at some other address in the
            // namespace-local /16 sees those replies as coming from an unexpected
            // peer and the handshake never completes (measured: the upstream
            // sender itself fails against the upstream receiver that way). The
            // lane's peer address is where the port actually answers.
            None => match std::env::var("RESOURCE_SWEEP_SRT_PEER_HOSTS")
                .ok()
                .and_then(|hosts| {
                    hosts
                        .split(',')
                        .map(str::trim)
                        .find(|host| !host.is_empty())
                        .map(str::to_string)
                }) {
                Some(peer) => (peer, "RESOURCE_SWEEP_SRT_PEER_HOSTS"),
                None => ("10.53.1.1".to_string(), "default"),
            },
        };
        Ok(Self {
            outputs,
            dest_base,
            dest_base_source,
            port_base,
            window_secs,
            shard_cpu,
            peer_cpus,
            harness_cpus,
            restream_cpus,
            receiver_bin: std::env::var_os("EGRESS_DUTY_RECEIVER_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_RECEIVER_BIN)),
            receiver_secs,
            receiver_latency_ms: std::env::var("EGRESS_DUTY_RECEIVER_LATENCY_MS")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .unwrap_or(DEFAULT_RECEIVER_LATENCY_MS),
            // Unset keeps the receiver's own 250 ms horizon (today's argv); set
            // to give each connection a queue bigger than the product's
            // per-visit burst (see `visitBurstBound` in the artifact).
            receiver_queue_horizon_ms: std::env::var("EGRESS_DUTY_RECEIVER_QUEUE_HORIZON_MS")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok()),
            restream_bin: std::env::var_os("EGRESS_DUTY_RESTREAM_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_RESTREAM_BIN)),
            netns: std::env::var("EGRESS_DUTY_NETNS")
                .ok()
                .filter(|netns| !netns.trim().is_empty())
                .or_else(|| std::env::var("RESTREAM_BENCH_NETNS").ok()),
            work_dir,
            bitrate: std::env::var("EGRESS_DUTY_BITRATE").unwrap_or_else(|_| "8M".to_string()),
            progress_timeout_secs: env_secs("EGRESS_DUTY_PROGRESS_TIMEOUT_SECS", 45),
            drain_settle_secs: env_secs("EGRESS_DUTY_DRAIN_SETTLE_SECS", 8),
            egress_shards: std::env::var("EGRESS_DUTY_EGRESS_SHARDS")
                .ok()
                .and_then(|value| value.trim().parse::<u32>().ok())
                .unwrap_or(1),
        })
    }

    /// The Restream child's CPU mask: explicit, or every CPU except the shard.
    ///
    /// Evaluated against the *host's* online CPUs, not this process's current
    /// affinity: the harness pins itself to `HARNESS_CPUS` before spawning
    /// Restream, and an affinity-derived count would then collapse the mask to
    /// nothing (measured: `taskset: failed to parse CPU list:`).
    pub(super) fn restream_mask(&self) -> String {
        self.restream_cpus.clone().unwrap_or_else(|| {
            let cpus: Vec<u32> = (0..system_cpu_count())
                .filter(|cpu| *cpu != self.shard_cpu)
                .collect();
            cpus.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        })
    }

    pub(super) fn output_url(&self, index: usize) -> String {
        format!(
            "srt://{}:{}?streamid=publish:egress-duty-{index:03}",
            self.dest_base,
            u32::from(self.port_base) + index as u32
        )
    }

    pub(super) fn json(&self) -> Value {
        json!({
            "outputs": self.outputs,
            "destBase": self.dest_base,
            "destBaseSource": self.dest_base_source,
            "portBase": self.port_base,
            "windowSecs": self.window_secs,
            "shardCpu": self.shard_cpu,
            "peerCpus": self.peer_cpus,
            "harnessCpus": self.harness_cpus,
            "restreamCpus": self.restream_mask(),
            "restreamCpusExplicit": self.restream_cpus,
            "receiverBin": self.receiver_bin.display().to_string(),
            "receiverSecs": self.receiver_secs,
            "receiverLatencyMs": self.receiver_latency_ms,
            "receiverQueueHorizonMs": self.receiver_queue_horizon_ms,
            "restreamBin": self.restream_bin.display().to_string(),
            "netns": self.netns,
            "workDir": self.work_dir.display().to_string(),
            "bitrate": self.bitrate,
            "egressShards": self.egress_shards,
            "progressTimeoutSecs": self.progress_timeout_secs,
            "drainSettleSecs": self.drain_settle_secs,
        })
    }
}

// ---------------------------------------------------------------------------
// Pure CPU-mask and thread-selection logic
// ---------------------------------------------------------------------------

/// Parse an `n`, `a-b`, or comma-separated CPU mask into a set.
pub(super) fn parse_cpu_mask(mask: &str) -> Result<BTreeSet<u32>, String> {
    let mut cpus = BTreeSet::new();
    let mut saw_token = false;
    for token in mask.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        saw_token = true;
        let (start, end) = match token.split_once('-') {
            Some((start, end)) => (start.trim(), end.trim()),
            None => (token, token),
        };
        let start: u32 = start
            .parse()
            .map_err(|_| format!("invalid CPU mask {mask:?}: {token:?}"))?;
        let end: u32 = end
            .parse()
            .map_err(|_| format!("invalid CPU mask {mask:?}: {token:?}"))?;
        if end < start {
            return Err(format!(
                "invalid CPU mask {mask:?}: inverted range {token:?}"
            ));
        }
        for cpu in start..=end {
            cpus.insert(cpu);
        }
    }
    if !saw_token {
        return Err(format!("empty CPU mask {mask:?}"));
    }
    Ok(cpus)
}

pub(super) fn single_cpu(cpus: &BTreeSet<u32>, knob: &str) -> Result<u32, String> {
    match cpus.len() {
        1 => Ok(*cpus.iter().next().expect("single CPU")),
        n => Err(format!(
            "{knob} must name exactly one CPU as a mask (got {n} CPUs in {cpus:?})"
        )),
    }
}

/// The sender/shard CPU must be a single CPU disjoint from the harness and the
/// receiver, and (when given explicitly) outside Restream's own mask — that
/// mask exists precisely to keep control/media work off the measured core.
pub(super) fn validate_cpu_masks(
    shard_cpu: u32,
    harness: &BTreeSet<u32>,
    peers: &BTreeSet<u32>,
    restream: Option<BTreeSet<u32>>,
) -> Result<(), String> {
    if harness.contains(&shard_cpu) {
        return Err(format!(
            "EGRESS_DUTY_SHARD_CPU={shard_cpu} overlaps EGRESS_DUTY_HARNESS_CPUS={harness:?}"
        ));
    }
    if peers.contains(&shard_cpu) {
        return Err(format!(
            "EGRESS_DUTY_SHARD_CPU={shard_cpu} overlaps EGRESS_DUTY_PEER_CPUS={peers:?}"
        ));
    }
    if let Some(restream) = restream
        && restream.contains(&shard_cpu)
    {
        return Err(format!(
            "EGRESS_DUTY_SHARD_CPU={shard_cpu} is inside EGRESS_DUTY_RESTREAM_CPUS={restream:?}; \
             Restream must run on every CPU except the measured egress shard's"
        ));
    }
    Ok(())
}
