//! Per-output delivery telemetry: the bytes a destination actually received
//! (peer-acknowledged) against the bytes its feed offered over the same
//! window. Sampled once per second in each shard's bounded stall sweep, off
//! the media path. The harness measures the same thing at the receiver
//! (`resource_sweep/delivery.rs`), so a disagreement is itself a finding.

use std::time::{Duration, Instant};

/// Shortest window a delivery rate is computed over. The stall sweep samples
/// every second, but one second of peer ACKs against one second of publishing
/// swings with keyframe bursts and ACK timing; five seconds is steady enough
/// to compare against the 0.95 floor and still reacts within a few samples.
pub(crate) const DELIVERY_WINDOW: Duration = Duration::from_secs(5);

/// Cumulative counters at the start of the current window.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DeliveryMark {
    delivered_bytes: u64,
    offered_bytes: u64,
    at: Instant,
}

/// Rates over the most recent complete window.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct DeliveryRates {
    pub(crate) delivered_bps: Option<f64>,
    pub(crate) offered_bps: Option<f64>,
    pub(crate) ratio: Option<f64>,
}

/// One output's delivery sampler, owned by its leaf on the shard thread.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DeliveryTracker {
    mark: Option<DeliveryMark>,
    rates: DeliveryRates,
}

impl DeliveryTracker {
    /// Feed the current cumulative counters and return the rates of the last
    /// complete [`DELIVERY_WINDOW`]. Until the first window closes there are no
    /// rates; a counter reset (a new connection) restarts the window rather
    /// than fabricating a value; the ratio is reported only when some load was
    /// offered.
    pub(crate) fn sample(
        &mut self,
        delivered_bytes: u64,
        offered_bytes: u64,
        now: Instant,
    ) -> DeliveryRates {
        let current = DeliveryMark {
            delivered_bytes,
            offered_bytes,
            at: now,
        };
        let Some(previous) = self.mark else {
            self.mark = Some(current);
            return self.rates;
        };
        if delivered_bytes < previous.delivered_bytes || offered_bytes < previous.offered_bytes {
            *self = Self {
                mark: Some(current),
                rates: DeliveryRates::default(),
            };
            return self.rates;
        }
        let elapsed = now.saturating_duration_since(previous.at);
        if elapsed < DELIVERY_WINDOW {
            return self.rates;
        }
        let seconds = elapsed.as_secs_f64();
        let delivered_bps = (delivered_bytes - previous.delivered_bytes) as f64 * 8.0 / seconds;
        let offered_bps = (offered_bytes - previous.offered_bytes) as f64 * 8.0 / seconds;
        self.mark = Some(current);
        self.rates = DeliveryRates {
            delivered_bps: Some(delivered_bps),
            offered_bps: Some(offered_bps),
            ratio: (offered_bps > 0.0).then(|| delivered_bps / offered_bps),
        };
        self.rates
    }
}

/// Jain's fairness index over destination rates: `(Σx)² / (n·Σx²)`, 1.0 when
/// all are equal and `1/n` when one receives everything.
pub(crate) fn jain_index(rates: &[f64]) -> Option<f64> {
    let total: f64 = rates.iter().sum();
    let squares: f64 = rates.iter().map(|rate| rate * rate).sum();
    (!rates.is_empty() && squares > 0.0).then(|| total * total / (rates.len() as f64 * squares))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const W: Duration = DELIVERY_WINDOW;

    #[test]
    fn rates_appear_once_a_window_closes_and_hold_until_the_next() {
        let start = Instant::now();
        let mut tracker = DeliveryTracker::default();
        assert_eq!(
            tracker.sample(1_000, 2_000, start),
            DeliveryRates::default()
        );
        assert_eq!(
            tracker.sample(1_500, 2_500, start + W / 2),
            DeliveryRates::default(),
            "no rate before a full window"
        );
        let rates = tracker.sample(6_000, 7_000, start + W);
        assert_eq!(rates.delivered_bps, Some(8_000.0));
        assert_eq!(rates.offered_bps, Some(8_000.0));
        assert_eq!(rates.ratio, Some(1.0));
        assert_eq!(
            tracker.sample(6_100, 9_000, start + W + W / 2),
            rates,
            "mid-window samples repeat the last complete window"
        );
    }

    #[test]
    fn a_stalled_destination_reports_a_low_ratio() {
        let start = Instant::now();
        let mut tracker = DeliveryTracker::default();
        tracker.sample(0, 0, start);
        let rates = tracker.sample(100, 1_000, start + W);
        assert_eq!(rates.ratio, Some(0.1));
    }

    #[test]
    fn counter_resets_and_idle_feeds_do_not_fabricate_rates() {
        let start = Instant::now();
        let mut tracker = DeliveryTracker::default();
        tracker.sample(0, 0, start);
        assert!(tracker.sample(5_000, 5_000, start + W).ratio.is_some());
        let reset = tracker.sample(10, 6_000, start + 2 * W);
        assert_eq!(reset, DeliveryRates::default(), "a reset clears old rates");
        let idle = tracker.sample(10, 6_000, start + 3 * W);
        assert_eq!(idle.offered_bps, Some(0.0));
        assert_eq!(idle.ratio, None, "no offered load means no ratio");
    }

    #[test]
    fn jain_index_is_one_when_equal_and_one_over_n_for_a_single_winner() {
        assert_eq!(jain_index(&[3.0, 3.0, 3.0]), Some(1.0));
        assert_eq!(jain_index(&[9.0, 0.0, 0.0]), Some(1.0 / 3.0));
        assert_eq!(jain_index(&[]), None);
    }
}
