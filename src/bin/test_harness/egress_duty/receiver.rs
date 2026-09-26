//! The pinned independent receiver: its `--out` TSV contract, its `STATS`
//! line, the lane network-namespace prefix it is launched through, and the
//! process-tree walk that resolves its real pid.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::super::*;

use super::*;
pub(super) const DEFAULT_RECEIVER_BIN: &str =
    "/home/dev/.cargo/git/checkouts/srt-rs-2a4e85d4a1e5ceb4/86369b0/target/release/srt-bench";

/// Default `latency_ms` for the receiver: srt-bench's 3rd positional is
/// `<latency_ms>` (the TSBPD delay), not a connect timeout. 120 ms matches the
/// Stage C rows, so C and D compare like-for-like.
pub(super) const DEFAULT_RECEIVER_LATENCY_MS: u64 = 120;
/// Receiver-TSV columns this mode consumes. Missing ⇒ reject the row rather
/// than read an absent counter as zero.
pub(super) const RECEIVER_REQUIRED_COLUMNS: &[&str] = &[
    "pkt_sent",
    "core_total",
    "sec_a",
    "sec_b",
    "established",
    "elapsed_s",
    "cpu_user_ms",
    "cpu_sys_ms",
    "udp_rcvbuf_err",
    "udp_in_err",
    "udp_no_ports",
    "datapath_q_dropped",
    "local_dropped",
    "retry_overflow",
];

// ---------------------------------------------------------------------------
// Receiver TSV
// ---------------------------------------------------------------------------

/// One parsed receiver result row, keyed by the TSV header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReceiverRow {
    pub(super) fields: BTreeMap<String, String>,
}

impl ReceiverRow {
    pub(super) fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    pub(super) fn number(&self, key: &str) -> Option<u64> {
        self.get(key)?.trim().parse::<u64>().ok()
    }

    pub(super) fn float(&self, key: &str) -> Option<f64> {
        self.get(key)?.trim().parse::<f64>().ok()
    }

    pub(super) fn json(&self) -> Value {
        let mut out = serde_json::Map::new();
        for key in RECEIVER_REQUIRED_COLUMNS {
            let value = self
                .get(key)
                .and_then(|raw| raw.trim().parse::<f64>().ok())
                .map_or_else(|| json!({"raw": self.get(key)}), |number| json!(number));
            out.insert((*key).to_string(), value);
        }
        Value::Object(out)
    }
}

/// Parse the pinned receiver's `--out` TSV: one header row plus exactly one
/// data row per process. A row whose width disagrees with its header is
/// rejected — a shifted row would silently report another column's number as
/// `pkt_sent`.
pub(super) fn parse_receiver_tsv(text: &str) -> Result<ReceiverRow, String> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header_line = lines.next().ok_or("receiver TSV is empty")?;
    let header: Vec<&str> = header_line.split('\t').map(str::trim).collect();
    for required in RECEIVER_REQUIRED_COLUMNS {
        if !header.contains(required) {
            return Err(format!(
                "receiver TSV header is missing required column {required:?}"
            ));
        }
    }
    let row_line = lines
        .next()
        .ok_or("receiver TSV has a header but no result row")?;
    let values: Vec<&str> = row_line.split('\t').map(str::trim).collect();
    if values.len() != header.len() {
        return Err(format!(
            "receiver TSV row has {} columns but its header declares {}",
            values.len(),
            header.len()
        ));
    }
    if let Some(extra) = lines.next() {
        return Err(format!(
            "pinned receiver appends exactly one row per process; found an extra row starting {:?}",
            extra.chars().take(32).collect::<String>()
        ));
    }
    Ok(ReceiverRow {
        fields: header
            .iter()
            .zip(values)
            .map(|(key, value)| ((*key).to_string(), value.to_string()))
            .collect(),
    })
}

/// The receiver's final `STATS ...` line, verbatim.
pub(super) fn receiver_stats_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .rfind(|line| line.trim_start().starts_with("STATS"))
        .map(|line| line.trim().to_string())
}

/// Kernel/network counters sampled from the receiver's network namespace.
/// Missing proc files stay `null`; verdicts reject missing required drop
/// counters instead of treating them as zero.
pub(super) fn read_proc_net_counters(pid: Option<u32>) -> Value {
    let Some(pid) = pid else {
        return json!({
            "pid": Value::Null,
            "udp": Value::Null,
            "interfaces": Value::Null,
            "softnet": Value::Null,
        });
    };
    let root = format!("/proc/{pid}");
    let udp = read_udp_snmp(&format!("{root}/net/snmp"));
    let interfaces = read_net_dev(&format!("{root}/net/dev"));
    let softnet = read_softnet(&format!("{root}/net/softnet_stat"));
    json!({
        "pid": pid,
        "udp": udp,
        "interfaces": interfaces,
        "softnet": softnet,
    })
}

fn read_udp_snmp(path: &str) -> Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Value::Null;
    };
    let mut header: Vec<&str> = Vec::new();
    for line in text.lines().filter(|line| line.starts_with("Udp:")) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        if header.is_empty() {
            header = fields;
            continue;
        }
        if header.len() != fields.len() {
            return Value::Null;
        }
        let mut out = serde_json::Map::new();
        for (key, value) in header.iter().skip(1).zip(fields.iter().skip(1)) {
            let parsed = value
                .parse::<u64>()
                .ok()
                .map_or(Value::Null, |value| json!(value));
            out.insert((*key).to_string(), parsed);
        }
        return Value::Object(out);
    }
    Value::Null
}

fn read_net_dev(path: &str) -> Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Value::Null;
    };
    let mut interface_rx_dropped = 0_u64;
    let mut interface_tx_dropped = 0_u64;
    let mut veth_rx_dropped = 0_u64;
    let mut veth_tx_dropped = 0_u64;
    let mut observed = 0_u64;
    for line in text.lines().skip(2) {
        let Some((name, values)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name == "lo" {
            continue;
        }
        let fields: Vec<&str> = values.split_whitespace().collect();
        if fields.len() < 12 {
            continue;
        }
        let Some(rx_dropped) = fields[3].parse::<u64>().ok() else {
            continue;
        };
        let Some(tx_dropped) = fields[11].parse::<u64>().ok() else {
            continue;
        };
        observed += 1;
        interface_rx_dropped = interface_rx_dropped.saturating_add(rx_dropped);
        interface_tx_dropped = interface_tx_dropped.saturating_add(tx_dropped);
        if name.starts_with("veth") {
            veth_rx_dropped = veth_rx_dropped.saturating_add(rx_dropped);
            veth_tx_dropped = veth_tx_dropped.saturating_add(tx_dropped);
        }
    }
    json!({
        "observedInterfaces": observed,
        "interfaceRxDropped": interface_rx_dropped,
        "interfaceTxDropped": interface_tx_dropped,
        "vethRxDropped": veth_rx_dropped,
        "vethTxDropped": veth_tx_dropped,
    })
}

fn read_softnet(path: &str) -> Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Value::Null;
    };
    let mut processed = 0_u64;
    let mut dropped = 0_u64;
    let mut time_squeeze = 0_u64;
    let mut received_rps = 0_u64;
    let mut flow_limit = 0_u64;
    let mut observed = 0_u64;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let parse_hex = |index: usize| {
            fields
                .get(index)
                .and_then(|value| u64::from_str_radix(value, 16).ok())
        };
        let (Some(row_processed), Some(row_dropped), Some(row_squeeze)) =
            (parse_hex(0), parse_hex(1), parse_hex(2))
        else {
            continue;
        };
        observed += 1;
        processed = processed.saturating_add(row_processed);
        dropped = dropped.saturating_add(row_dropped);
        time_squeeze = time_squeeze.saturating_add(row_squeeze);
        received_rps = received_rps.saturating_add(parse_hex(9).unwrap_or(0));
        flow_limit = flow_limit.saturating_add(parse_hex(10).unwrap_or(0));
    }
    json!({
        "observedRows": observed,
        "processed": processed,
        "dropped": dropped,
        "timeSqueeze": time_squeeze,
        "receivedRps": received_rps,
        "flowLimit": flow_limit,
    })
}

// ---------------------------------------------------------------------------
// Lane / child processes
// ---------------------------------------------------------------------------

/// A resolved `ip netns exec` prefix, plus how it was authorized.
pub(super) struct NetnsExec {
    pub(super) prefix: Vec<String>,
    pub(super) authorized_via_sudo: bool,
}

impl NetnsExec {
    pub(super) fn json(&self) -> Value {
        json!({
            "prefix": self.prefix,
            "sudo": self.authorized_via_sudo,
        })
    }
}

pub(super) fn probe_netns(netns: &str) -> Result<NetnsExec, String> {
    let direct = std::process::Command::new("ip")
        .args(["netns", "exec", netns, "true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if direct.map(|status| status.success()).unwrap_or(false) {
        return Ok(NetnsExec {
            prefix: vec![
                "ip".into(),
                "netns".into(),
                "exec".into(),
                netns.into(),
                "env".into(),
            ],
            authorized_via_sudo: false,
        });
    }
    let sudo = std::process::Command::new("sudo")
        .args(["-n", "ip", "netns", "exec", netns, "true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if sudo.map(|status| status.success()).unwrap_or(false) {
        return Ok(NetnsExec {
            prefix: vec![
                "sudo".into(),
                "-n".into(),
                "ip".into(),
                "netns".into(),
                "exec".into(),
                netns.into(),
                // `ip netns exec` parses leading `NAME=VALUE` arguments as
                // environment assignments, and the receiver's first argument is
                // `runtime=compio`; `env` keeps the receiver's own argv exactly
                // as specified instead of letting iproute2 reinterpret it.
                "env".into(),
            ],
            authorized_via_sudo: true,
        });
    }
    Err(format!(
        "cannot enter network namespace {netns:?}: neither `ip netns exec {netns} true` nor \
         `sudo -n ip netns exec {netns} true` succeeded"
    ))
}

/// Direct child pids of `pid`, from `/proc/<pid>/task/*/children`.
pub(super) fn child_pids(pid: u32) -> Vec<u32> {
    let task_dir = PathBuf::from(format!("/proc/{pid}/task"));
    let Ok(entries) = std::fs::read_dir(&task_dir) else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        let Ok(children) = std::fs::read_to_string(entry.path().join("children")) else {
            continue;
        };
        pids.extend(
            children
                .split_whitespace()
                .filter_map(|value| value.parse::<u32>().ok()),
        );
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// The descendant of `root` whose `comm` matches `comm` — for a receiver
/// launched through `sudo ip netns exec`, the harness's direct child is `sudo`
/// and the receiver is its child.
pub(super) fn descendant_with_comm(root: u32, comm: &str) -> Option<u32> {
    let mut frontier = child_pids(root);
    for _ in 0..4 {
        let mut next = Vec::new();
        for pid in frontier {
            if let Ok(name) = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                && name.trim() == comm
            {
                return Some(pid);
            }
            next.extend(child_pids(pid));
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    None
}

/// A receiver the mode just started: the direct child it owns, the receiver's
/// own pid when it differs (a `sudo ip netns exec` grandchild), and the argv as
/// spawned.
pub(super) struct SpawnedReceiver {
    pub(super) child: Child,
    pub(super) pid: Option<u32>,
    pub(super) sudo: bool,
    pub(super) argv: Vec<String>,
}

/// Spawn the pinned receiver under `netns_prefix`, into `tsv_path`, with its
/// stdout/stderr captured for the `LISTENING` gate and the final `STATS` line.
pub(super) async fn spawn_receiver(
    cfg: &EgressDutyConfig,
    netns: Option<&NetnsExec>,
    tsv_path: &Path,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<SpawnedReceiver, String> {
    // ── Receiver ────────────────────────────────────────────────────────
    // Positionals are `<port> <duration_secs> <latency_ms>`; the remaining
    // flags are the hand-off contract's argv, plus the optional
    // datapath-queue horizon (the receiver's own default derives a
    // per-connection queue smaller than the product's per-visit burst).
    let mut receiver_args: Vec<String> = vec![
        "runtime=compio".into(),
        "mode=receiver".into(),
        cfg.port_base.to_string(),
        cfg.receiver_secs.to_string(),
        cfg.receiver_latency_ms.to_string(),
        "--connections".into(),
        cfg.outputs.to_string(),
        "--ingress".into(),
        "per-port".into(),
        "--encryption".into(),
        cfg.crypto.clone(),
        "--workers".into(),
        "1".into(),
        "--cpus".into(),
        cfg.peer_cpus.clone(),
        "--pin".into(),
        "on".into(),
    ];
    if let Some(horizon_ms) = cfg.receiver_queue_horizon_ms {
        receiver_args.push("--datapath-queue-horizon-ms".into());
        receiver_args.push(horizon_ms.to_string());
    }
    receiver_args.push("--out".into());
    receiver_args.push(tsv_path.display().to_string());
    let receiver_comm = cfg
        .receiver_bin
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("srt-bench")
        .chars()
        .take(COMM_MAX_BYTES)
        .collect::<String>();
    let (mut receiver_cmd, mut full_argv) = match netns {
        Some(netns) => {
            let mut cmd = Command::new(&netns.prefix[0]);
            cmd.args(&netns.prefix[1..]).arg(&cfg.receiver_bin);
            let mut argv = netns.prefix.clone();
            argv.push(cfg.receiver_bin.display().to_string());
            (cmd, argv)
        }
        None => (
            Command::new(&cfg.receiver_bin),
            vec![cfg.receiver_bin.display().to_string()],
        ),
    };
    let stdout_log = std::fs::File::create(stdout_path).map_err(|error| error.to_string())?;
    let stderr_log = std::fs::File::create(stderr_path).map_err(|error| error.to_string())?;
    let sudo = netns.is_some_and(|netns| netns.authorized_via_sudo);
    receiver_cmd
        .args(&receiver_args)
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log))
        .kill_on_drop(true);
    full_argv.extend(receiver_args.iter().cloned());
    let receiver = receiver_cmd
        .spawn()
        .map_err(|error| format!("failed to spawn receiver {}: {error}", full_argv.join(" ")))?;
    let receiver_pid = receiver.id();
    // Resolve the real receiver pid (a `sudo ip netns exec` child) so the clean
    // stop can signal *it* rather than the sudo wrapper.
    let mut resolved_pid = None;
    if let Some(root) = receiver_pid {
        for _ in 0..40 {
            if let Some(pid) = descendant_with_comm(root, &receiver_comm) {
                resolved_pid = Some(pid);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if resolved_pid.is_none() && !sudo {
            resolved_pid = Some(root);
        }
    }
    let spawned = SpawnedReceiver {
        child: receiver,
        pid: resolved_pid,
        sudo,
        argv: full_argv,
    };
    Ok(spawned)
}

/// Wait for the receiver's only readiness signal — `LISTENING` on stdout. It
/// does not have to precede the bind, so this is a liveness gate, not a fence.
pub(super) async fn wait_for_listening(
    stdout_path: &Path,
    stderr_path: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let listen_deadline = Instant::now() + timeout;
    loop {
        let stdout = std::fs::read_to_string(stdout_path).unwrap_or_default();
        if stdout.contains("LISTENING") {
            return Ok(());
        }
        if Instant::now() >= listen_deadline {
            return Err(format!(
                "receiver never printed LISTENING within {}s (see {})",
                timeout.as_secs(),
                stderr_path.display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
