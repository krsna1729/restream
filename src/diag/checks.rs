use std::sync::Arc;
use std::time::Instant;

use crate::media::engine::MediaEngine;

use super::model::DiagResult;

mod file;
mod system;

pub(super) use file::{
    check_file_ingest_runtime, check_file_source, check_preview_recording_state,
};
pub(super) use system::{
    check_network_bandwidth, check_srt_listener_socket, check_system_resources,
};

const DIAG_OUTPUT_PROGRESS_STALE_MS: i64 = 10_000;
const DIAG_SRT_BUFFER_WARNING_PCT: f64 = 80.0;
const DIAG_SRT_BUFFER_CRITICAL_PCT: f64 = 95.0;

pub(super) async fn check_engine_status(
    idx: u32,
    engine: &Arc<MediaEngine>,
    pipeline_id: &str,
) -> DiagResult {
    let start = Instant::now();
    let active_ingest = engine.active_ingest_diag_snapshot(pipeline_id).await;
    let pipeline_rb = engine.pipeline_ring_diag_snapshot(pipeline_id).await;
    let active_output_count = engine.active_egress_diag_snapshots(pipeline_id).await.len();
    let active_ingest_count = engine.active_ingest_count().await;
    let active_egress_count = engine.active_egress_count().await;

    let mut issues = vec![];
    let mut lines = vec![];

    lines.push(format!("Pipeline ID: {}", pipeline_id));
    lines.push(format!(
        "Active ingests (all pipelines): {}",
        active_ingest_count
    ));
    lines.push(format!(
        "Active egresses (all pipelines): {}",
        active_egress_count
    ));

    if let Some(ingest) = active_ingest {
        lines.push(format!("Ingest protocol: {}", ingest.protocol));
        lines.push(format!("Ingest uptime: {:.1}s", ingest.uptime_secs));
        lines.push(format!("Bytes received: {}", ingest.bytes_received));
        if let Some(addr) = &ingest.remote_addr {
            lines.push(format!("Publisher remote: {}", addr));
        }
    } else {
        lines.push("No active ingest for this pipeline.".to_string());
        issues.push("No active publisher is connected to this pipeline.".to_string());
    }

    if let Some(rb) = pipeline_rb {
        let fill = rb.fill_slots;
        let cap = rb.capacity_slots;
        let fill_pct = (fill * 100).checked_div(cap).unwrap_or(0);
        lines.push(format!(
            "Ring buffer: {}/{} slots filled ({}%)",
            fill, cap, fill_pct
        ));
        if fill_pct > 90 {
            issues.push(format!(
                "Ring buffer is {}% full — possible consumer lag or encoder overrun.",
                fill_pct
            ));
        }

        let max_lag = rb
            .readers
            .iter()
            .map(|reader| reader.lag_slots)
            .max()
            .unwrap_or(0);
        let total_overflows: usize = rb.readers.iter().map(|reader| reader.overflow_count).sum();
        let max_packet_age_ms = rb
            .readers
            .iter()
            .filter_map(|reader| reader.packet_age_ms)
            .max();
        lines.push(format!(
            "Ring buffer readers: max lag={}, total overflows={}, max packet age={}ms",
            max_lag,
            total_overflows,
            max_packet_age_ms
                .map(|age| age.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        ));
        if total_overflows > 0 {
            issues.push(format!(
                "Consumers are dropping frames due to overflow ({} total overflows).",
                total_overflows
            ));
        }
    } else {
        lines.push("Ring buffer not yet allocated for this pipeline.".to_string());
    }

    lines.push(format!(
        "Active outputs for this pipeline: {}",
        active_output_count
    ));

    DiagResult::ok(
        idx,
        "Engine Status",
        "MediaEngine active state",
        "api_runtime_views::health_snapshot(...)",
        lines.join("\n"),
        start.elapsed().as_millis() as u64,
    )
    .with_issues(issues)
}

pub(super) async fn check_active_outputs(
    idx: u32,
    engine: &Arc<MediaEngine>,
    pipeline_id: &str,
) -> DiagResult {
    let start = Instant::now();
    let my_egresses = engine.active_egress_diag_snapshots(pipeline_id).await;

    let mut issues = vec![];
    let mut lines = vec![];

    if my_egresses.is_empty() {
        lines.push("No active outputs for this pipeline.".to_string());
    } else {
        for egress in &my_egresses {
            let output_id = egress.output_id.as_str();
            let target_addr = egress
                .target_addr
                .clone()
                .unwrap_or_else(|| "unresolved".to_string());
            let last_progress_ms = egress.last_progress_ms;
            let progress_age_ms = if last_progress_ms > 0 {
                let age = chrono::Utc::now()
                    .timestamp_millis()
                    .max(0)
                    .saturating_sub(last_progress_ms as i64);
                Some(age)
            } else {
                None
            };
            let last_progress = if let Some(age) = progress_age_ms {
                format!("{}ms ago", age)
            } else {
                "never".to_string()
            };
            let last_error = egress.last_error.clone();
            lines.push(format!("Output {}", output_id));
            lines.push(format!("  protocol: {}", egress.protocol));
            lines.push(format!("  status: {} / {}", egress.status, egress.phase));
            lines.push(format!("  target: {}", target_addr));
            lines.push(format!("  target_addr: {}", target_addr));
            lines.push(format!("  bytes_sent: {}", egress.bytes_sent));
            lines.push(format!("  last_progress: {}", last_progress));
            if let Some(error) = last_error {
                lines.push(format!("  last_error: {}", error));
            }
            if egress.status == "failed" || egress.phase == "failed" {
                issues.push(format!(
                    "Output {} has failed in phase {}.",
                    output_id, egress.phase
                ));
            }
            if egress.status == "stalled"
                || progress_age_ms.is_some_and(|age| age >= DIAG_OUTPUT_PROGRESS_STALE_MS)
            {
                let detail = progress_age_ms
                    .map(|age| format!("last progress was {age}ms ago"))
                    .unwrap_or_else(|| "no progress has been recorded".to_string());
                issues.push(format!(
                    "Output {output_id} is not making progress ({detail}) while its runtime phase is {}.",
                    egress.phase
                ));
            } else if last_progress_ms == 0
                && matches!(
                    egress.phase.as_str(),
                    "sending" | "publishing" | "uploading" | "waitingUpstream"
                )
            {
                issues.push(format!(
                    "Output {output_id} has entered {} but has never recorded media progress.",
                    egress.phase
                ));
            }
        }
    }

    DiagResult::ok(
        idx,
        "Active Outputs",
        "Egress target status and throughput",
        "engine.active_egress_diag_snapshots()",
        lines.join("\n"),
        start.elapsed().as_millis() as u64,
    )
    .with_issues(issues)
}

pub(super) async fn check_ingest_stream_info(
    idx: u32,
    engine: &Arc<MediaEngine>,
    pipeline_id: &str,
) -> DiagResult {
    let start = Instant::now();
    let ingest_opt = engine.active_ingest_diag_snapshot(pipeline_id).await;

    let mut issues = vec![];
    let mut lines = vec![];

    if let Some(ingest) = ingest_opt {
        if let Some(video) = &ingest.video {
            lines.push(format!("Video codec: {}", video.codec));
            lines.push(format!("Resolution: {}x{}", video.width, video.height));
            lines.push(format!("Frame rate: {:.2} fps", video.fps));
            if let Some(bw) = video.bw {
                lines.push(format!("Video bitrate: {:.1} Kbps", bw));
            }
            if video.width == 0 || video.height == 0 {
                issues
                    .push("Video resolution is 0x0 — stream metadata not yet parsed.".to_string());
            }
        } else {
            lines.push("No video stream metadata available yet.".to_string());
            issues.push(
                "No video stream detected. The publisher may not have sent media yet.".to_string(),
            );
        }
        if let Some(audio) = &ingest.audio {
            lines.push(format!("Audio codec: {}", audio.codec));
            lines.push(format!("Sample rate: {} Hz", audio.sample_rate));
            lines.push(format!("Channels: {}", audio.channels));
        } else {
            lines.push("No audio stream metadata available yet.".to_string());
        }
    } else {
        lines.push("No active ingest — cannot inspect stream info.".to_string());
        issues.push("Pipeline is not actively receiving data.".to_string());
    }

    DiagResult::ok(
        idx,
        "Stream Info",
        "Video and audio codec parameters",
        "engine.ingests.active.video/audio",
        lines.join("\n"),
        start.elapsed().as_millis() as u64,
    )
    .with_issues(issues)
}

pub(super) async fn check_publisher_transport(
    idx: u32,
    engine: &Arc<MediaEngine>,
    pipeline_id: &str,
    probe_protocol: &str,
) -> DiagResult {
    let start = Instant::now();
    let ingest_opt = engine.active_ingest_diag_snapshot(pipeline_id).await;

    let mut issues = vec![];
    let mut lines = vec![];

    if let Some(ingest) = ingest_opt {
        let q = &ingest.quality;
        if probe_protocol == "srt" {
            lines.push("Protocol: SRT".to_string());
            if q.srt_bonded == Some(true) {
                let members = q.srt_group_member_count.unwrap_or(0);
                let connected = q.srt_group_connected_members.unwrap_or(0);
                let active = q.srt_group_active_members.unwrap_or(0);
                let broken = q.srt_group_broken_members.unwrap_or(0);
                lines.push(format!(
                    "Bonded group: {} members, {} connected, {} active, {} broken",
                    members, connected, active, broken
                ));
                if active == 0 {
                    issues.push("SRT bond has no active member links.".to_string());
                }
                if broken > 0 {
                    issues.push(format!("SRT bond has {} broken member link(s).", broken));
                }
            } else if q.srt_bonded == Some(false) {
                lines.push("Bonded group: no (single SRT link)".to_string());
            }
            if let Some(rtt) = q.ms_rtt {
                lines.push(format!("RTT: {:.1} ms", rtt));
                if rtt > 200.0 {
                    issues.push(format!("High SRT RTT: {:.1}ms (threshold 200ms)", rtt));
                }
            }
            if let Some(recv_rate) = q.mbps_receive_rate {
                lines.push(format!("Receive rate: {:.2} Mbps", recv_rate));
            }
            if let Some(cap) = q.mbps_link_capacity {
                lines.push(format!("Link capacity: {:.2} Mbps", cap));
            }
            let loss_total = q.packets_received_loss.unwrap_or(0);
            match q.packets_received_loss_per_sec {
                Some(rate) => {
                    lines.push(format!(
                        "Packets lost: {:.1}/s ({} total)",
                        rate, loss_total
                    ));
                    if rate >= 5.0 {
                        issues.push(format!(
                            "High SRT packet loss rate: {:.1}/s (threshold 5/s)",
                            rate
                        ));
                    }
                }
                None => lines.push(format!("Packets lost: —/s ({} total)", loss_total)),
            }
            let drop_total = q.packets_received_drop.unwrap_or(0);
            match q.packets_received_drop_per_sec {
                Some(rate) => {
                    lines.push(format!(
                        "Packets dropped: {:.1}/s ({} total)",
                        rate, drop_total
                    ));
                    if rate >= 1.0 {
                        issues.push(format!(
                            "SRT packet drop rate: {:.1}/s (threshold 1/s)",
                            rate
                        ));
                    }
                }
                None => lines.push(format!("Packets dropped: —/s ({} total)", drop_total)),
            }
            let retrans_total = q.packets_received_retrans.unwrap_or(0);
            match q.packets_received_retrans_per_sec {
                Some(rate) => {
                    lines.push(format!(
                        "Packets retransmitted: {:.1}/s ({} total)",
                        rate, retrans_total
                    ));
                    if rate >= 10.0 {
                        issues.push(format!(
                            "High SRT retransmission rate: {:.1}/s (threshold 10/s)",
                            rate
                        ));
                    }
                }
                None => lines.push(format!(
                    "Packets retransmitted: —/s ({} total)",
                    retrans_total
                )),
            }
            let undecrypt_total = q.packets_received_undecrypt.unwrap_or(0);
            match q.packets_received_undecrypt_per_sec {
                Some(rate) => {
                    lines.push(format!(
                        "Packets undecrypted: {:.1}/s ({} total)",
                        rate, undecrypt_total
                    ));
                    if rate > 0.0 {
                        issues.push(format!(
                            "SRT undecrypted packet rate: {:.1}/s (expected 0/s)",
                            rate
                        ));
                    }
                }
                None => lines.push(format!(
                    "Packets undecrypted: —/s ({} total)",
                    undecrypt_total
                )),
            }
            if let Some(latency) = q.ms_receive_tsb_pd_delay {
                lines.push(format!("Negotiated latency buffer: {:.0}ms", latency));
            }
            if let Some(buf) = q.ms_receive_buf {
                lines.push(format!("Current latency buffer: {:.0}ms", buf));
            }
            if let (Some(snd), Some(snd_avail)) = (q.srt_send_buf_bytes, q.srt_send_buf_avail_bytes)
            {
                let total = snd + snd_avail;
                let pct = if total > 0 {
                    (snd as f64 / total as f64) * 100.0
                } else {
                    0.0
                };
                lines.push(format!(
                    "Send buffer: {}KB / {}KB ({:.0}%)",
                    snd / 1024,
                    total / 1024,
                    pct
                ));
            }
            if let (Some(rcv), Some(rcv_avail)) = (q.srt_recv_buf_bytes, q.srt_recv_buf_avail_bytes)
            {
                let total = rcv + rcv_avail;
                let pct = if total > 0 {
                    (rcv as f64 / total as f64) * 100.0
                } else {
                    0.0
                };
                lines.push(format!(
                    "Recv buffer: {}KB / {}KB ({:.0}%)",
                    rcv / 1024,
                    total / 1024,
                    pct
                ));
                if pct >= DIAG_SRT_BUFFER_CRITICAL_PCT {
                    issues.push(format!(
                        "SRT application receive buffer is {:.0}% full ({}KB / {}KB). Restream is not draining publisher data; downstream outputs will starve even if the kernel UDP queue looks empty.",
                        pct,
                        rcv / 1024,
                        total / 1024
                    ));
                } else if pct >= DIAG_SRT_BUFFER_WARNING_PCT {
                    issues.push(format!(
                        "SRT application receive buffer is {:.0}% full ({}KB / {}KB). Ingest is close to stalling.",
                        pct,
                        rcv / 1024,
                        total / 1024
                    ));
                }
            }
            if let Some(flight) = q.srt_flight_size_pkts {
                lines.push(format!("Packets in flight: {}", flight));
            }
            if lines.len() == 1 {
                lines.push("No SRT transport stats available yet.".to_string());
                issues.push("SRT quality metrics not yet populated. Stats update after first packets arrive.".to_string());
            }
        } else {
            // RTMP/TCP
            lines.push("Protocol: RTMP (TCP)".to_string());
            if let Some(reason) = &q.tcp_stats_unavailable_reason {
                lines.push(format!("TCP stats unavailable: {}", reason));
                issues.push(format!(
                    "TCP transport stats could not be collected: {}",
                    reason
                ));
            } else {
                if let Some(rtt) = q.tcp_rtt_ms {
                    lines.push(format!("TCP RTT: {:.1} ms", rtt));
                    if rtt >= 200.0 {
                        issues.push(format!("High TCP RTT: {:.1}ms (threshold 200ms)", rtt));
                    }
                }
                if let Some(rate) = q.tcp_receive_rate_mbps {
                    lines.push(format!("TCP receive rate: {:.2} Mbps", rate));
                }
                if let Some(rcv_rtt) = q.tcp_rcv_rtt_ms {
                    lines.push(format!("TCP receive RTT: {:.1} ms", rcv_rtt));
                }
                if let Some(last_rcv) = q.tcp_last_rcv_ms {
                    lines.push(format!("Time since last receive: {} ms", last_rcv));
                    if last_rcv >= 5_000 {
                        issues.push(format!(
                            "RTMP publisher receive stall: {}ms since last packet (threshold 5000ms)",
                            last_rcv
                        ));
                    }
                }
                if let Some(out_of_order) = q.tcp_rcv_ooopack {
                    lines.push(format!("Out-of-order packets (HOL): {}", out_of_order));
                    if out_of_order >= 50 {
                        issues.push(format!(
                            "High TCP out-of-order packet count: {} (threshold 50)",
                            out_of_order
                        ));
                    }
                }
                if let Some(window) = q.tcp_rcv_space {
                    lines.push(format!("TCP receive window: {} bytes", window));
                }
                if let Some(used) = q.tcp_skmem_rmem_alloc {
                    match q.tcp_skmem_rmem_max {
                        Some(max) if max > 0 => {
                            let saturation = used as f64 / max as f64;
                            lines.push(format!(
                                "TCP receive buffer: {} / {} bytes ({:.1}%)",
                                used,
                                max,
                                saturation * 100.0
                            ));
                            if saturation > 0.8 {
                                issues.push(format!(
                                    "TCP receive buffer is {:.1}% full (threshold 80%)",
                                    saturation * 100.0
                                ));
                            }
                        }
                        _ => lines.push(format!("TCP receive buffer used: {} bytes", used)),
                    }
                }
                if lines.len() == 1 {
                    lines.push("No TCP socket stats available yet.".to_string());
                    issues.push("TCP quality metrics not yet populated. Stats update periodically while RTMP is connected.".to_string());
                }
            }
        }
    } else {
        lines.push("No active ingest — cannot inspect publisher transport.".to_string());
        issues.push("Pipeline has no active publisher.".to_string());
    }

    DiagResult::ok(
        idx,
        "Publisher Transport",
        "Network connection quality metrics",
        if probe_protocol == "srt" {
            "libsrt srt_bistats()"
        } else {
            "getsockopt(TCP_INFO/SO_MEMINFO)"
        },
        lines.join("\n"),
        start.elapsed().as_millis() as u64,
    )
    .with_issues(issues)
}

pub(super) async fn check_ring_buffer_health(
    idx: u32,
    engine: &Arc<MediaEngine>,
    pipeline_id: &str,
) -> DiagResult {
    let start = Instant::now();
    let rb_opt = engine.pipeline_ring_diag_snapshot(pipeline_id).await;

    let mut issues = vec![];
    let mut lines = vec![];

    if let Some(rb) = rb_opt {
        let fill = rb.fill_slots;
        let cap = rb.capacity_slots;
        let fill_pct = (fill * 100).checked_div(cap).unwrap_or(0);
        lines.push(format!("Capacity: {} slots", cap));
        lines.push(format!("Filled: {} slots ({}%)", fill, fill_pct));
        lines.push("Compact packet slots: yes".to_string());
        lines.push("Frame size: variable (media packets)".to_string());

        let readers_info = rb.readers;

        if !readers_info.is_empty() {
            lines.push("Active readers:".to_string());
            for reader in &readers_info {
                lines.push(format!(
                    "  - {}: lag={} slots, overflows={}, packet_age={}ms",
                    reader.name,
                    reader.lag_slots,
                    reader.overflow_count,
                    reader
                        .packet_age_ms
                        .map(|age| age.to_string())
                        .unwrap_or_else(|| "n/a".to_string())
                ));
                if reader.lag_slots > (cap * 8 / 10) {
                    issues.push(format!(
                        "Reader {} is severely lagging ({} / {} slots). Possible network congestion or performance bottleneck.",
                        reader.name, reader.lag_slots, cap
                    ));
                }
                if reader.overflow_count > 0 {
                    issues.push(format!(
                        "Reader {} has experienced {} overflow(s). Dropped frames occurred.",
                        reader.name, reader.overflow_count
                    ));
                }
            }
        } else {
            lines.push("Active readers: none".to_string());
        }

        if fill_pct > 85 {
            issues.push(format!(
                "Ring buffer is {}% full. Egress consumers may be lagging behind ingest. \
                 Check output target connectivity and bitrate matching.",
                fill_pct
            ));
        }
        if fill == 0 {
            if readers_info.is_empty() {
                lines.push("Buffer is empty — no active readers are attached.".to_string());
            } else {
                lines.push(
                    "Buffer is empty — active readers are caught up with the producer.".to_string(),
                );
            }
        }
    } else {
        lines.push("Ring buffer not yet allocated for this pipeline.".to_string());
        issues.push(
            "Ring buffer allocation indicates no ingest has started on this pipeline.".to_string(),
        );
    }

    DiagResult::ok(
        idx,
        "Ring Buffer Health",
        "In-process media ring buffer state",
        "RingBuffer::fill_and_capacity()",
        lines.join("\n"),
        start.elapsed().as_millis() as u64,
    )
    .with_issues(issues)
}

pub(super) async fn check_gop_analysis(
    idx: u32,
    engine: &Arc<MediaEngine>,
    pipeline_id: &str,
) -> DiagResult {
    let start = Instant::now();
    let ingest_opt = engine.active_ingest_diag_snapshot(pipeline_id).await;

    let mut issues = vec![];
    let mut lines = vec![];

    if let Some(ingest) = ingest_opt {
        let times = ingest.keyframe_times;
        if times.len() < 2 {
            lines.push(format!("Keyframes observed: {}", times.len()));
            lines.push("Not enough keyframes to analyze GOP intervals yet.".to_string());
        } else {
            let intervals: Vec<f64> = times
                .windows(2)
                .map(|w| ((w[1] - w[0]) as f64 / 1000.0).max(0.0))
                .collect();
            let avg = intervals.iter().sum::<f64>() / intervals.len() as f64;
            let min = intervals.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = intervals.iter().cloned().fold(0.0f64, f64::max);
            let variance =
                intervals.iter().map(|v| (v - avg).powi(2)).sum::<f64>() / intervals.len() as f64;
            let stddev = variance.sqrt();

            lines.push(format!("Keyframes observed: {}", times.len()));
            lines.push(format!("GOP intervals sampled: {}", intervals.len()));
            lines.push(format!("Average GOP interval: {:.2}s", avg));
            lines.push(format!("Min: {:.2}s  Max: {:.2}s", min, max));
            lines.push(format!("Std deviation: {:.3}s", stddev));

            if stddev > 0.5 {
                issues.push(format!(
                    "Unstable keyframe interval (jitter {:.2}s). Average GOP is {:.2}s. \
                     This causes player buffering and adaptive bitrate switching failures.",
                    stddev, avg
                ));
            }
            if avg > 8.0 {
                issues.push(format!(
                    "Keyframe interval is very high ({:.2}s). \
                     High intervals make seeking sluggish and increase stream latency.",
                    avg
                ));
            }
        }
    } else {
        lines.push("No active ingest — cannot analyze GOP.".to_string());
        issues.push("Pipeline is not actively receiving data.".to_string());
    }

    DiagResult::ok(
        idx,
        "GOP Analysis",
        "Keyframe interval consistency",
        "engine.keyframe_times analysis",
        lines.join("\n"),
        start.elapsed().as_millis() as u64,
    )
    .with_issues(issues)
}
