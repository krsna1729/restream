//! Segment delivery for one HLS output: which published segments the
//! destination acknowledged in time. A byte rate over a fixed window cannot
//! grade HLS, because segments land in bursts of whole seconds and one in
//! flight at a window's edge reads as a shortfall. Here every segment
//! resolves once: acknowledged before it is due counts as delivered,
//! otherwise (late, retrying, dropped) as missed, and both counts move at
//! the instant it resolves, so a window over them has no edge error.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A segment acknowledged within one target duration of its publish keeps a
/// live player's three-segment buffer from starving.
pub(crate) const SEGMENT_DUE_AFTER: Duration = Duration::from_secs(6);

#[derive(Debug, Default)]
pub(crate) struct SegmentDelivery {
    /// Published, unresolved segments, oldest (first due) first.
    awaiting: VecDeque<Awaiting>,
    due_bytes: u64,
    on_time_bytes: u64,
}

#[derive(Debug)]
struct Awaiting {
    sequence: u64,
    bytes: u64,
    due_at: Instant,
}

impl SegmentDelivery {
    pub(crate) fn published(&mut self, sequence: u64, bytes: u64, now: Instant) {
        self.settle(now);
        self.awaiting.push_back(Awaiting {
            sequence,
            bytes,
            due_at: now + SEGMENT_DUE_AFTER,
        });
    }

    pub(crate) fn acknowledged(&mut self, sequence: u64, now: Instant) {
        self.settle(now);
        if let Some(position) = self
            .awaiting
            .iter()
            .position(|segment| segment.sequence == sequence)
            && let Some(segment) = self.awaiting.remove(position)
        {
            self.due_bytes = self.due_bytes.saturating_add(segment.bytes);
            self.on_time_bytes = self.on_time_bytes.saturating_add(segment.bytes);
        }
    }

    /// Resolve every segment past its due time as missed, and return the
    /// cumulative `(on-time bytes, resolved bytes)`; neither ever decreases.
    pub(crate) fn totals(&mut self, now: Instant) -> (u64, u64) {
        self.settle(now);
        (self.on_time_bytes, self.due_bytes)
    }

    fn settle(&mut self, now: Instant) {
        while self
            .awaiting
            .front()
            .is_some_and(|segment| segment.due_at <= now)
            && let Some(segment) = self.awaiting.pop_front()
        {
            self.due_bytes = self.due_bytes.saturating_add(segment.bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each segment resolves once, on time only when acknowledged before
    /// it is due: a late, never-acknowledged, or repeated acknowledgement
    /// counts as missed or not at all.
    #[test]
    fn a_segment_counts_as_delivered_only_when_acknowledged_before_it_is_due() {
        let start = Instant::now();
        let mut delivery = SegmentDelivery::default();
        delivery.published(0, 100, start);
        delivery.published(1, 200, start);
        delivery.published(2, 400, start);
        delivery.acknowledged(0, start + SEGMENT_DUE_AFTER / 2);
        assert_eq!(delivery.totals(start + SEGMENT_DUE_AFTER / 2), (100, 100));
        // Segment 1 is acknowledged late; segment 2 never.
        let late = start + SEGMENT_DUE_AFTER;
        delivery.acknowledged(1, late);
        delivery.acknowledged(0, late);
        assert_eq!(delivery.totals(late), (100, 700));
    }
}
