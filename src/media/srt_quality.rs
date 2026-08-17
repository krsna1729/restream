use std::time::Instant;

use crate::media::snapshots::PublisherQuality;

use super::{SrtSenderStats, SrtTraceBStats};

#[derive(Debug, Clone, Copy)]
pub(super) struct SrtCounterSnapshot {
    pub(super) packets_received_loss: u64,
    pub(super) packets_received_drop: u64,
    pub(super) packets_received_retrans: u64,
    pub(super) packets_received_undecrypt: u64,
    pub(super) sampled_at: Instant,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SrtSenderCounterSnapshot {
    pub(crate) packets_sent_loss: u64,
    pub(crate) packets_sent_drop: u64,
    pub(crate) packets_sent_retrans: u64,
    pub(crate) sampled_at: Instant,
}

fn counter_rate(current: u64, previous: u64, elapsed_seconds: f64) -> Option<f64> {
    if elapsed_seconds <= 0.0 {
        return None;
    }
    current
        .checked_sub(previous)
        .map(|delta| (delta as f64 / elapsed_seconds * 10.0).round() / 10.0)
}

pub(super) fn quality_from_stats(
    stats: &SrtTraceBStats,
    previous: Option<SrtCounterSnapshot>,
    sampled_at: Instant,
) -> (PublisherQuality, SrtCounterSnapshot) {
    let current = SrtCounterSnapshot {
        packets_received_loss: stats.pkt_rcv_loss_total.max(0) as u64,
        packets_received_drop: stats.pkt_rcv_drop_total.max(0) as u64,
        packets_received_retrans: stats.pkt_rcv_retrans.max(0) as u64,
        packets_received_undecrypt: stats.pkt_rcv_undecrypt_total.max(0) as u64,
        sampled_at,
    };
    let elapsed =
        previous.map(|snapshot| sampled_at.duration_since(snapshot.sampled_at).as_secs_f64());

    (
        PublisherQuality {
            ms_rtt: Some(stats.ms_rtt),
            mbps_receive_rate: Some(stats.mbps_recv_rate),
            mbps_link_capacity: Some(stats.mbps_bandwidth),
            ms_receive_tsb_pd_delay: Some(stats.ms_rcv_tsb_pd_delay.max(0) as f64),
            ms_receive_buf: Some(stats.ms_rcv_buf.max(0) as f64),
            packets_sent_nak: Some(stats.pkt_sent_nak_total.max(0) as u64),
            packets_received_loss: Some(current.packets_received_loss),
            packets_received_drop: Some(current.packets_received_drop),
            packets_received_retrans: Some(current.packets_received_retrans),
            packets_received_undecrypt: Some(current.packets_received_undecrypt),
            packets_received_loss_per_sec: previous.zip(elapsed).and_then(|(snapshot, seconds)| {
                counter_rate(
                    current.packets_received_loss,
                    snapshot.packets_received_loss,
                    seconds,
                )
            }),
            packets_received_drop_per_sec: previous.zip(elapsed).and_then(|(snapshot, seconds)| {
                counter_rate(
                    current.packets_received_drop,
                    snapshot.packets_received_drop,
                    seconds,
                )
            }),
            packets_received_retrans_per_sec: previous.zip(elapsed).and_then(
                |(snapshot, seconds)| {
                    counter_rate(
                        current.packets_received_retrans,
                        snapshot.packets_received_retrans,
                        seconds,
                    )
                },
            ),
            packets_received_undecrypt_per_sec: previous.zip(elapsed).and_then(
                |(snapshot, seconds)| {
                    counter_rate(
                        current.packets_received_undecrypt,
                        snapshot.packets_received_undecrypt,
                        seconds,
                    )
                },
            ),
            srt_send_buf_bytes: Some(stats.byte_snd_buf),
            srt_recv_buf_bytes: Some(stats.byte_rcv_buf),
            srt_send_buf_avail_bytes: Some(stats.byte_avail_snd_buf),
            srt_recv_buf_avail_bytes: Some(stats.byte_avail_rcv_buf),
            srt_flight_size_pkts: Some(stats.pkt_flight_size),
            ..PublisherQuality::default()
        },
        current,
    )
}

pub(crate) fn sender_quality_from_stats(
    stats: &SrtSenderStats,
    previous: Option<SrtSenderCounterSnapshot>,
    sampled_at: Instant,
) -> (PublisherQuality, SrtSenderCounterSnapshot) {
    let current = SrtSenderCounterSnapshot {
        packets_sent_loss: stats.packets_sent_loss_total,
        packets_sent_drop: stats.packets_sent_drop_total,
        packets_sent_retrans: stats.packets_retransmit_total,
        sampled_at,
    };
    let elapsed =
        previous.map(|snapshot| sampled_at.duration_since(snapshot.sampled_at).as_secs_f64());

    (
        PublisherQuality {
            ms_rtt: Some(stats.rtt_ms),
            mbps_send_rate: Some(stats.send_rate_mbps),
            mbps_link_capacity: Some(stats.bandwidth_mbps),
            ms_send_tsb_pd_delay: Some(stats.send_tsbpd_delay_ms),
            ms_send_buf: Some(stats.send_buf_ms),
            packets_sent_loss: Some(current.packets_sent_loss),
            packets_sent_drop: Some(current.packets_sent_drop),
            packets_sent_retrans: Some(current.packets_sent_retrans),
            packets_received_nak: Some(stats.packets_received_nak_total),
            packets_sent_loss_per_sec: previous.zip(elapsed).and_then(|(snapshot, seconds)| {
                counter_rate(
                    current.packets_sent_loss,
                    snapshot.packets_sent_loss,
                    seconds,
                )
            }),
            packets_sent_drop_per_sec: previous.zip(elapsed).and_then(|(snapshot, seconds)| {
                counter_rate(
                    current.packets_sent_drop,
                    snapshot.packets_sent_drop,
                    seconds,
                )
            }),
            packets_sent_retrans_per_sec: previous.zip(elapsed).and_then(|(snapshot, seconds)| {
                counter_rate(
                    current.packets_sent_retrans,
                    snapshot.packets_sent_retrans,
                    seconds,
                )
            }),
            srt_send_buf_bytes: Some(stats.send_buf_bytes),
            srt_send_buf_avail_bytes: Some(stats.send_buf_available_bytes),
            srt_flight_size_pkts: Some(stats.flight_size_packets),
            srt_flow_window_pkts: Some(stats.flow_window_packets),
            srt_congestion_window_pkts: Some(stats.congestion_window_packets),
            ..PublisherQuality::default()
        },
        current,
    )
}
