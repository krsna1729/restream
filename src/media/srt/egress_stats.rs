//! Product-level SRT sender signals derived from public `srt-transport`
//! logical-caller statistics. Nothing here reads private table state.

use std::time::{Duration, Instant};

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

/// Bytes per second as megabits per second (the unit `mbps_*` fields carry).
fn megabits_per_second(bytes_per_second: f64) -> f64 {
    bytes_per_second * 8.0 / 1_000_000.0
}

/// `current - previous` as a per-second rate, or `None` when the interval is
/// empty or the counter moved backwards (a reset is not a zero rate).
fn per_second(current: u64, previous: u64, interval: Duration) -> Option<f64> {
    let seconds = interval.as_secs_f64();
    (seconds > 0.0 && current >= previous).then(|| (current - previous) as f64 / seconds)
}

/// One cumulative wire-sender counter reading, stamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SenderCounterSample {
    observed_at: Instant,
    wire_srt_bytes_sent: u64,
}

/// Per-caller sent-rate sampling state.
///
/// `mbps_send_rate` must mean what Restream actually put on the wire, so it
/// comes from the caller's own wire byte counter — a direct caller's
/// `total_srt_bytes_sent`, or a bond's summed `wire_srt_bytes_sent` — and
/// never from `peer_receiving_rate_bytes_per_second`, which is peer ACK
/// feedback about the peer's receive rate. A counter only becomes a rate
/// across two observations, so the sampler is stateful and lives beside the
/// leaf that owns the caller: a first sample, a reset, or an absent sender
/// direction yields no rate rather than a fabricated zero.
#[derive(Debug, Default)]
pub(crate) struct SenderQualitySampler {
    previous: Option<SenderCounterSample>,
}

impl SenderQualitySampler {
    /// Sample one logical caller's sender statistics observed at `now`,
    /// folding in the interval since this sampler last saw the same caller.
    pub(crate) fn sample(
        &mut self,
        stats: &LogicalCallerStats,
        now: Instant,
    ) -> Option<PublisherQuality> {
        match stats {
            LogicalCallerStats::Direct(stats) => {
                let sender = stats.sender?;
                let mbps_send_rate = self.rate(sender.total_srt_bytes_sent, now);
                Some(PublisherQuality {
                    // Peer feedback: `None` until an ACK carries the RTT
                    // section, never a fabricated zero.
                    ms_rtt: sender
                        .peer_rtt_micros
                        .map(|micros| f64::from(micros) / 1_000.0),
                    mbps_send_rate,
                    packets_sent_loss: Some(sender.total_lost),
                    packets_sent_drop: Some(sender.total_dropped),
                    ..PublisherQuality::default()
                })
            }
            LogicalCallerStats::Group(stats) => {
                if !stats.legs.iter().any(|leg| leg.connection.sender.is_some()) {
                    return None;
                }
                let mbps_send_rate = self.rate(stats.aggregate.wire_srt_bytes_sent, now);
                let mut rtt_total = 0_f64;
                let mut rtt_count = 0_u64;
                for leg in &stats.legs {
                    if let Some(rtt) = leg
                        .connection
                        .sender
                        .as_ref()
                        .and_then(|sender| sender.peer_rtt_micros)
                    {
                        rtt_total += f64::from(rtt);
                        rtt_count += 1;
                    }
                }
                Some(PublisherQuality {
                    ms_rtt: (rtt_count > 0).then(|| rtt_total / rtt_count as f64 / 1_000.0),
                    mbps_send_rate,
                    packets_sent_loss: Some(stats.aggregate.wire_sender_packets_lost),
                    // A bond reports no aggregate sender TLPKTDROP counter, so
                    // drops stay unknown instead of reading as zero.
                    packets_sent_drop: None,
                    ..PublisherQuality::default()
                })
            }
        }
    }

    /// Fold one wire-sender counter reading into a rate against the previous
    /// one. The baseline advances even when the reading moved backwards, so a
    /// reset costs exactly one interval of rate instead of poisoning the next.
    fn rate(&mut self, wire_srt_bytes_sent: u64, now: Instant) -> Option<f64> {
        let previous = self.previous.replace(SenderCounterSample {
            observed_at: now,
            wire_srt_bytes_sent,
        })?;
        per_second(
            wire_srt_bytes_sent,
            previous.wire_srt_bytes_sent,
            now.saturating_duration_since(previous.observed_at),
        )
        .map(megabits_per_second)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use srt_proto::ConnectionStats;
    use srt_proto::sender::SenderStats;
    use srt_transport::advanced::group::{
        GroupAggregateStats, GroupConnectionStats, GroupLegStats,
    };

    /// A sender with the wire counter under test; peer feedback stays absent
    /// unless a test sets it, exactly as it is until an ACK arrives.
    fn sender(wire_srt_bytes_sent: u64) -> SenderStats {
        SenderStats {
            total_srt_bytes_sent: wire_srt_bytes_sent,
            ..SenderStats::default()
        }
    }

    fn direct(wire_srt_bytes_sent: u64) -> LogicalCallerStats {
        LogicalCallerStats::Direct(Box::new(ConnectionStats {
            sender: Some(sender(wire_srt_bytes_sent)),
            receiver: None,
        }))
    }

    fn leg(member_id: u32, wire_srt_bytes_sent: u64) -> GroupLegStats {
        GroupLegStats {
            member_id,
            weight: 1,
            state: srt_proto::GroupMemberState::Active,
            local_addr: None,
            peer_addr: None,
            connection: ConnectionStats {
                sender: Some(sender(wire_srt_bytes_sent)),
                receiver: None,
            },
        }
    }

    fn group(legs: Vec<GroupLegStats>, wire_srt_bytes_sent: u64) -> LogicalCallerStats {
        LogicalCallerStats::Group(Box::new(GroupConnectionStats {
            group_id: 1,
            mode: srt_proto::GroupMode::Broadcast,
            aggregate: GroupAggregateStats {
                wire_srt_bytes_sent,
                ..GroupAggregateStats::default()
            },
            legs,
        }))
    }

    #[test]
    fn direct_send_rate_is_the_local_wire_byte_delta_not_the_peer_rate() {
        let t0 = Instant::now();
        let mut sampler = SenderQualitySampler::default();
        // A peer-reported receive rate is present from the first observation,
        // but the sent rate is local and only exists across two samples.
        let first = LogicalCallerStats::Direct(Box::new(ConnectionStats {
            sender: Some(SenderStats {
                total_srt_bytes_sent: 0,
                peer_receiving_rate_bytes_per_second: Some(1_000_000),
                ..SenderStats::default()
            }),
            receiver: None,
        }));
        let quality = sampler.sample(&first, t0).expect("sender stats present");
        assert_eq!(quality.mbps_send_rate, None, "first sample has no interval");
        // 500 000 wire bytes in one second is 4 Mbps; the peer's 8 Mbps
        // receive-rate advertisement must not leak into it.
        let second = direct(500_000);
        let quality = sampler
            .sample(&second, t0 + Duration::from_secs(1))
            .unwrap();
        assert_eq!(quality.mbps_send_rate, Some(4.0));
    }

    #[test]
    fn a_slow_sample_interval_still_yields_a_per_second_rate() {
        let t0 = Instant::now();
        let mut sampler = SenderQualitySampler::default();
        sampler.sample(&direct(0), t0).unwrap();
        let quality = sampler
            .sample(&direct(2_000_000), t0 + Duration::from_secs(2))
            .unwrap();
        assert_eq!(quality.mbps_send_rate, Some(8.0));
    }

    #[test]
    fn a_counter_reset_costs_one_interval_and_rebaselines() {
        let t0 = Instant::now();
        let mut sampler = SenderQualitySampler::default();
        sampler.sample(&direct(1_000_000), t0).unwrap();
        let reset = sampler
            .sample(&direct(100), t0 + Duration::from_secs(1))
            .unwrap();
        assert_eq!(reset.mbps_send_rate, None, "a reset is not a zero rate");
        let after = sampler
            .sample(&direct(100 + 1_000_000), t0 + Duration::from_secs(2))
            .unwrap();
        assert_eq!(after.mbps_send_rate, Some(8.0));
    }

    #[test]
    fn direct_rtt_and_drop_stay_absent_until_the_transport_reports_them() {
        let t0 = Instant::now();
        let mut sampler = SenderQualitySampler::default();
        let quality = sampler.sample(&direct(0), t0).unwrap();
        assert_eq!(quality.ms_rtt, None, "no ACK feedback yet");
        assert_eq!(quality.packets_sent_drop, Some(0), "a real local counter");
        let feedback = LogicalCallerStats::Direct(Box::new(ConnectionStats {
            sender: Some(SenderStats {
                peer_rtt_micros: Some(12_500),
                total_dropped: 3,
                ..SenderStats::default()
            }),
            receiver: None,
        }));
        let quality = sampler
            .sample(&feedback, t0 + Duration::from_secs(1))
            .unwrap();
        assert_eq!(quality.ms_rtt, Some(12.5));
        assert_eq!(quality.packets_sent_drop, Some(3));
    }

    #[test]
    fn group_send_rate_uses_the_wire_aggregate_delta() {
        let t0 = Instant::now();
        let mut sampler = SenderQualitySampler::default();
        let first = group(vec![leg(1, 0), leg(2, 0)], 0);
        assert_eq!(sampler.sample(&first, t0).unwrap().mbps_send_rate, None);
        // Two legs each carrying a full copy: 2 x 500 000 wire bytes in one
        // second is 8 Mbps of wire traffic.
        let second = group(vec![leg(1, 500_000), leg(2, 500_000)], 1_000_000);
        let quality = sampler
            .sample(&second, t0 + Duration::from_secs(1))
            .unwrap();
        assert_eq!(quality.mbps_send_rate, Some(8.0));
    }

    #[test]
    fn group_rtt_averages_reporting_legs_and_drops_stay_unknown() {
        let t0 = Instant::now();
        let mut sampler = SenderQualitySampler::default();
        let reporting = |member_id, rtt: Option<u32>| GroupLegStats {
            connection: ConnectionStats {
                sender: Some(SenderStats {
                    peer_rtt_micros: rtt,
                    ..SenderStats::default()
                }),
                receiver: None,
            },
            ..leg(member_id, 0)
        };
        let partial = group(vec![reporting(1, Some(10_000)), reporting(2, None)], 0);
        let quality = sampler.sample(&partial, t0).unwrap();
        assert_eq!(quality.ms_rtt, Some(10.0), "only legs reporting an RTT");
        assert_eq!(
            quality.packets_sent_drop, None,
            "no aggregate sender TLPKTDROP counter exists for a bond"
        );
        let silent = group(vec![reporting(1, None), reporting(2, None)], 0);
        let quality = sampler
            .sample(&silent, t0 + Duration::from_secs(1))
            .unwrap();
        assert_eq!(quality.ms_rtt, None, "never a fabricated zero");
    }

    #[test]
    fn a_caller_without_sender_statistics_has_no_sample() {
        let mut sampler = SenderQualitySampler::default();
        let no_sender = LogicalCallerStats::Direct(Box::default());
        assert!(sampler.sample(&no_sender, Instant::now()).is_none());
        let no_leg_senders = LogicalCallerStats::Group(Box::new(GroupConnectionStats {
            group_id: 1,
            mode: srt_proto::GroupMode::Broadcast,
            aggregate: GroupAggregateStats::default(),
            legs: vec![GroupLegStats {
                connection: ConnectionStats {
                    sender: None,
                    receiver: None,
                },
                ..leg(1, 0)
            }],
        }));
        assert!(sampler.sample(&no_leg_senders, Instant::now()).is_none());
    }
}
