//! Product-level SRT sender signals derived from public `srt-transport`
//! logical-caller statistics.

use srt_transport::advanced::caller::LogicalCallerStats;

/// Sender-side backlog of one logical caller: bytes/packets still held in the
/// protocol sender buffer (unacknowledged or not yet sent) and the time span
/// they cover. For a bonded caller: bytes/packets summed over legs, the widest
/// leg span.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SrtSendBacklog {
    pub bytes: u64,
    pub packets: u32,
    pub ms: u32,
}

pub fn send_backlog(stats: &LogicalCallerStats) -> Option<SrtSendBacklog> {
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
