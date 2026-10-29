//! Tests for the generalized sink's counters and media clock.

use super::*;

/// The clock the window checks read: the first packet is a baseline (its
/// absolute timestamp is not progress), forward steps accumulate, and a
/// backwards step adds nothing instead of freezing the total.
#[test]
fn media_progress_counts_forward_steps_only() {
    let metrics = GeneralizedSinkMetrics::default();
    assert_eq!(metrics.summary()["mediaProgressMs"], 0);

    metrics.note_video_time(90_000); // an absolute offset, not progress
    assert_eq!(metrics.summary()["mediaProgressMs"], 0);
    assert_eq!(metrics.summary()["mediaTimestampMs"], 90_000);

    metrics.note_video_time(90_040);
    metrics.note_video_time(90_080);
    assert_eq!(metrics.summary()["mediaProgressMs"], 80);

    // A re-anchor: the timeline jumps back, then advances again.
    metrics.note_video_time(1_000);
    assert_eq!(metrics.summary()["mediaProgressMs"], 80);
    metrics.note_video_time(1_040);
    assert_eq!(metrics.summary()["mediaProgressMs"], 120);
}

/// A sequence header is configuration in both tag forms: the legacy
/// second-byte type, and enhanced RTMP's low nibble, where `0x90` is an
/// enhanced keyframe *sequence start* and not media.
#[test]
fn sequence_headers_are_recognized_in_both_tag_forms() {
    assert!(video_is_sequence_header(&[0x17, 0x00, 0x00]));
    assert!(video_is_sequence_header(&[0x90, b'h', b'v', b'c', b'1']));
    assert!(!video_is_sequence_header(&[0x17, 0x01, 0x00]));
    assert!(!video_is_sequence_header(&[0x91, b'h', b'v', b'c', b'1']));
    assert!(!video_is_sequence_header(&[0x27, 0x01, 0x00]));

    assert!(video_is_keyframe(0x17));
    assert!(video_is_keyframe(0x91));
    assert!(!video_is_keyframe(0x27));
    assert!(!video_is_keyframe(0xa1));
}
