//! Per-publisher SRT receive quality, sampled on the ingress owner thread from
//! public `srt-transport` logical-peer statistics.
//!
//! The Owner takes a bounded slice of samples per visit at a low rate, stamps
//! each with its observation time, and offers it to Tokio through a separate
//! bounded, LOSSY telemetry bridge (`try_send`): a full bridge drops that sample
//! and counts the drop, so telemetry can never delay protocol service. Tokio
//! turns two consecutive Owner observations of one peer into a quality snapshot;
//! rates use the interval between the two Owner observations, and a counter that
//! moved backwards yields no rate (the `srt-rs` `interval_since` rule) rather
//! than a silent zero.
//!
//! Direct and bonded peers are mapped with their own meaning. A bond's publisher
//! level rate is the LOGICAL payload rate (one copy of each delivered payload);
//! per-leg wire loss and undecryptable counters are reported as explicit wire
//! fields, never folded into the ordinary publisher loss counters, because one
//! degraded leg does not mean the deduplicated logical stream lost data.

use std::time::{Duration, Instant};

use srt_transport::advanced::admission::{LogicalPeerId, LogicalPeerStats};

use crate::media::snapshots::PublisherQuality;

/// Cumulative counters of a direct connection's receiver.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DirectCounters {
    pub receiving_rate_bytes_per_second: u64,
    pub total_lost: u64,
    pub total_dropped: u64,
    pub total_retransmitted: u64,
    pub total_undecryptable: u64,
}

/// Group-level view of a bonded peer: logical delivery plus wire degradation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GroupView {
    /// Payload bytes delivered ONCE at the group boundary (logical).
    pub logical_payload_bytes_received: u64,
    /// Receiver-side missing sequence numbers summed over legs (wire).
    pub wire_receiver_packets_lost: u64,
    /// Packets rejected at decryption, summed over legs (wire).
    pub wire_packets_undecryptable: u64,
    pub members: u32,
    pub connected_members: u32,
    pub active_members: u32,
    pub broken_members: u32,
}

/// What differs between a direct connection and a bond.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PeerKind {
    Direct(DirectCounters),
    Group(GroupView),
}

/// One receive-quality observation. Plain numbers, so it crosses threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PeerReceiverSample {
    /// Receive-buffer state (a bond reports its fullest leg).
    pub packets_in_buffer: u32,
    pub payload_bytes_in_buffer: u64,
    pub max_buffer_packets: u32,
    pub buffer_span_micros: u64,
    /// Path metrics (a bond reports the mean over legs).
    pub rtt_micros: u32,
    pub jitter_micros: u32,
    pub tsbpd_delay_micros: u64,
    pub kind: PeerKind,
}

impl PeerReceiverSample {
    /// Occupancy of the receive buffer in percent, when its capacity is known.
    #[cfg(test)]
    pub(crate) fn occupancy_percent(&self) -> Option<f64> {
        (self.max_buffer_packets > 0)
            .then(|| f64::from(self.packets_in_buffer) / f64::from(self.max_buffer_packets) * 100.0)
    }
}

/// A sample stamped with the time the Owner observed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Observation {
    pub observed_at: Instant,
    pub sample: PeerReceiverSample,
}

/// An observation addressed to one logical peer, as sent over the telemetry
/// bridge.
#[derive(Debug, Clone, Copy)]
pub(crate) struct QualitySample {
    pub peer: LogicalPeerId,
    pub observation: Observation,
}

/// Sample a peer's receive state from its public statistics.
pub(crate) fn sample_from_stats(stats: &LogicalPeerStats) -> Option<PeerReceiverSample> {
    match stats {
        LogicalPeerStats::Direct(stats) => {
            let receiver = stats.receiver?;
            Some(PeerReceiverSample {
                packets_in_buffer: receiver.packets_in_buffer,
                payload_bytes_in_buffer: receiver.payload_bytes_in_buffer,
                max_buffer_packets: receiver.max_buffer_packets,
                buffer_span_micros: receiver.buffer_span_micros,
                rtt_micros: receiver.rtt,
                jitter_micros: receiver.jitter,
                tsbpd_delay_micros: receiver.tsbpd_delay_micros,
                kind: PeerKind::Direct(DirectCounters {
                    receiving_rate_bytes_per_second: u64::from(
                        receiver.receiving_rate_bytes_per_second,
                    ),
                    total_lost: receiver.total_lost,
                    total_dropped: receiver.total_dropped,
                    total_retransmitted: receiver.total_retransmitted,
                    total_undecryptable: receiver.total_undecryptable,
                }),
            })
        }
        LogicalPeerStats::Group(stats) => {
            let mut legs = 0_u64;
            let (mut rtt, mut jitter) = (0_u64, 0_u64);
            let mut delay = 0_u64;
            let mut span = 0_u64;
            let mut fullest: Option<(f64, u32, u64, u32)> = None;
            for leg in &stats.legs {
                let Some(receiver) = leg.connection.receiver else {
                    continue;
                };
                legs += 1;
                rtt += u64::from(receiver.rtt);
                jitter += u64::from(receiver.jitter);
                delay = delay.max(receiver.tsbpd_delay_micros);
                span = span.max(receiver.buffer_span_micros);
                let fill = if receiver.max_buffer_packets > 0 {
                    f64::from(receiver.packets_in_buffer) / f64::from(receiver.max_buffer_packets)
                } else {
                    0.0
                };
                if fullest.is_none_or(|(best, ..)| fill > best) {
                    fullest = Some((
                        fill,
                        receiver.packets_in_buffer,
                        receiver.payload_bytes_in_buffer,
                        receiver.max_buffer_packets,
                    ));
                }
            }
            let (_, packets, bytes, capacity) = fullest?;
            let aggregate = &stats.aggregate;
            let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
            Some(PeerReceiverSample {
                packets_in_buffer: packets,
                payload_bytes_in_buffer: bytes,
                max_buffer_packets: capacity,
                buffer_span_micros: span,
                rtt_micros: u32::try_from(rtt / legs).unwrap_or(u32::MAX),
                jitter_micros: u32::try_from(jitter / legs).unwrap_or(u32::MAX),
                tsbpd_delay_micros: delay,
                kind: PeerKind::Group(GroupView {
                    logical_payload_bytes_received: aggregate.logical_payload_bytes_received,
                    wire_receiver_packets_lost: aggregate.wire_receiver_packets_lost,
                    wire_packets_undecryptable: aggregate.wire_packets_undecryptable,
                    members: count(stats.legs.len()),
                    connected_members: count(aggregate.active_legs + aggregate.standby_legs),
                    active_members: count(aggregate.active_legs),
                    broken_members: count(aggregate.broken_legs),
                }),
            })
        }
    }
}

/// `current - previous` as a per-second rate, or `None` when the interval is
/// empty or the counter moved backwards (a reset is not a zero rate).
fn per_second(current: u64, previous: u64, interval: Duration) -> Option<f64> {
    let seconds = interval.as_secs_f64();
    (seconds > 0.0 && current >= previous).then(|| (current - previous) as f64 / seconds)
}

/// The ingest quality snapshot for `current`, with rates computed against the
/// previous Owner observation of the same peer when there is one.
pub(crate) fn quality_from(
    current: &Observation,
    previous: Option<&Observation>,
) -> PublisherQuality {
    let sample = &current.sample;
    let interval = previous.map(|previous| {
        (
            previous,
            current
                .observed_at
                .saturating_duration_since(previous.observed_at),
        )
    });
    let mut quality = PublisherQuality {
        ms_rtt: Some(f64::from(sample.rtt_micros) / 1_000.0),
        ms_receive_tsb_pd_delay: Some(sample.tsbpd_delay_micros as f64 / 1_000.0),
        ms_receive_buf: Some(sample.buffer_span_micros as f64 / 1_000.0),
        srt_recv_buf_packets: Some(sample.packets_in_buffer),
        srt_recv_buf_capacity_packets: Some(sample.max_buffer_packets),
        srt_recv_buf_payload_bytes: Some(sample.payload_bytes_in_buffer),
        ..PublisherQuality::default()
    };
    match sample.kind {
        PeerKind::Direct(now) => {
            quality.mbps_receive_rate =
                Some(now.receiving_rate_bytes_per_second as f64 * 8.0 / 1_000_000.0);
            quality.packets_received_loss = Some(now.total_lost);
            quality.packets_received_drop = Some(now.total_dropped);
            quality.packets_received_retrans = Some(now.total_retransmitted);
            quality.packets_received_undecrypt = Some(now.total_undecryptable);
            if let Some((previous, elapsed)) = interval
                && let PeerKind::Direct(before) = previous.sample.kind
            {
                quality.packets_received_loss_per_sec =
                    per_second(now.total_lost, before.total_lost, elapsed);
                quality.packets_received_drop_per_sec =
                    per_second(now.total_dropped, before.total_dropped, elapsed);
                quality.packets_received_retrans_per_sec =
                    per_second(now.total_retransmitted, before.total_retransmitted, elapsed);
                quality.packets_received_undecrypt_per_sec =
                    per_second(now.total_undecryptable, before.total_undecryptable, elapsed);
            }
        }
        PeerKind::Group(now) => {
            // Bond identity and member state.
            quality.srt_bonded = Some(true);
            quality.srt_group_member_count = Some(now.members);
            quality.srt_group_connected_members = Some(now.connected_members);
            quality.srt_group_active_members = Some(now.active_members);
            quality.srt_group_broken_members = Some(now.broken_members);
            // Wire degradation stays explicitly wire: never the ordinary
            // publisher loss/undecrypt counters.
            quality.srt_group_wire_receiver_packets_lost = Some(now.wire_receiver_packets_lost);
            quality.srt_group_wire_packets_undecryptable = Some(now.wire_packets_undecryptable);
            // Logical payload rate: one copy of each delivered payload.
            if let Some((previous, elapsed)) = interval
                && let PeerKind::Group(before) = previous.sample.kind
            {
                quality.mbps_receive_rate = per_second(
                    now.logical_payload_bytes_received,
                    before.logical_payload_bytes_received,
                    elapsed,
                )
                .map(|bytes_per_second| bytes_per_second * 8.0 / 1_000_000.0);
            }
        }
    }
    quality
}

/// Per-publisher fold state: the last Owner observation seen for the peer.
#[derive(Debug, Default)]
pub(crate) struct QualityFold {
    last: Option<Observation>,
}

impl QualityFold {
    /// Fold one Owner observation. One no newer than the last (a duplicate or
    /// out-of-order generation) is ignored and yields `None`, so the same
    /// observation can never produce a zero rate followed by a mis-timed spike.
    pub(crate) fn fold(&mut self, current: Observation) -> Option<PublisherQuality> {
        if let Some(last) = &self.last
            && current.observed_at <= last.observed_at
        {
            return None;
        }
        let quality = quality_from(&current, self.last.as_ref());
        self.last = Some(current);
        Some(quality)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use srt_proto::receiver::ReceiverStats;
    use srt_proto::{ConnectionStats, GroupMemberState, GroupMode};
    use srt_transport::advanced::group::{
        GroupAggregateStats, GroupConnectionStats, GroupLegStats,
    };

    fn at(base: Instant, seconds: u64, sample: PeerReceiverSample) -> Observation {
        Observation {
            observed_at: base + Duration::from_secs(seconds),
            sample,
        }
    }

    fn direct_sample(counters: DirectCounters) -> PeerReceiverSample {
        PeerReceiverSample {
            packets_in_buffer: 300,
            payload_bytes_in_buffer: 394_800,
            max_buffer_packets: 1_000,
            buffer_span_micros: 90_000,
            rtt_micros: 12_500,
            jitter_micros: 700,
            tsbpd_delay_micros: 120_000,
            kind: PeerKind::Direct(counters),
        }
    }

    fn lost(total_lost: u64) -> DirectCounters {
        DirectCounters {
            total_lost,
            ..DirectCounters::default()
        }
    }

    fn receiver(rate: u32, lost: u64) -> ReceiverStats {
        ReceiverStats {
            packets_in_buffer: 10,
            max_buffer_packets: 100,
            rtt: 10_000,
            jitter: 500,
            receiving_rate_bytes_per_second: rate,
            tsbpd_delay_micros: 120_000,
            total_lost: lost,
            ..ReceiverStats::default()
        }
    }

    #[test]
    fn direct_peer_maps_authoritative_receiver_state() {
        let stats = LogicalPeerStats::Direct(Box::new(ConnectionStats {
            sender: None,
            receiver: Some(ReceiverStats {
                packets_in_buffer: 300,
                max_buffer_packets: 1_000,
                payload_bytes_in_buffer: 394_800,
                rtt: 12_500,
                receiving_rate_bytes_per_second: 1_000_000,
                tsbpd_delay_micros: 120_000,
                total_lost: 4,
                ..ReceiverStats::default()
            }),
        }));
        let sample = sample_from_stats(&stats).expect("receiver stats present");
        assert_eq!(sample.occupancy_percent(), Some(30.0));
        let observation = Observation {
            observed_at: Instant::now(),
            sample,
        };
        let quality = quality_from(&observation, None);
        assert_eq!(quality.ms_rtt, Some(12.5));
        assert_eq!(quality.mbps_receive_rate, Some(8.0));
        assert_eq!(quality.srt_recv_buf_packets, Some(300));
        assert_eq!(quality.srt_recv_buf_capacity_packets, Some(1_000));
        assert_eq!(quality.packets_received_loss, Some(4));
        assert_eq!(quality.srt_bonded, None);
        assert_eq!(quality.packets_received_loss_per_sec, None);
    }

    #[test]
    fn a_peer_without_receiver_state_has_no_sample() {
        let stats = LogicalPeerStats::Direct(Box::default());
        assert_eq!(sample_from_stats(&stats), None);
    }

    #[test]
    fn rates_use_the_interval_between_owner_observations() {
        let t0 = Instant::now();
        let first = at(t0, 0, direct_sample(lost(10)));
        let second = at(
            t0,
            2,
            direct_sample(DirectCounters {
                total_lost: 14,
                total_dropped: 2,
                ..DirectCounters::default()
            }),
        );
        let quality = quality_from(&second, Some(&first));
        assert_eq!(quality.packets_received_loss_per_sec, Some(2.0));
        assert_eq!(quality.packets_received_drop_per_sec, Some(1.0));
        assert_eq!(quality.packets_received_retrans_per_sec, Some(0.0));
    }

    #[test]
    fn a_counter_reset_is_no_rate_not_a_zero() {
        let t0 = Instant::now();
        let first = at(t0, 0, direct_sample(lost(50)));
        let after_reset = at(t0, 1, direct_sample(lost(3)));
        let quality = quality_from(&after_reset, Some(&first));
        assert_eq!(quality.packets_received_loss_per_sec, None);
    }

    #[test]
    fn a_duplicate_owner_sample_is_ignored_and_never_a_zero_rate() {
        let t0 = Instant::now();
        let mut fold = QualityFold::default();
        let first = at(t0, 0, direct_sample(lost(10)));
        assert!(fold.fold(first).is_some());
        assert!(fold.fold(first).is_none(), "the same observation twice");
        // The next real observation is measured against the FIRST one, over
        // the true elapsed interval, not against the duplicate.
        let later = at(t0, 2, direct_sample(lost(14)));
        let quality = fold.fold(later).expect("newer observation");
        assert_eq!(quality.packets_received_loss_per_sec, Some(2.0));
    }

    fn group_sample(leg_lost: [u64; 2], logical_bytes: u64) -> PeerReceiverSample {
        let leg = |member_id, lost| GroupLegStats {
            member_id,
            weight: 1,
            state: GroupMemberState::Active,
            local_addr: None,
            peer_addr: None,
            connection: ConnectionStats {
                sender: None,
                receiver: Some(receiver(1_000_000, lost)),
            },
        };
        let stats = LogicalPeerStats::Group(Box::new(GroupConnectionStats {
            group_id: 1,
            mode: GroupMode::Broadcast,
            aggregate: GroupAggregateStats {
                active_legs: 2,
                broken_legs: 0,
                logical_payload_bytes_received: logical_bytes,
                wire_receiver_packets_lost: leg_lost.iter().sum(),
                ..GroupAggregateStats::default()
            },
            legs: vec![leg(1, leg_lost[0]), leg(2, leg_lost[1])],
        }));
        sample_from_stats(&stats).expect("group sample")
    }

    #[test]
    fn a_bond_reports_logical_rate_and_explicit_wire_degradation() {
        let t0 = Instant::now();
        let first = at(t0, 0, group_sample([0, 0], 0));
        // 1 000 000 logical payload bytes in 1 s = 8 Mbps, although each of the
        // two legs also carried a full copy (2 x 1 MB/s on the wire).
        let second = at(t0, 1, group_sample([7, 0], 1_000_000));
        let quality = quality_from(&second, Some(&first));
        assert_eq!(quality.srt_bonded, Some(true));
        assert_eq!(quality.srt_group_member_count, Some(2));
        assert_eq!(quality.srt_group_connected_members, Some(2));
        assert_eq!(quality.srt_group_active_members, Some(2));
        assert_eq!(quality.srt_group_broken_members, Some(0));
        assert_eq!(
            quality.mbps_receive_rate,
            Some(8.0),
            "logical rate, not the sum of the legs"
        );
        // One degraded leg is visible as wire loss, never as ordinary
        // publisher loss.
        assert_eq!(quality.srt_group_wire_receiver_packets_lost, Some(7));
        assert_eq!(quality.packets_received_loss, None);
        assert_eq!(quality.packets_received_loss_per_sec, None);
        // No logical rate without a previous observation.
        assert_eq!(quality_from(&first, None).mbps_receive_rate, None);
    }
}
