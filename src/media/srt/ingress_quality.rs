//! Per-publisher SRT receive quality, sampled on the ingress owner thread from
//! public `srt-transport` logical-peer statistics and read by Tokio.
//!
//! The Owner thread is the only writer; it samples a bounded slice of live
//! peers at a low rate (see `ingress_owner`) and drops a peer's sample when the
//! peer is retired. Tokio reads a copy by `LogicalPeerId` and folds it into the
//! ingest's `PublisherQuality`. Every field is authoritative `ReceiverStats`
//! state; nothing is derived from guessed byte capacities.

use std::collections::HashMap;
use std::sync::Mutex;

use std::time::Duration;

use srt_transport::advanced::admission::{LogicalPeerId, LogicalPeerStats};

use crate::media::snapshots::PublisherQuality;

/// One receive-quality sample for a logical peer (a direct connection or a
/// bonded group). Plain numbers, so it crosses threads freely.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PeerReceiverSample {
    pub packets_in_buffer: u32,
    pub payload_bytes_in_buffer: u64,
    pub max_buffer_packets: u32,
    pub buffer_span_micros: u64,
    pub rtt_micros: u32,
    pub jitter_micros: u32,
    pub receiving_rate_bytes_per_second: u64,
    pub tsbpd_delay_micros: u64,
    pub total_lost: u64,
    pub total_dropped: u64,
    pub total_retransmitted: u64,
    pub total_undecryptable: u64,
}

impl PeerReceiverSample {
    /// Occupancy of the receive buffer in percent, when its capacity is known.
    #[cfg(test)]
    pub(crate) fn occupancy_percent(&self) -> Option<f64> {
        (self.max_buffer_packets > 0)
            .then(|| f64::from(self.packets_in_buffer) / f64::from(self.max_buffer_packets) * 100.0)
    }
}

/// Sample a peer's receive state from its public statistics. A bonded peer
/// reports the worst (fullest) leg buffer, the mean leg RTT and jitter, summed
/// rate and counters, and the largest configured delay.
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
                receiving_rate_bytes_per_second: u64::from(
                    receiver.receiving_rate_bytes_per_second,
                ),
                tsbpd_delay_micros: receiver.tsbpd_delay_micros,
                total_lost: receiver.total_lost,
                total_dropped: receiver.total_dropped,
                total_retransmitted: receiver.total_retransmitted,
                total_undecryptable: receiver.total_undecryptable,
            })
        }
        LogicalPeerStats::Group(stats) => {
            let mut sample = PeerReceiverSample::default();
            let mut legs = 0_u64;
            let (mut rtt, mut jitter) = (0_u64, 0_u64);
            let mut fullest: Option<(f64, PeerReceiverSample)> = None;
            for leg in &stats.legs {
                let Some(receiver) = leg.connection.receiver else {
                    continue;
                };
                legs += 1;
                rtt += u64::from(receiver.rtt);
                jitter += u64::from(receiver.jitter);
                sample.receiving_rate_bytes_per_second +=
                    u64::from(receiver.receiving_rate_bytes_per_second);
                sample.tsbpd_delay_micros =
                    sample.tsbpd_delay_micros.max(receiver.tsbpd_delay_micros);
                sample.total_lost += receiver.total_lost;
                sample.total_dropped += receiver.total_dropped;
                sample.total_retransmitted += receiver.total_retransmitted;
                sample.total_undecryptable += receiver.total_undecryptable;
                sample.buffer_span_micros =
                    sample.buffer_span_micros.max(receiver.buffer_span_micros);
                let fill = if receiver.max_buffer_packets > 0 {
                    f64::from(receiver.packets_in_buffer) / f64::from(receiver.max_buffer_packets)
                } else {
                    0.0
                };
                if fullest.as_ref().is_none_or(|(best, _)| fill > *best) {
                    fullest = Some((
                        fill,
                        PeerReceiverSample {
                            packets_in_buffer: receiver.packets_in_buffer,
                            payload_bytes_in_buffer: receiver.payload_bytes_in_buffer,
                            max_buffer_packets: receiver.max_buffer_packets,
                            ..PeerReceiverSample::default()
                        },
                    ));
                }
            }
            if let Some((_, buffer)) = fullest {
                sample.packets_in_buffer = buffer.packets_in_buffer;
                sample.payload_bytes_in_buffer = buffer.payload_bytes_in_buffer;
                sample.max_buffer_packets = buffer.max_buffer_packets;
            }
            if legs == 0 {
                return None;
            }
            sample.rtt_micros = u32::try_from(rtt / legs).unwrap_or(u32::MAX);
            sample.jitter_micros = u32::try_from(jitter / legs).unwrap_or(u32::MAX);
            Some(sample)
        }
    }
}

/// The ingest quality snapshot for one sample. When the previous sample and the
/// time since it are known, counter rates per second are derived from the
/// counter deltas; otherwise they stay unset (never a guess).
pub(crate) fn quality_from_sample(
    sample: &PeerReceiverSample,
    previous: Option<(&PeerReceiverSample, Duration)>,
) -> PublisherQuality {
    let rate = |current: u64, earlier: fn(&PeerReceiverSample) -> u64| {
        let (previous, elapsed) = previous?;
        let seconds = elapsed.as_secs_f64();
        (seconds > 0.0).then(|| current.saturating_sub(earlier(previous)) as f64 / seconds)
    };
    PublisherQuality {
        packets_received_loss_per_sec: rate(sample.total_lost, |s| s.total_lost),
        packets_received_drop_per_sec: rate(sample.total_dropped, |s| s.total_dropped),
        packets_received_retrans_per_sec: rate(sample.total_retransmitted, |s| {
            s.total_retransmitted
        }),
        packets_received_undecrypt_per_sec: rate(sample.total_undecryptable, |s| {
            s.total_undecryptable
        }),
        ms_rtt: Some(f64::from(sample.rtt_micros) / 1_000.0),
        mbps_receive_rate: Some(sample.receiving_rate_bytes_per_second as f64 * 8.0 / 1_000_000.0),
        ms_receive_tsb_pd_delay: Some(sample.tsbpd_delay_micros as f64 / 1_000.0),
        ms_receive_buf: Some(sample.buffer_span_micros as f64 / 1_000.0),
        packets_received_loss: Some(sample.total_lost),
        packets_received_drop: Some(sample.total_dropped),
        packets_received_retrans: Some(sample.total_retransmitted),
        packets_received_undecrypt: Some(sample.total_undecryptable),
        srt_recv_buf_packets: Some(sample.packets_in_buffer),
        srt_recv_buf_capacity_packets: Some(sample.max_buffer_packets),
        srt_recv_buf_payload_bytes: Some(sample.payload_bytes_in_buffer),
        ..PublisherQuality::default()
    }
}

/// Owner-written, Tokio-read receive samples keyed by `LogicalPeerId`. Bounded
/// by the live peer count: the Owner removes a peer's entry when it retires it.
#[derive(Default)]
pub(crate) struct PeerSampleTable {
    samples: Mutex<HashMap<LogicalPeerId, PeerReceiverSample>>,
}

impl PeerSampleTable {
    pub(crate) fn record(&self, peer: LogicalPeerId, sample: PeerReceiverSample) {
        self.samples
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(peer, sample);
    }

    pub(crate) fn forget(&self, peer: &LogicalPeerId) {
        self.samples
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(peer);
    }

    pub(crate) fn get(&self, peer: &LogicalPeerId) -> Option<PeerReceiverSample> {
        self.samples
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(peer)
            .copied()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.samples
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use srt_proto::ConnectionStats;
    use srt_proto::receiver::ReceiverStats;

    fn direct(receiver: ReceiverStats) -> LogicalPeerStats {
        LogicalPeerStats::Direct(Box::new(ConnectionStats {
            sender: None,
            receiver: Some(receiver),
        }))
    }

    #[test]
    fn direct_peer_maps_authoritative_receiver_state() {
        let stats = direct(ReceiverStats {
            packets_in_buffer: 300,
            max_buffer_packets: 1_000,
            payload_bytes_in_buffer: 394_800,
            rtt: 12_500,
            jitter: 700,
            receiving_rate_bytes_per_second: 1_000_000,
            tsbpd_delay_micros: 120_000,
            total_lost: 4,
            total_dropped: 1,
            total_retransmitted: 3,
            total_undecryptable: 2,
            buffer_span_micros: 90_000,
            ..ReceiverStats::default()
        });
        let sample = sample_from_stats(&stats).expect("receiver stats present");
        assert_eq!(sample.occupancy_percent(), Some(30.0));
        let quality = quality_from_sample(&sample, None);
        assert_eq!(quality.ms_rtt, Some(12.5));
        assert_eq!(quality.mbps_receive_rate, Some(8.0));
        assert_eq!(quality.ms_receive_tsb_pd_delay, Some(120.0));
        assert_eq!(quality.srt_recv_buf_packets, Some(300));
        assert_eq!(quality.srt_recv_buf_capacity_packets, Some(1_000));
        assert_eq!(quality.srt_recv_buf_payload_bytes, Some(394_800));
        assert_eq!(quality.packets_received_loss, Some(4));
        assert_eq!(quality.packets_received_drop, Some(1));
        assert_eq!(quality.packets_received_retrans, Some(3));
        assert_eq!(quality.packets_received_undecrypt, Some(2));
    }

    #[test]
    fn counter_rates_come_from_sample_deltas_only() {
        let earlier = PeerReceiverSample {
            total_lost: 10,
            total_retransmitted: 4,
            ..PeerReceiverSample::default()
        };
        let later = PeerReceiverSample {
            total_lost: 14,
            total_retransmitted: 4,
            total_dropped: 2,
            ..PeerReceiverSample::default()
        };
        let quality = quality_from_sample(&later, Some((&earlier, Duration::from_secs(2))));
        assert_eq!(quality.packets_received_loss_per_sec, Some(2.0));
        assert_eq!(quality.packets_received_retrans_per_sec, Some(0.0));
        assert_eq!(quality.packets_received_drop_per_sec, Some(1.0));
        let first = quality_from_sample(&later, None);
        assert_eq!(first.packets_received_loss_per_sec, None);
    }

    #[test]
    fn a_peer_without_receiver_state_has_no_sample() {
        let stats = LogicalPeerStats::Direct(Box::default());
        assert_eq!(sample_from_stats(&stats), None);
    }

    #[test]
    fn unknown_capacity_yields_no_occupancy_never_a_guess() {
        let sample = PeerReceiverSample {
            packets_in_buffer: 10,
            max_buffer_packets: 0,
            ..PeerReceiverSample::default()
        };
        assert_eq!(sample.occupancy_percent(), None);
    }
}
