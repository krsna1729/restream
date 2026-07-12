//! Runtime snapshot and observed-media contract types.
//!
//! These are the small data carriers that describe live ingest/egress quality,
//! stream metadata, dependency state, and listener counters. `MediaEngine`
//! populates them, while API, diagnostics, alerts, and harness code consume
//! them through stable runtime-facing shapes.

use std::sync::atomic::{AtomicBool, AtomicU64};

/// Per-pipeline ingest quality snapshot (RTMP TCP or SRT link stats).
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublisherQuality {
    // TCP metrics (RTMP)
    pub tcp_congestion_algorithm: Option<String>,
    pub tcp_rtt_ms: Option<f64>,
    pub tcp_rtt_var_ms: Option<f64>,
    pub tcp_bytes_received: Option<u64>,
    pub tcp_bytes_sent: Option<u64>,
    pub tcp_bytes_acked: Option<u64>,
    pub tcp_bytes_retrans: Option<u64>,
    pub tcp_last_rcv_ms: Option<u64>,
    pub tcp_last_snd_ms: Option<u64>,
    pub tcp_rcv_rtt_ms: Option<f64>,
    pub tcp_rcv_space: Option<u64>,
    pub tcp_rcv_ooopack: Option<u64>,
    pub tcp_snd_mss: Option<u64>,
    pub tcp_pmtu: Option<u64>,
    pub tcp_unacked: Option<u64>,
    pub tcp_sacked: Option<u64>,
    pub tcp_lost: Option<u64>,
    pub tcp_retrans: Option<u64>,
    pub tcp_snd_cwnd: Option<u64>,
    pub tcp_snd_ssthresh: Option<u64>,
    pub tcp_advmss: Option<u64>,
    pub tcp_reordering: Option<u64>,
    pub tcp_notsent_bytes: Option<u64>,
    pub tcp_total_retrans: Option<u64>,
    pub tcp_pacing_rate_bps: Option<u64>,
    pub tcp_max_pacing_rate_bps: Option<u64>,
    pub tcp_delivery_rate_bps: Option<u64>,
    pub tcp_segs_out: Option<u64>,
    pub tcp_data_segs_out: Option<u64>,
    pub tcp_delivered: Option<u64>,
    pub tcp_delivered_ce: Option<u64>,
    pub tcp_busy_time_ms: Option<u64>,
    pub tcp_rwnd_limited_ms: Option<u64>,
    pub tcp_sndbuf_limited_ms: Option<u64>,
    pub tcp_dsack_dups: Option<u64>,
    pub tcp_reord_seen: Option<u64>,
    pub tcp_snd_wnd: Option<u64>,
    pub tcp_total_rto: Option<u64>,
    pub tcp_total_rto_recoveries: Option<u64>,
    pub tcp_total_rto_time_ms: Option<u64>,
    pub tcp_skmem_rmem_alloc: Option<u64>,
    pub tcp_skmem_rmem_max: Option<u64>,
    pub tcp_skmem_wmem_alloc: Option<u64>,
    pub tcp_skmem_wmem_max: Option<u64>,
    pub tcp_receive_rate_mbps: Option<f64>,
    pub tcp_send_rate_mbps: Option<f64>,
    pub tcp_stats_unavailable_reason: Option<String>,
    // SRT metrics
    pub ms_rtt: Option<f64>,
    pub mbps_send_rate: Option<f64>,
    pub mbps_receive_rate: Option<f64>,
    pub mbps_link_capacity: Option<f64>,
    pub ms_send_tsb_pd_delay: Option<f64>,
    pub ms_receive_tsb_pd_delay: Option<f64>,
    pub ms_send_buf: Option<f64>,
    pub ms_receive_buf: Option<f64>,
    pub packets_sent_loss: Option<u64>,
    pub packets_sent_drop: Option<u64>,
    pub packets_sent_retrans: Option<u64>,
    pub packets_sent_nak: Option<u64>,
    pub packets_received_nak: Option<u64>,
    pub packets_received_loss: Option<u64>,
    pub packets_received_drop: Option<u64>,
    pub packets_received_retrans: Option<u64>,
    pub packets_received_undecrypt: Option<u64>,
    pub packets_sent_loss_per_sec: Option<f64>,
    pub packets_sent_drop_per_sec: Option<f64>,
    pub packets_sent_retrans_per_sec: Option<f64>,
    pub packets_received_loss_per_sec: Option<f64>,
    pub packets_received_drop_per_sec: Option<f64>,
    pub packets_received_retrans_per_sec: Option<f64>,
    pub packets_received_undecrypt_per_sec: Option<f64>,
    // SRT buffer occupancy
    pub srt_send_buf_bytes: Option<i32>,
    pub srt_recv_buf_bytes: Option<i32>,
    pub srt_send_buf_avail_bytes: Option<i32>,
    pub srt_recv_buf_avail_bytes: Option<i32>,
    pub srt_flight_size_pkts: Option<i32>,
    pub srt_flow_window_pkts: Option<i32>,
    pub srt_congestion_window_pkts: Option<i32>,
    pub srt_bonded: Option<bool>,
    pub srt_group_member_count: Option<u32>,
    pub srt_group_connected_members: Option<u32>,
    pub srt_group_active_members: Option<u32>,
    pub srt_group_broken_members: Option<u32>,
    pub inbound_rtp_packets_lost: Option<u64>,
    pub inbound_rtp_packets_in_error: Option<u64>,
    pub inbound_rtp_packets_jitter: Option<f64>,
}

/// Video stream metadata collected from the demuxer.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoMeta {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub bw: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixel_format: Option<String>,
}

/// Audio stream metadata collected from the demuxer.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioMeta {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_layout: Option<String>,
    pub track_index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IngestDiagSnapshot {
    pub protocol: String,
    pub uptime_secs: f64,
    pub bytes_received: u64,
    pub remote_addr: Option<String>,
    pub video: Option<VideoMeta>,
    pub audio: Option<AudioMeta>,
    pub quality: PublisherQuality,
    pub keyframe_times: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct EgressDiagSnapshot {
    pub output_id: String,
    pub pipeline_id: String,
    pub protocol: String,
    pub status: String,
    pub phase: String,
    pub target_addr: Option<String>,
    pub bytes_sent: u64,
    pub last_progress_ms: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RingBufferDiagSnapshot {
    pub fill_slots: usize,
    pub capacity_slots: usize,
    pub readers: Vec<crate::media::ring_buffer::ReaderSnapshot>,
}

#[derive(Debug, Clone)]
pub struct SrtListenerDiagSnapshot {
    pub bonding_available: bool,
    pub rx_queue_bytes: u64,
    pub rx_queue_peak_bytes: u64,
    pub drops: u64,
    pub active_ingest_count: usize,
}

#[derive(Debug, Clone)]
pub struct HlsDependencySnapshot {
    pub store_exists: bool,
    pub active: bool,
    pub persistent_consumers: u64,
    pub last_access_age_ms: Option<u64>,
    pub segments: usize,
    pub playlist_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct FileIngestDependencySnapshot {
    pub marked_active: bool,
    pub child_registered: bool,
}

/// Shared SRT listener socket state, updated by the SRT monitor task.
#[derive(Debug, Default)]
pub struct ListenerSocketStats {
    pub bonding_available: AtomicBool,
    pub rx_queue_bytes: AtomicU64,
    pub rx_queue_max_bytes: AtomicU64,
    pub drops: AtomicU64,
}

/// Shared RTMP listener accept/error counters.
#[derive(Debug, Default)]
pub struct RtmpListenerStats {
    pub rtmp_accept_errors: AtomicU64,
    pub rtmp_fd_exhaustion_errors: AtomicU64,
}
