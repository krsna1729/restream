//! Picture and sound checks on a pipeline's source: black picture, a
//! picture that stopped changing, silence, and loudness. They are opt in
//! per pipeline (`stream_qa_enabled:{id}`).
//!
//! A sampler ([`runner`]) reads the source ring like any other consumer,
//! on its own OS thread: it decodes at most one keyframe a second and all
//! of one audio track, and publishes a [`StreamQa`] snapshot. A reader
//! that falls behind is moved forward by the ring and the sampler starts
//! again from the next keyframe; the writer and the outputs never wait for
//! it. The flight recorder samples the snapshot for Stream health, and the
//! alert rules read it for incidents.

pub mod loudness;
pub mod picture;
pub mod runner;

use serde::{Deserialize, Serialize};

/// A black picture this long is an alert.
pub const BLACK_ALERT_MS: u64 = 10_000;
/// A picture unchanged this long is an alert (a slate or a still is meant
/// to be still; the alert says so).
pub const STILL_ALERT_MS: u64 = 30_000;
/// Silence this long is an alert.
pub const SILENT_ALERT_MS: u64 = 10_000;
/// Short-term loudness at or below this is silence.
pub const SILENT_BELOW_LUFS: f64 = -60.0;
/// A check older than this says nothing about now.
pub const QA_STALE_MS: u64 = 10_000;

/// The checks' latest findings. Times are wall-clock epoch milliseconds;
/// a `*_since_ms` is set while the condition holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamQa {
    /// The last picture looked at; absent until one decodes.
    pub picture_checked_ms: Option<u64>,
    pub black_since_ms: Option<u64>,
    /// When the picture last changed, while it has not since.
    pub still_since_ms: Option<u64>,
    /// The last sound measured; absent until 3 s of one track decode.
    pub sound_checked_ms: Option<u64>,
    pub silent_since_ms: Option<u64>,
    /// Short-term loudness (3 s), LUFS, floored at -70.
    pub loudness_lufs: Option<f64>,
}

impl StreamQa {
    /// How long a condition has held at `now_ms`, if it does and its check
    /// is current.
    fn held(since_ms: Option<u64>, checked_ms: Option<u64>, now_ms: u64) -> Option<u64> {
        let checked = checked_ms?;
        if now_ms.saturating_sub(checked) > QA_STALE_MS {
            return None;
        }
        since_ms.map(|since| now_ms.saturating_sub(since))
    }

    pub fn black_for_ms(&self, now_ms: u64) -> Option<u64> {
        Self::held(self.black_since_ms, self.picture_checked_ms, now_ms)
    }

    pub fn still_for_ms(&self, now_ms: u64) -> Option<u64> {
        Self::held(self.still_since_ms, self.picture_checked_ms, now_ms)
    }

    pub fn silent_for_ms(&self, now_ms: u64) -> Option<u64> {
        Self::held(self.silent_since_ms, self.sound_checked_ms, now_ms)
    }
}

/// What the alert rules read: how long each condition has held now.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QaHeld {
    pub black_for_ms: Option<u64>,
    pub still_for_ms: Option<u64>,
    pub silent_for_ms: Option<u64>,
    pub loudness_lufs: Option<f64>,
}

impl StreamQa {
    pub fn held_at(&self, now_ms: u64) -> QaHeld {
        QaHeld {
            black_for_ms: self.black_for_ms(now_ms),
            still_for_ms: self.still_for_ms(now_ms),
            silent_for_ms: self.silent_for_ms(now_ms),
            loudness_lufs: self.loudness_lufs,
        }
    }
}

/// Turns checks into findings: each picture against the last, each
/// loudness reading against the silence line.
#[derive(Debug, Default)]
pub struct QaTracker {
    qa: StreamQa,
    last_thumb: Option<Vec<u8>>,
    last_change_ms: u64,
}

impl QaTracker {
    pub fn snapshot(&self) -> &StreamQa {
        &self.qa
    }

    /// One picture, as a grey thumbnail, seen at `at_ms`.
    pub fn picture(&mut self, at_ms: u64, thumb: Vec<u8>) {
        let black = picture::is_black(&thumb);
        self.qa.black_since_ms = if black {
            Some(self.qa.black_since_ms.unwrap_or(at_ms))
        } else {
            None
        };
        let changed = self
            .last_thumb
            .as_deref()
            .is_none_or(|last| picture::differs(last, &thumb));
        if changed {
            self.last_change_ms = at_ms;
            self.qa.still_since_ms = None;
        } else {
            self.qa.still_since_ms = Some(self.last_change_ms);
        }
        self.last_thumb = Some(thumb);
        self.qa.picture_checked_ms = Some(at_ms);
    }

    /// One short-term loudness reading at `at_ms`.
    pub fn sound(&mut self, at_ms: u64, loudness_lufs: f64) {
        self.qa.silent_since_ms = if loudness_lufs <= SILENT_BELOW_LUFS {
            Some(self.qa.silent_since_ms.unwrap_or(at_ms))
        } else {
            None
        };
        self.qa.loudness_lufs = Some(loudness_lufs);
        self.qa.sound_checked_ms = Some(at_ms);
    }

    /// The source changed under the sampler (a reconnect, a skipped
    /// stretch): the next picture is compared with nothing.
    pub fn forget_picture(&mut self) {
        self.last_thumb = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thumb(level: u8) -> Vec<u8> {
        vec![level; (picture::THUMB_WIDTH * picture::THUMB_HEIGHT) as usize]
    }

    #[test]
    fn a_still_picture_is_dated_from_its_last_change_and_clears_on_the_next() {
        let mut tracker = QaTracker::default();
        tracker.picture(1_000, thumb(100));
        assert_eq!(
            tracker.snapshot().still_since_ms,
            None,
            "nothing to compare yet"
        );
        tracker.picture(2_000, thumb(120));
        tracker.picture(3_000, thumb(120));
        tracker.picture(4_000, thumb(120));
        assert_eq!(tracker.snapshot().still_since_ms, Some(2_000));
        assert_eq!(tracker.snapshot().still_for_ms(9_000), Some(7_000));
        tracker.picture(5_000, thumb(60));
        assert_eq!(tracker.snapshot().still_since_ms, None);
        // After a gap in the source, the next picture starts afresh.
        tracker.forget_picture();
        tracker.picture(6_000, thumb(60));
        assert_eq!(tracker.snapshot().still_since_ms, None);
    }

    #[test]
    fn black_and_silence_hold_from_their_first_sighting() {
        let mut tracker = QaTracker::default();
        tracker.picture(1_000, thumb(10));
        tracker.picture(3_000, thumb(12));
        assert_eq!(tracker.snapshot().black_since_ms, Some(1_000));
        tracker.picture(4_000, thumb(200));
        assert_eq!(tracker.snapshot().black_since_ms, None);

        tracker.sound(1_000, -70.0);
        tracker.sound(2_000, -65.0);
        assert_eq!(tracker.snapshot().silent_since_ms, Some(1_000));
        assert_eq!(tracker.snapshot().silent_for_ms(2_500), Some(1_500));
        tracker.sound(3_000, -23.0);
        assert_eq!(tracker.snapshot().silent_since_ms, None);
        assert_eq!(tracker.snapshot().loudness_lufs, Some(-23.0));
    }

    #[test]
    fn a_finding_whose_check_went_stale_says_nothing() {
        let mut tracker = QaTracker::default();
        tracker.picture(1_000, thumb(10));
        assert_eq!(tracker.snapshot().black_for_ms(5_000), Some(4_000));
        assert_eq!(
            tracker.snapshot().black_for_ms(1_000 + QA_STALE_MS + 1),
            None
        );
        assert_eq!(
            tracker.snapshot().silent_for_ms(5_000),
            None,
            "no sound checked"
        );
    }
}
