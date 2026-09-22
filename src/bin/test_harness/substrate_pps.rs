//! Substrate UDP submission benchmark: Compio vs native `io_uring`.
//!
//! The roadmap's substrate question is "how many datagrams per CPU-second can
//! one core submit?", so this mode's hot loop contains nothing but UDP
//! submission and completion: no SRT, no MPEG-TS, no media rings, no crypto, no
//! scheduler. Two variants are implemented and held identical in everything
//! that matters — payload bytes, destination sequence, socket model, queue
//! depth and completion semantics:
//!
//! * `compio` — the completion-based UDP submission mechanism Restream's SRT
//!   egress already uses (`compio::net::UdpSocket::send_to` on a Compio
//!   runtime).
//! * `io-uring` — a purpose-built native ring: one `IoUring`, a fixed window of
//!   `SendTo` SQEs per batch, `submit()` then reap, with the same sliding
//!   window.
//!
//! Placement is the experiment's other half: the sender thread is pinned to
//! exactly one CPU, and harness/control threads and the drain peer are pinned
//! to disjoint CPUs. The peer's counters decide attribution — a result below
//! target only counts against the sender while the drain keeps up and its drop
//! counters stay clean; otherwise the run is `receiver-limited`.

#[path = "substrate_pps/arms.rs"]
mod arms;
#[path = "substrate_pps/config.rs"]
mod config;
#[path = "substrate_pps/sender.rs"]
mod sender;
#[cfg(test)]
#[path = "substrate_pps/tests.rs"]
mod tests;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use config::*;
use sender::*;

use super::peer_state::{ThreadCpu, cpus_allowed_list, pin_to_cpuset};

use super::*;

/// How long the receiver may take to account for datagrams the sender already
/// completed. Settlement time is measured separately and never enters the rated
/// window.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of waiting for the receiver to settle after a sender quiescence
/// boundary. A completed send means the sender operation finished; the datagram
/// can still be in the peer's veth/RPS/UDP path, so the receiver is polled until
/// its own count matches — otherwise a perfectly lossless run can read as short
/// delivery purely because the RX path had not drained yet.
#[derive(Debug)]
pub(crate) enum Settlement {
    Settled {
        polls: u32,
        secs: f64,
    },
    /// The receiver counted more datagrams than the sender completed: the
    /// boundary is contaminated rather than settled, and `observed > expected`
    /// must never be reported as success.
    Overshoot {
        expected: u64,
        observed: u64,
        polls: u32,
    },
    Loss {
        field: String,
        before: u64,
        after: u64,
        polls: u32,
    },
    Timeout {
        expected: u64,
        observed: u64,
        polls: u32,
    },
}

pub(crate) async fn settle_receiver(
    url: &str,
    before: &Value,
    expected: u64,
    timeout: Duration,
) -> Result<Settlement, String> {
    let deadline = Instant::now() + timeout;
    let started = Instant::now();
    let mut polls = 0_u32;
    loop {
        polls += 1;
        let state = fetch_peer_state(url).await?;
        // A drop that increments during settlement is immediate evidence that
        // the receiver did not take everything, whatever the totals say.
        for field in [
            "udpRcvbufErrors",
            "udpInErrors",
            "receiveErrors",
            "nicRxDropped",
        ] {
            if let (Some(before), Some(after)) = (counter(before, field), counter(&state, field))
                && after > before
            {
                return Ok(Settlement::Loss {
                    field: field.to_string(),
                    before,
                    after,
                    polls,
                });
            }
        }
        // Both softnet loss columns gate settlement; `timeSqueeze` and
        // `receivedRps` stay diagnostics.
        for field in ["dropped", "flowLimit"] {
            if let (Some(before), Some(after)) = (
                before
                    .get("softnet")
                    .and_then(|softnet| softnet[field].as_u64()),
                state
                    .get("softnet")
                    .and_then(|softnet| softnet[field].as_u64()),
            ) && after > before
            {
                return Ok(Settlement::Loss {
                    field: format!("softnet.{field}"),
                    before,
                    after,
                    polls,
                });
            }
        }
        let observed = counter(&state, "datagrams")
            .zip(counter(before, "datagrams"))
            .map(|(after, before)| after.saturating_sub(before))
            .unwrap_or(0);
        if observed == expected {
            return Ok(Settlement::Settled {
                polls,
                secs: started.elapsed().as_secs_f64(),
            });
        }
        if observed > expected {
            return Ok(Settlement::Overshoot {
                expected,
                observed,
                polls,
            });
        }
        if Instant::now() > deadline {
            return Ok(Settlement::Timeout {
                expected,
                observed,
                polls,
            });
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Netdev transmit counters, read from `/sys/class/net/<dev>/statistics`.
#[derive(Clone, Copy)]
struct TxCounters {
    packets: u64,
    bytes: u64,
}

fn read_tx_counters(dev: &str) -> Option<TxCounters> {
    let read = |name: &str| -> Option<u64> {
        std::fs::read_to_string(format!("/sys/class/net/{dev}/statistics/{name}"))
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
    };
    Some(TxCounters {
        packets: read("tx_packets")?,
        bytes: read("tx_bytes")?,
    })
}

/// Minimal `GET <url>` against the peer's state endpoint.
async fn fetch_peer_state(url: &str) -> Result<Value, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("receiver state URL must start with http:// (got {url:?})"))?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/state".to_string()),
    };
    let mut stream = TcpStream::connect(host)
        .await
        .map_err(|e| format!("connect {host}: {e}"))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("write request: {e}"))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .map_err(|e| format!("read response: {e}"))?;
    let body_start = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .ok_or_else(|| format!("{url}: malformed HTTP response"))?;
    serde_json::from_slice(&response[body_start..])
        .map_err(|e| format!("{url}: state is not JSON: {e}"))
}

/// The lane a run used, copied into the artifact so later A/B rows can prove
/// they share a topology instead of relying on the operator's memory.
fn lane_configuration() -> Value {
    let env = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    json!({
        "topologyKind": env("RESTREAM_BENCH_TOPOLOGY"),
        "netns": env("RESTREAM_BENCH_NETNS"),
        "peerHosts": env("RESOURCE_SWEEP_SRT_PEER_HOSTS"),
        "senderCpusRequested": env("SUBSTRATE_SENDER_CPUS"),
        "harnessCpusRequested": env("SUBSTRATE_HARNESS_CPUS"),
        "receiverCpus": env("WI3_RECEIVER_CPUS"),
        "rpsCpusRequested": env("WI3_RPS_CPUS_REQUESTED"),
        "rpsCpusObserved": env("WI3_RPS_CPUS_OBSERVED"),
        "netdevMaxBacklogOriginal": env("WI3_NETDEV_MAX_BACKLOG_ORIGINAL"),
        "netdevMaxBacklogRequested": env("WI3_NETDEV_MAX_BACKLOG_REQUESTED"),
        "netdevMaxBacklogObserved": env("WI3_NETDEV_MAX_BACKLOG_OBSERVED"),
        "deliveryToleranceRequested": env("SUBSTRATE_DELIVERY_TOLERANCE"),
        "txNetdev": env("SUBSTRATE_TX_NETDEV"),
    })
}

fn settlement_json(settlement: &Option<Settlement>) -> Value {
    match settlement {
        None => Value::Null,
        Some(Settlement::Settled { polls, secs }) => {
            json!({"outcome": "settled", "polls": polls, "secs": secs})
        }
        Some(Settlement::Overshoot {
            expected,
            observed,
            polls,
        }) => json!({
            "outcome": "overshoot",
            "expected": expected,
            "observed": observed,
            "polls": polls,
        }),
        Some(Settlement::Loss {
            field,
            before,
            after,
            polls,
        }) => json!({
            "outcome": "loss",
            "field": field,
            "before": before,
            "after": after,
            "polls": polls,
        }),
        Some(Settlement::Timeout {
            expected,
            observed,
            polls,
        }) => json!({
            "outcome": "timeout",
            "expected": expected,
            "observed": observed,
            "polls": polls,
        }),
    }
}

fn counter(state: &Value, field: &str) -> Option<u64> {
    state.get(field).and_then(Value::as_u64)
}

/// Run one substrate measurement and report it. The window is the same for both
/// variants: warm up, snapshot, send for `duration`, snapshot again.
pub(crate) async fn substrate_pps_mode() -> Result<Value, String> {
    let config = Arc::new(parse_config()?);
    if let Some(mask) = &config.harness_cpus {
        // Harness/control threads first: the sender overrides its own mask.
        pin_to_cpuset(mask)?;
    }
    let payload = build_payload(config.payload_bytes);
    let handles = Arc::new(SenderHandles::new());
    let tx_netdev = std::env::var("SUBSTRATE_TX_NETDEV")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(dev) = &tx_netdev
        && read_tx_counters(dev).is_none()
    {
        return Err(format!(
            "SUBSTRATE_TX_NETDEV={dev:?} has no readable tx_packets/tx_bytes: this lane cannot \
             account for transmitted datagrams and must not be used for attribution"
        ));
    }

    let receiver_before_all = match &config.receiver_state {
        Some(url) => Some(fetch_peer_state(url).await?),
        None => None,
    };

    let sender = std::thread::Builder::new()
        .name("substrate-sender".to_string())
        .spawn({
            let config = Arc::clone(&config);
            let handles = Arc::clone(&handles);
            let payload = payload.clone();
            move || sender_thread(config, payload, handles)
        })
        .map_err(|e| format!("spawn substrate sender: {e}"))?;

    println!(
        "[substrate-pps] run {} variant {} sender cpus {} depth {} dests {} payload {} B; \
         warmup {}s then {}s measured{}",
        config.run,
        config.variant.as_str(),
        config.sender_cpus,
        config.queue_depth,
        config.destinations.len(),
        config.payload_bytes,
        config.warmup.as_secs(),
        config.duration.as_secs(),
        tx_netdev
            .as_ref()
            .map(|dev| format!("; tx accounted on {dev}"))
            .unwrap_or_default()
    );

    // Warm up, then park the sender so both sides are snapshotted over exactly
    // the same interval: a sender that keeps transmitting while the peer is
    // polled would book those datagrams as loss.
    tokio::time::sleep(config.warmup).await;
    pause_sender(&handles).await?;
    let sender_tid = handles.tid.load(Ordering::Relaxed);
    let warm_completed = handles.completed();
    let warm_cpu = handles.cpu_sample();
    // Warmup boundary: the receiver must account for everything the sender
    // completed before the window baseline is taken.
    let warm_settlement = match (&config.receiver_state, &receiver_before_all) {
        (Some(url), Some(before_all)) => {
            Some(settle_receiver(url, before_all, warm_completed, SETTLE_TIMEOUT).await?)
        }
        _ => None,
    };
    // A warmup boundary that did not settle leaves warmup datagrams in the RX
    // path, which would contaminate the rated interval: stop before the window.
    // A warmup boundary that lost or overshot datagrams is not a valid baseline:
    // stop before the rated window rather than measuring on top of it.
    if let Some(
        Settlement::Timeout { .. } | Settlement::Loss { .. } | Settlement::Overshoot { .. },
    ) = warm_settlement
    {
        handles.stop.store(true, Ordering::Relaxed);
        resume_sender(&handles).await?;
        sender
            .join()
            .map_err(|_| "substrate sender thread panicked".to_string())?;
        return Err(format!(
            "warmup boundary did not settle ({}): refusing to start the rated window",
            settlement_json(&warm_settlement)
        ));
    }
    let receiver_before = match &config.receiver_state {
        Some(url) => Some(fetch_peer_state(url).await?),
        None => None,
    };
    let tx_before = tx_netdev.as_deref().and_then(read_tx_counters);
    resume_sender(&handles).await?;
    let window_started = Instant::now();

    // Progress line per interval, so a stalled variant is visible while it runs.
    let mut ticker = tokio::time::interval(Duration::from_secs(config.report_secs));
    ticker.tick().await;
    let mut last_completed = warm_completed;
    let mut last_at = window_started;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let completed = handles.completed();
                let now = Instant::now();
                let secs = now.duration_since(last_at).as_secs_f64().max(1e-9);
                println!(
                    "[substrate-pps] {}",
                    json!({
                        "variant": config.variant.as_str(),
                        "windowSecs": window_started.elapsed().as_secs_f64(),
                        "completed": completed,
                        "pps": (completed.saturating_sub(last_completed)) as f64 / secs,
                        "errors": handles.counters.errors.load(Ordering::Relaxed),
                    })
                );
                last_completed = completed;
                last_at = now;
                if window_started.elapsed() >= config.duration {
                    break;
                }
            }
            _ = tokio::time::sleep(config.duration.saturating_sub(window_started.elapsed())) => break,
        }
    }

    // Park again for the closing snapshot: the rated clock and the sender CPU
    // sample stop at quiescence, so receiver settlement is never charged to the
    // sender.
    pause_sender(&handles).await?;
    let window_secs = window_started.elapsed().as_secs_f64();
    let completed = handles.completed();
    let final_cpu = handles.cpu_sample();
    let window_settlement = match (&config.receiver_state, &receiver_before) {
        (Some(url), Some(before)) => {
            let expected = completed.saturating_sub(warm_completed);
            Some(settle_receiver(url, before, expected, SETTLE_TIMEOUT).await?)
        }
        _ => None,
    };
    let receiver_after = match &config.receiver_state {
        Some(url) => Some(fetch_peer_state(url).await?),
        None => None,
    };
    let tx_after = tx_netdev.as_deref().and_then(read_tx_counters);
    handles.stop.store(true, Ordering::Relaxed);
    resume_sender(&handles).await?;
    let report = sender
        .join()
        .map_err(|_| "substrate sender thread panicked".to_string())?;
    let outcome = report.outcome;

    let measured = completed.saturating_sub(warm_completed);
    let cpu_delta = |pick: fn(&ThreadCpu) -> f64| (pick(&final_cpu) - pick(&warm_cpu)).max(0.0);
    let cpu_secs = cpu_delta(|cpu| cpu.user_secs + cpu.system_secs);
    let user_secs = cpu_delta(|cpu| cpu.user_secs);
    let system_secs = cpu_delta(|cpu| cpu.system_secs);
    let switches = json!({
        "voluntary": final_cpu
            .voluntary_switches
            .saturating_sub(warm_cpu.voluntary_switches),
        "involuntary": final_cpu
            .involuntary_switches
            .saturating_sub(warm_cpu.involuntary_switches),
    });
    let payload_gbit = measured as f64 * config.payload_bytes as f64 * 8.0 / 1e9;
    let pps = measured as f64 / window_secs.max(1e-9);
    let pps_per_core = Some(measured as f64 / cpu_secs.max(1e-9));

    // Peer counters, judged strictly: an unattributable run is never reported
    // as lossless. Saturating arithmetic is deliberately avoided — a reset
    // counter must not read as zero loss.
    let receiver = receiver_before
        .as_ref()
        .zip(receiver_after.as_ref())
        .map(|(before, after)| {
            let delta = |field: &str| -> Option<i64> {
                let before = counter(before, field)? as i64;
                let after = counter(after, field)? as i64;
                Some(after - before)
            };
            let datagrams = delta("datagrams");
            let drops = delta("udpRcvbufErrors");
            let in_errors = delta("udpInErrors");
            let nic_rx = delta("nicRxDropped");
            let receive_errors = delta("receiveErrors");
            let reset = [datagrams, drops, in_errors, nic_rx, receive_errors]
                .iter()
                .any(|value| value.is_some_and(|value| value < 0));
            let run_id_changed = before.get("runId") != after.get("runId")
                || receiver_before_all
                    .as_ref()
                    .and_then(|state| state.get("runId"))
                    != after.get("runId");
            let loss_ratio = datagrams.map(|received| {
                if measured == 0 {
                    0.0
                } else {
                    1.0 - received as f64 / measured as f64
                }
            });
            let softnet = before
                .get("softnet")
                .zip(after.get("softnet"))
                .map(|(before, after)| {
                    let delta = |field: &str| -> Option<i64> {
                        Some(
                            after.get(field)?.as_u64()? as i64
                                - before.get(field)?.as_u64()? as i64,
                        )
                    };
                    json!({
                        "processed": delta("processed"),
                        "dropped": delta("dropped"),
                        "timeSqueeze": delta("timeSqueeze"),
                        "receivedRps": delta("receivedRps"),
                        "flowLimit": delta("flowLimit"),
                    })
                });
            json!({
                "runId": after.get("runId"),
                "softnet": softnet,
                "runIdChanged": run_id_changed,
                "cpusAllowedList": after.get("cpusAllowedList"),
                "datagrams": datagrams,
                "bytes": delta("bytes"),
                "lossRatio": loss_ratio,
                "udpRcvbufErrors": drops,
                "udpInErrors": in_errors,
                "nicRxDropped": nic_rx,
                "receiveErrors": receive_errors,
                "counterReset": reset,
            })
        });

    // Attribution requires the peer's own evidence to be intact: same run,
    // observable monotonic counters, no application receive errors, no kernel
    // or NIC drops, and delivery inside the tolerance.
    let receiver_attributable = receiver.as_ref().map(|receiver| {
        let observable = [
            "datagrams",
            "bytes",
            "udpRcvbufErrors",
            "udpInErrors",
            "nicRxDropped",
            "receiveErrors",
        ]
        .iter()
        .all(|field| receiver[*field].is_u64());
        observable
            && receiver["counterReset"] == false
            && receiver["runIdChanged"] == false
            && receiver["udpRcvbufErrors"] == 0
            && receiver["udpInErrors"] == 0
            && receiver["nicRxDropped"] == 0
            && receiver["receiveErrors"] == 0
            && receiver["softnet"]["dropped"].as_i64() == Some(0)
            && receiver["softnet"]["flowLimit"].as_i64() == Some(0)
            && matches!(window_settlement, Some(Settlement::Settled { .. }))
    });
    // WI3.6 strictness: with a quiescent boundary the sender's completed
    // datagrams must equal the receiver's, exactly, and every drop counter must
    // be zero. `SUBSTRATE_DELIVERY_TOLERANCE` exists only to reproduce the
    // historical WI3.5 behaviour; the ladder runs at 0.
    let tolerance = std::env::var("SUBSTRATE_DELIVERY_TOLERANCE")
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .unwrap_or(0.0)
        .max(0.0);
    let receiver_kept_up = receiver.as_ref().map(|receiver| {
        let loss_ok = receiver["lossRatio"]
            .as_f64()
            .is_some_and(|ratio| ratio <= tolerance);
        let exact_ok = receiver["datagrams"]
            .as_u64()
            .is_some_and(|received| received == measured);
        loss_ok && (tolerance > 0.0 || exact_ok)
    });

    // Transmit accounting: with no receiver, a TX-only lane is only usable when
    // the sender's completions reconcile with the netdev's own counters.
    let tx = tx_before.zip(tx_after).map(|(before, after)| {
        let packets = after.packets.saturating_sub(before.packets);
        let bytes = after.bytes.saturating_sub(before.bytes);
        let expected_bytes_min = measured * config.payload_bytes as u64;
        let expected_bytes_max = measured * (config.payload_bytes as u64 + 64);
        // A completion can be observed just before the device's own counter for
        // it, so reconciliation allows the in-flight window as slack — never
        // more, and never a shortfall the other way.
        let slack = config.queue_depth as u64;
        json!({
            "dev": tx_netdev,
            "packets": packets,
            "bytes": bytes,
            "packetsPerCompletion": if measured == 0 {
                Value::Null
            } else {
                json!(packets as f64 / measured as f64)
            },
            "packetsShortOfCompletions": measured.saturating_sub(packets),
            "slackAllowed": slack,
            "reconciled": packets <= measured
                && measured - packets <= slack
                && bytes >= expected_bytes_min
                && bytes <= expected_bytes_max,
        })
    });
    let tx_reconciled = tx
        .as_ref()
        .and_then(|tx| tx["reconciled"].as_bool())
        .unwrap_or(false);

    let sender_mask = report.observed_mask;
    // Effective values the program actually used, alongside the environment
    // provenance: an unset tolerance means 0, not "unknown".
    let observed_lane = json!({
        "deliveryToleranceEffective": tolerance,
        "senderCpusObserved": sender_mask,
        "harnessCpusObserved": cpus_allowed_list(),
        "receiverCpusObserved": receiver
            .as_ref()
            .and_then(|receiver| receiver["cpusAllowedList"].clone().into()),
        "rpsCpusObserved": std::env::var("WI3_RPS_CPUS_OBSERVED").ok(),
        "settlementWarmup": settlement_json(&warm_settlement),
    });
    let verdict = match (&outcome, &receiver, tx_netdev.as_deref()) {
        (Err(_), _, _) => "failed",
        (Ok(()), None, Some(_)) if tx_reconciled => "healthy",
        (Ok(()), None, Some(_)) => "tx-unreconciled",
        (Ok(()), None, None) => "unclassified",
        (Ok(()), Some(_), _) if receiver_attributable != Some(true) => "unclassified",
        (Ok(()), Some(_), _) if receiver_kept_up != Some(true) => "receiver-limited",
        (Ok(()), Some(_), _) => "healthy",
    };

    let result = json!({
        "mode": "substrate-pps",
        "runId": config.run,
        "variant": config.variant.as_str(),
        "config": {
            "payloadBytes": config.payload_bytes,
            "destinations": config.destinations.len(),
            "destinationBase": config.destinations.first().map(|d| d.to_string()),
            "queueDepth": config.queue_depth,
            "reapMode": config.reap_mode.as_str(),
            "warmupSecs": config.warmup.as_secs(),
            "durationSecs": config.duration.as_secs(),
            "deliveryTolerance": tolerance,
            "senderCpusRequested": config.sender_cpus,
            "harnessCpusRequested": config.harness_cpus,
            "txNetdev": tx_netdev,
        },
        "lane": lane_configuration(),
        "placement": {
            "senderTid": sender_tid,
            "senderCpusAllowedList": sender_mask,
            "harnessCpusAllowedList": cpus_allowed_list(),
            "receiverCpusAllowedList": receiver.as_ref().and_then(|r| r["cpusAllowedList"].clone().into()),
            "senderOwnsOneCpu": sender_mask.as_deref().map(|mask| !mask.contains(',') && !mask.contains('-')),
        },
        "sends": {
            "submitted": handles.counters.submitted.load(Ordering::Relaxed),
            "completed": completed,
            "completedInWindow": measured,
            "errors": handles.counters.errors.load(Ordering::Relaxed),
            "batches": handles.counters.batches.load(Ordering::Relaxed),
            "maxInFlight": handles.counters.max_in_flight.load(Ordering::Relaxed),
            "ringEnters": match config.variant {
                // Compio owns its ring internally; its submit count is not
                // observable from the application side.
                Variant::Compio | Variant::CompioPipeline => Value::Null,
                Variant::IoUring | Variant::Sendto => {
                    json!(handles.counters.ring_enters.load(Ordering::Relaxed))
                }
            },
            "sqesPerBatch": (handles.counters.submitted.load(Ordering::Relaxed) as f64)
                / (handles.counters.batches.load(Ordering::Relaxed).max(1) as f64),
        },
        "window": {
            "secs": window_secs,
            "senderCpuSecs": cpu_secs,
            "senderUserSecs": user_secs,
            "senderSystemSecs": system_secs,
            "contextSwitches": switches,
            "pps": pps,
            "ppsPerCore": pps_per_core,
            "payloadGbitPerSec": payload_gbit / window_secs.max(1e-9),
            "payloadGbitPerCpuSec": payload_gbit / cpu_secs.max(1e-9),
        },
        "laneObserved": observed_lane,
        "receiver": receiver,
        "settlement": json!({
            "warmup": settlement_json(&warm_settlement),
            "window": settlement_json(&window_settlement),
        }),
        "receiverAttributable": receiver_attributable,
        "receiverKeptUp": receiver_kept_up,
        "tx": tx,
        "verdict": verdict,
        "senderOutcome": match outcome {
            Ok(()) => Value::Null,
            Err(error) => json!(error),
        },
    });
    println!(
        "[substrate-pps] {}",
        json!({
            "variant": result["variant"],
            "verdict": result["verdict"],
            "ppsPerCore": result["window"]["ppsPerCore"],
            "payloadGbitPerCpuSec": result["window"]["payloadGbitPerCpuSec"],
        })
    );
    Ok(result)
}
