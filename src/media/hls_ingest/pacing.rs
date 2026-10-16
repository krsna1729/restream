//! Real-time pacing of released segments. Pure: the session passes the wall
//! clock and the publisher's media time, and gets back how long to wait.
//!
//! Every other TS source reaches the ring in real time (RTMP and SRT are
//! live; file ingest runs FFmpeg with `-re`). A whole HLS segment released
//! at once would put seconds of media in the ring within milliseconds, which
//! trips egress stall detection and floods downstream players.

use std::time::{Duration, Instant};

/// Wall instant and media time of the first data published after a session
/// start or a discontinuity; later media is due at
/// `anchor_wall + (media - anchor_media)`.
#[derive(Debug, Default)]
pub(super) struct Pacer {
    anchor: Option<(Instant, i64)>,
}

impl Pacer {
    /// The next data starts an unrelated timeline.
    pub(super) fn reset(&mut self) {
        self.anchor = None;
    }

    /// How long to wait before publishing more, given the media time
    /// published so far. Media never runs ahead of the wall clock by more
    /// than the data already in flight, and a wait is never longer than
    /// `lead`: a longer one means a timestamp jump, which re-anchors. Media
    /// that fell behind is published at once (catch-up), but never more than
    /// `lead` of it: further behind re-anchors instead of bursting.
    pub(super) fn delay(
        &mut self,
        now: Instant,
        media_ms: Option<i64>,
        lead: Duration,
    ) -> Duration {
        let Some(media_ms) = media_ms else {
            return Duration::ZERO;
        };
        let Some((anchor_wall, anchor_ms)) = self.anchor else {
            self.anchor = Some((now, media_ms));
            return Duration::ZERO;
        };
        let Ok(elapsed_ms) = u64::try_from(media_ms.saturating_sub(anchor_ms)) else {
            // Timestamps went back without a marked discontinuity.
            self.anchor = Some((now, media_ms));
            return Duration::ZERO;
        };
        let due = anchor_wall + Duration::from_millis(elapsed_ms);
        match due.checked_duration_since(now) {
            Some(wait) if wait > lead => {
                self.anchor = Some((now, media_ms));
                Duration::ZERO
            }
            Some(wait) => wait,
            None => {
                if now.saturating_duration_since(due) > lead {
                    self.anchor = Some((now, media_ms));
                }
                Duration::ZERO
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEAD: Duration = Duration::from_secs(2);

    #[test]
    fn media_is_due_at_its_offset_from_the_anchor() {
        let t0 = Instant::now();
        let mut pacer = Pacer::default();
        assert_eq!(pacer.delay(t0, None, LEAD), Duration::ZERO);
        assert_eq!(
            pacer.delay(t0, Some(5_000), LEAD),
            Duration::ZERO,
            "anchors"
        );
        assert_eq!(
            pacer.delay(t0, Some(5_400), LEAD),
            Duration::from_millis(400)
        );
        assert_eq!(
            pacer.delay(t0 + Duration::from_millis(400), Some(5_400), LEAD),
            Duration::ZERO
        );
        pacer.reset();
        assert_eq!(pacer.delay(t0, Some(0), LEAD), Duration::ZERO, "re-anchors");
        assert_eq!(pacer.delay(t0, Some(100), LEAD), Duration::from_millis(100));
    }

    #[test]
    fn a_wait_never_exceeds_the_lead_and_a_jump_re_anchors() {
        let t0 = Instant::now();
        let mut pacer = Pacer::default();
        pacer.delay(t0, Some(0), LEAD);
        assert_eq!(pacer.delay(t0, Some(60_000), LEAD), Duration::ZERO);
        assert_eq!(
            pacer.delay(t0, Some(60_500), LEAD),
            Duration::from_millis(500)
        );
        assert_eq!(
            pacer.delay(t0, Some(59_000), LEAD),
            Duration::ZERO,
            "went back"
        );
        assert_eq!(
            pacer.delay(t0, Some(59_300), LEAD),
            Duration::from_millis(300)
        );
    }

    /// Whatever the backlog, media published without waiting at one wall
    /// instant (a burst) stays within the lead.
    #[test]
    fn a_catch_up_burst_cannot_exceed_the_lead() {
        let t0 = Instant::now();
        for late_ms in [500u64, 1_900, 2_100, 60_000] {
            let mut pacer = Pacer::default();
            pacer.delay(t0, Some(0), LEAD);
            let now = t0 + Duration::from_millis(late_ms);
            let mut media = 0i64;
            let mut burst_start = None;
            while pacer.delay(now, Some(media), LEAD).is_zero() {
                burst_start.get_or_insert(media);
                media += 40;
                assert!(media < 100_000, "the burst never ends");
            }
            let burst = media - burst_start.unwrap_or(media);
            assert!(
                u128::try_from(burst).unwrap() <= LEAD.as_millis() + 40,
                "{late_ms} ms late: burst of {burst} ms"
            );
        }
    }
}
