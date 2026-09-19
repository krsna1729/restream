//! Product-level SRT sender signals derived from public `srt-transport`
//! logical-caller statistics. Nothing here reads private table state.

use srt_transport::advanced::caller::LogicalCallerStats;

use crate::media::snapshots::PublisherQuality;

/// Sender-side backlog of one logical caller: bytes/packets still held in the
/// protocol sender buffer (unacknowledged or not yet sent) and the time span
/// they cover. For a bonded caller: bytes/packets summed over legs, the widest
/// leg span.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SrtSendBacklog {
    pub bytes: u64,
    pub packets: u32,
    pub ms: u32,
}

pub(crate) fn send_backlog(stats: &LogicalCallerStats) -> Option<SrtSendBacklog> {
    match stats {
        LogicalCallerStats::Direct(stats) => stats.sender.map(|sender| SrtSendBacklog {
            bytes: sender.payload_bytes_in_buffer,
            packets: sender.packets_in_buffer,
            ms: u32::try_from(sender.buffer_span_micros / 1_000).unwrap_or(u32::MAX),
        }),
        LogicalCallerStats::Group(stats) => {
            let mut backlog = SrtSendBacklog::default();
            let mut span_micros = 0_u64;
            for leg in &stats.legs {
                if let Some(sender) = leg.connection.sender {
                    backlog.bytes = backlog.bytes.saturating_add(sender.payload_bytes_in_buffer);
                    backlog.packets = backlog.packets.saturating_add(sender.packets_in_buffer);
                    span_micros = span_micros.max(sender.buffer_span_micros);
                }
            }
            backlog.ms = u32::try_from(span_micros / 1_000).unwrap_or(u32::MAX);
            Some(backlog)
        }
    }
}

/// The cross-protocol quality snapshot the status layer publishes
/// (`rtmp/ingest.rs` builds the same type from its own counters).
pub(crate) fn sender_quality(stats: &LogicalCallerStats) -> Option<PublisherQuality> {
    match stats {
        LogicalCallerStats::Direct(stats) => {
            let sender = stats.sender?;
            Some(PublisherQuality {
                ms_rtt: Some(sender.peer_rtt_micros.map_or(0.0, f64::from) / 1_000.0),
                mbps_send_rate: Some(
                    sender
                        .peer_receiving_rate_bytes_per_second
                        .map_or(0.0, f64::from)
                        / 1_000_000.0,
                ),
                packets_sent_loss: Some(sender.total_lost),
                packets_sent_drop: Some(sender.total_dropped),
                ..PublisherQuality::default()
            })
        }
        LogicalCallerStats::Group(stats) => {
            // RTT averaged over legs reporting one, send rate summed over
            // legs, loss from the group's aggregate. Groups report no
            // aggregate TLPKTDROP counter, so drops read as zero.
            let mut rtt_total = 0_f64;
            let mut rtt_count = 0_u64;
            let mut rate = 0_f64;
            for leg in &stats.legs {
                let Some(sender) = leg.connection.sender.as_ref() else {
                    continue;
                };
                if let Some(rtt) = sender.peer_rtt_micros {
                    rtt_total += f64::from(rtt);
                    rtt_count += 1;
                }
                rate += sender
                    .peer_receiving_rate_bytes_per_second
                    .map_or(0.0, f64::from);
            }
            Some(PublisherQuality {
                ms_rtt: Some(if rtt_count == 0 {
                    0.0
                } else {
                    rtt_total / rtt_count as f64 / 1_000.0
                }),
                mbps_send_rate: Some(rate / 1_000_000.0),
                packets_sent_loss: Some(stats.aggregate.wire_sender_packets_lost),
                packets_sent_drop: Some(0),
                ..PublisherQuality::default()
            })
        }
    }
}
