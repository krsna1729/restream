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
    /// While still: the picture looks drawn (bars, a card), so it is
    /// likely a slate rather than a frozen camera.
    #[serde(default)]
    pub still_is_slate: bool,
    /// The last sound measured; absent until 3 s of one track decode.
    pub sound_checked_ms: Option<u64>,
    /// The first track's silence and loudness (also `tracks[0]`).
    pub silent_since_ms: Option<u64>,
    /// Short-term loudness (3 s), LUFS, floored at -70.
    pub loudness_lufs: Option<f64>,
    /// Every audio track checked, in source order.
    #[serde(default)]
    pub tracks: Vec<TrackSound>,
}

/// One audio track's sound.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackSound {
    pub silent_since_ms: Option<u64>,
    pub loudness_lufs: f64,
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

    /// Each track after the first that is silent now: (index, how long).
    pub fn other_silent_tracks(&self, now_ms: u64) -> Vec<SilentTrack> {
        self.tracks
            .iter()
            .enumerate()
            .skip(1)
            .filter_map(|(track, sound)| {
                Self::held(sound.silent_since_ms, self.sound_checked_ms, now_ms).map(
                    |silent_for_ms| SilentTrack {
                        track,
                        silent_for_ms,
                    },
                )
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SilentTrack {
    /// 0-based, in source order.
    pub track: usize,
    pub silent_for_ms: u64,
}

/// What the alert rules read: how long each condition has held now.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QaHeld {
    pub black_for_ms: Option<u64>,
    pub still_for_ms: Option<u64>,
    pub still_is_slate: bool,
    pub silent_for_ms: Option<u64>,
    pub loudness_lufs: Option<f64>,
    /// Silent tracks after the first.
    pub silent_tracks: Vec<SilentTrack>,
}

impl StreamQa {
    pub fn held_at(&self, now_ms: u64) -> QaHeld {
        QaHeld {
            black_for_ms: self.black_for_ms(now_ms),
            still_for_ms: self.still_for_ms(now_ms),
            still_is_slate: self.still_is_slate,
            silent_for_ms: self.silent_for_ms(now_ms),
            loudness_lufs: self.loudness_lufs,
            silent_tracks: self.other_silent_tracks(now_ms),
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
            self.qa.still_is_slate = false;
        } else {
            self.qa.still_since_ms = Some(self.last_change_ms);
            self.qa.still_is_slate = picture::looks_like_slate(&thumb);
        }
        self.last_thumb = Some(thumb);
        self.qa.picture_checked_ms = Some(at_ms);
    }

    /// The source has this many audio tracks now; readings past it are
    /// dropped with the tracks.
    pub fn set_track_count(&mut self, tracks: usize) {
        self.qa.tracks.resize_with(tracks, TrackSound::default);
    }

    /// One short-term loudness reading at `at_ms` for each track from
    /// `first` on, in source order.
    pub fn sound(&mut self, at_ms: u64, first: usize, loudness_lufs: &[f64]) {
        let end = first.saturating_add(loudness_lufs.len());
        if self.qa.tracks.len() < end {
            self.qa.tracks.resize_with(end, TrackSound::default);
        }
        for (track, &lufs) in self.qa.tracks[first..end].iter_mut().zip(loudness_lufs) {
            track.silent_since_ms = if lufs <= SILENT_BELOW_LUFS {
                Some(track.silent_since_ms.unwrap_or(at_ms))
            } else {
                None
            };
            track.loudness_lufs = lufs;
        }
        if first == 0 {
            let lead = self.qa.tracks.first();
            self.qa.silent_since_ms = lead.and_then(|track| track.silent_since_ms);
            self.qa.loudness_lufs = lead.map(|track| track.loudness_lufs);
        }
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

        tracker.sound(1_000, 0, &[-70.0]);
        tracker.sound(2_000, 0, &[-65.0]);
        assert_eq!(tracker.snapshot().silent_since_ms, Some(1_000));
        assert_eq!(tracker.snapshot().silent_for_ms(2_500), Some(1_500));
        tracker.sound(3_000, 0, &[-23.0]);
        assert_eq!(tracker.snapshot().silent_since_ms, None);
        assert_eq!(tracker.snapshot().loudness_lufs, Some(-23.0));
    }

    #[test]
    fn a_still_slate_is_told_from_a_still_picture() {
        let mut tracker = QaTracker::default();
        let slate = thumb(128);
        tracker.picture(1_000, slate.clone());
        tracker.picture(2_000, slate);
        assert!(tracker.snapshot().still_is_slate, "a flat card held still");
        // A textured picture held still is a frozen picture.
        let textured: Vec<u8> = (0..slate_len()).map(|i| (i * 37 % 251) as u8).collect();
        tracker.picture(3_000, textured.clone());
        assert!(
            !tracker.snapshot().still_is_slate,
            "changed: not still at all"
        );
        tracker.picture(4_000, textured);
        assert_eq!(tracker.snapshot().still_since_ms, Some(3_000));
        assert!(!tracker.snapshot().still_is_slate);
    }

    fn slate_len() -> usize {
        (picture::THUMB_WIDTH * picture::THUMB_HEIGHT) as usize
    }

    #[test]
    fn every_track_is_measured_and_a_silent_second_track_is_named() {
        let mut tracker = QaTracker::default();
        tracker.sound(1_000, 0, &[-23.0, -70.0, -20.0]);
        tracker.sound(2_000, 0, &[-22.0, -68.0, -61.0]);
        let qa = tracker.snapshot();
        assert_eq!(qa.loudness_lufs, Some(-22.0), "the first track leads");
        assert_eq!(qa.silent_since_ms, None);
        assert_eq!(
            qa.other_silent_tracks(3_000),
            [
                SilentTrack {
                    track: 1,
                    silent_for_ms: 2_000
                },
                SilentTrack {
                    track: 2,
                    silent_for_ms: 1_000
                },
            ]
        );
        tracker.sound(3_000, 0, &[-22.0, -18.0, -61.0]);
        assert_eq!(
            tracker.snapshot().other_silent_tracks(3_000),
            [SilentTrack {
                track: 2,
                silent_for_ms: 1_000
            }]
        );
        // A later group reports its own tracks.
        tracker.sound(3_500, 40, &[-70.0]);
        assert_eq!(tracker.snapshot().tracks.len(), 41);
        assert_eq!(
            tracker.snapshot().loudness_lufs,
            Some(-22.0),
            "the first track still leads"
        );
        // A source with fewer tracks drops the rest.
        tracker.set_track_count(1);
        tracker.sound(4_000, 0, &[-70.0]);
        assert_eq!(tracker.snapshot().tracks.len(), 1);
        assert_eq!(tracker.snapshot().silent_since_ms, Some(4_000));
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
