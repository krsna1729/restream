use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use proptest::prelude::*;
use tokio::sync::Semaphore;

use super::*;
use crate::media::hls_ingest::playlist::{
    self, MasterPlaylist, MediaPlaylist, Playlist, SegmentEntry, Variant,
};

const TARGET: Duration = Duration::from_secs(2);

fn limits() -> SessionLimits {
    SessionLimits {
        hold_segments: 4,
        hold_bytes: 1_000,
        idle_floor: IDLE_FLOOR,
    }
}

fn payload(tag: &str) -> Payload {
    Payload::new(Bytes::copy_from_slice(tag.as_bytes()), None)
}

fn sized(len: usize) -> Payload {
    Payload::new(Bytes::from(vec![0u8; len]), None)
}

fn media(dir: &str, first: u64, len: u64, ended: bool) -> Playlist {
    Playlist::Media(MediaPlaylist {
        target_duration: TARGET,
        media_sequence: first,
        segments: (first..first + len)
            .map(|sequence| SegmentEntry {
                sequence,
                path: format!("{dir}index{sequence}.ts"),
                duration: TARGET,
                discontinuity: false,
            })
            .collect(),
        ended,
    })
}

fn released(session: &mut Session, now: Instant) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    session.poll(now, &mut out);
    out.into_iter()
        .map(|release| {
            (
                release.sequence,
                String::from_utf8(release.payload.bytes.to_vec()).unwrap(),
            )
        })
        .collect()
}

fn seg(session: &mut Session, name: &str, now: Instant) -> SegmentOutcome {
    session.on_segment(name.to_string(), payload(name), now)
}

#[test]
fn early_segments_are_held_then_released_in_media_sequence_order() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    assert_eq!(seg(&mut session, "index1.ts", t0), SegmentOutcome::Held);
    assert_eq!(seg(&mut session, "index0.ts", t0), SegmentOutcome::Held);
    assert!(released(&mut session, t0).is_empty(), "nothing listed yet");

    let outcome = session.on_playlist("index.m3u8", media("", 0, 3, false), t0);
    assert_eq!(outcome, PlaylistOutcome::Accepted);
    assert_eq!(
        released(&mut session, t0),
        vec![(0, "index0.ts".into()), (1, "index1.ts".into())]
    );
    assert_eq!(seg(&mut session, "index2.ts", t0), SegmentOutcome::Attached);
    assert_eq!(released(&mut session, t0), vec![(2, "index2.ts".into())]);
    assert_eq!(session.held_segments(), 0);
}

#[test]
fn a_duplicate_sequence_is_never_released_twice() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    session.on_playlist("index.m3u8", media("", 0, 2, false), t0);
    assert_eq!(seg(&mut session, "index1.ts", t0), SegmentOutcome::Attached);
    assert_eq!(
        seg(&mut session, "index1.ts", t0),
        SegmentOutcome::Duplicate
    );
    assert_eq!(seg(&mut session, "index0.ts", t0), SegmentOutcome::Attached);
    assert_eq!(released(&mut session, t0).len(), 2);

    // A re-upload of a released sequence is held until the next playlist
    // shows it is the same epoch's sequence, then discarded.
    assert_eq!(seg(&mut session, "index0.ts", t0), SegmentOutcome::Held);
    session.on_playlist("index.m3u8", media("", 0, 3, false), t0);
    assert_eq!(session.held_segments(), 0);
    assert!(released(&mut session, t0).is_empty());
    assert_eq!(session.counters().duplicates, 2);
}

#[test]
fn a_gap_is_skipped_after_one_target_duration_once_later_bytes_wait() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    session.on_playlist("index.m3u8", media("", 0, 3, false), t0);
    seg(&mut session, "index1.ts", t0);
    seg(&mut session, "index2.ts", t0);
    assert_eq!(session.next_deadline(), Some(t0 + TARGET));
    assert!(released(&mut session, t0 + TARGET - Duration::from_millis(1)).is_empty());
    assert_eq!(
        released(&mut session, t0 + TARGET),
        vec![(1, "index1.ts".into()), (2, "index2.ts".into())]
    );
    assert_eq!(session.counters().skipped, 1);
}

#[test]
fn a_missing_live_edge_segment_is_awaited_not_skipped() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    session.on_playlist("index.m3u8", media("", 0, 1, false), t0);
    assert_eq!(session.next_deadline(), None);
    assert!(released(&mut session, t0 + TARGET * 10).is_empty());
    assert_eq!(seg(&mut session, "index0.ts", t0), SegmentOutcome::Attached);
    assert_eq!(released(&mut session, t0 + TARGET * 10).len(), 1);
    assert_eq!(session.counters().skipped, 0);
}

#[test]
fn sequences_that_slid_out_of_the_window_are_skipped_and_counted() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    seg(&mut session, "index0.ts", t0);
    session.on_playlist("index.m3u8", media("", 0, 1, false), t0);
    assert_eq!(released(&mut session, t0).len(), 1);
    seg(&mut session, "index3.ts", t0);
    session.on_playlist("index.m3u8", media("", 3, 1, false), t0);
    assert_eq!(released(&mut session, t0), vec![(3, "index3.ts".into())]);
    assert_eq!(session.counters().skipped, 2);
}

#[test]
fn a_master_playlist_selects_the_highest_bandwidth_variant() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    seg(&mut session, "low/index0.ts", t0);
    seg(&mut session, "high/index0.ts", t0);
    let master = Playlist::Master(MasterPlaylist {
        variants: vec![
            Variant {
                bandwidth: 800_000,
                path: "low/index.m3u8".into(),
            },
            Variant {
                bandwidth: 2_500_000,
                path: "high/index.m3u8".into(),
            },
        ],
    });
    assert_eq!(
        session.on_playlist("master.m3u8", master, t0),
        PlaylistOutcome::Accepted
    );
    assert_eq!(
        session.on_playlist("low/index.m3u8", media("low/", 0, 1, false), t0),
        PlaylistOutcome::OtherVariant
    );
    assert_eq!(
        session.held_segments(),
        1,
        "the low variant's hold is purged"
    );
    assert_eq!(
        seg(&mut session, "low/index1.ts", t0),
        SegmentOutcome::Held,
        "not listed by the low playlist yet"
    );
    session.on_playlist("low/index.m3u8", media("low/", 0, 2, false), t0);
    assert_eq!(
        seg(&mut session, "low/index1.ts", t0),
        SegmentOutcome::OtherVariant
    );
    assert_eq!(
        session.on_playlist("high/index.m3u8", media("high/", 0, 1, false), t0),
        PlaylistOutcome::Accepted
    );
    assert_eq!(
        released(&mut session, t0),
        vec![(0, "high/index0.ts".into())]
    );
}

#[test]
fn without_a_master_the_first_media_playlist_is_followed() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    assert_eq!(
        session.on_playlist("a/index.m3u8", media("a/", 0, 1, false), t0),
        PlaylistOutcome::Accepted
    );
    assert_eq!(
        session.on_playlist("b/index.m3u8", media("b/", 0, 1, false), t0),
        PlaylistOutcome::OtherVariant
    );
    assert_eq!(
        seg(&mut session, "b/index0.ts", t0),
        SegmentOutcome::OtherVariant
    );
    assert_eq!(
        seg(&mut session, "a/index0.ts", t0),
        SegmentOutcome::Attached
    );
}

#[test]
fn endlist_finishes_the_session_after_the_last_listed_segment() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    let text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nindex0.ts\n#EXTINF:2,\nindex1.ts\n#EXT-X-ENDLIST\n";
    let parsed = playlist::parse(text.as_bytes(), "index.m3u8").unwrap();
    seg(&mut session, "index0.ts", t0);
    session.on_playlist("index.m3u8", parsed, t0);
    assert_eq!(released(&mut session, t0).len(), 1);
    assert!(!session.finished());
    seg(&mut session, "index1.ts", t0);
    assert_eq!(released(&mut session, t0).len(), 1);
    assert!(session.finished());
}

#[test]
fn the_hold_evicts_the_oldest_beyond_its_count_bytes_and_age() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    for index in 0..5 {
        session.on_segment(format!("c{index}.ts"), sized(10), t0);
    }
    assert_eq!(session.held_segments(), 4);
    session.on_segment("big.ts".into(), sized(995), t0);
    assert_eq!(session.held_segments(), 1, "the byte bound evicts the rest");
    assert!(session.held_bytes() <= limits().hold_bytes);
    assert_eq!(session.counters().evicted, 5);

    assert_eq!(session.next_deadline(), Some(t0 + IDLE_FLOOR));
    released(&mut session, t0 + IDLE_FLOOR);
    assert_eq!(
        session.held_segments(),
        0,
        "held past the idle floor ages out"
    );
}

#[test]
fn the_idle_timeout_is_three_targets_with_a_ten_second_floor() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    assert_eq!(session.idle_timeout(), Duration::from_secs(10));
    session.on_playlist("index.m3u8", media("", 0, 1, false), t0);
    assert_eq!(session.idle_timeout(), Duration::from_secs(10));
    let mut long = media("", 1, 1, false);
    if let Playlist::Media(media) = &mut long {
        media.target_duration = Duration::from_secs(6);
    }
    session.on_playlist("index.m3u8", long, t0);
    assert_eq!(session.idle_timeout(), Duration::from_secs(18));
}

#[test]
fn a_restarted_encoder_inside_the_idle_window_starts_a_new_epoch() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    session.on_playlist("index.m3u8", media("", 0, 5, false), t0);
    for sequence in 0..5 {
        seg(&mut session, &format!("index{sequence}.ts"), t0);
    }
    assert_eq!(released(&mut session, t0).len(), 5);

    // FFmpeg restarts names at index0.ts and the media sequence at 0. The
    // segment matches a released sequence of the old epoch: it is held, not
    // dropped, and the new epoch's first playlist must release it.
    assert_eq!(seg(&mut session, "index0.ts", t0), SegmentOutcome::Held);
    session.on_playlist("index.m3u8", media("", 0, 1, false), t0);
    let mut out = Vec::new();
    session.poll(t0, &mut out);
    assert_eq!(out.len(), 1, "the restarted index0.ts is released");
    assert_eq!((out[0].sequence, out[0].epoch), (0, 1));
    assert!(out[0].discontinuity);
    assert_eq!(session.counters().epochs, 1);
    assert_eq!(session.counters().duplicates, 0);

    // The next segment continues the new epoch without a discontinuity.
    seg(&mut session, "index1.ts", t0);
    session.on_playlist("index.m3u8", media("", 0, 2, false), t0);
    out.clear();
    session.poll(t0, &mut out);
    assert_eq!(out.len(), 1);
    assert!(!out[0].discontinuity);
}

#[test]
fn a_resent_identical_playlist_is_not_a_new_epoch() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    seg(&mut session, "index0.ts", t0);
    session.on_playlist("index.m3u8", media("", 0, 2, false), t0);
    released(&mut session, t0);
    session.on_playlist("index.m3u8", media("", 0, 2, false), t0);
    assert_eq!(session.counters().epochs, 0);
    assert_eq!(seg(&mut session, "index1.ts", t0), SegmentOutcome::Attached);
}

#[derive(Debug, Clone)]
enum Op {
    Segment { index: u64, len: usize },
    Playlist { first: u64, len: u64, ended: bool },
    Advance(u64),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0u64..12, 1usize..400).prop_map(|(index, len)| Op::Segment { index, len }),
        3 => (0u64..12, 0u64..6, proptest::bool::weighted(0.1))
            .prop_map(|(first, len, ended)| Op::Playlist { first, len, ended }),
        2 => (0u64..5_000).prop_map(Op::Advance),
    ]
}

/// Payload bytes name the segment index they belong to.
fn tagged(index: u64, len: usize, budget: &Arc<Semaphore>) -> Payload {
    let mut bytes = format!("{index}:").into_bytes();
    bytes.resize(len.max(bytes.len()), b'.');
    let permits = u32::try_from(bytes.len()).unwrap();
    let permit = budget.clone().try_acquire_many_owned(permits).unwrap();
    Payload::new(Bytes::from(bytes), Some(permit))
}

fn index_of(payload: &Payload) -> u64 {
    let text = std::str::from_utf8(&payload.bytes).unwrap();
    text.split(':').next().unwrap().parse().unwrap()
}

proptest! {
    /// Arbitrary interleavings of uploads (early, late, duplicate), sliding
    /// playlists (one that moves back is an encoder restart) and time. Model: what leaves is a subset
    /// of the listed sequences, ascending within an epoch, each (epoch,
    /// sequence) at most once, always the bytes of the name listed at that
    /// sequence; the hold stays inside its bounds; every byte of budget
    /// returns once the payloads are gone.
    #[test]
    fn releases_follow_the_listing_model(ops in proptest::collection::vec(op(), 1..80)) {
        const BUDGET: usize = 1 << 20;
        let budget = Arc::new(Semaphore::new(BUDGET));
        let t0 = Instant::now();
        let mut now = t0;
        let mut session = Session::new(limits());
        let mut listed = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut last_by_epoch: Option<(u64, u64)> = None;
        let mut out = Vec::new();
        for op in ops {
            match op {
                Op::Segment { index, len } => {
                    let payload = tagged(index, len, &budget);
                    session.on_segment(format!("index{index}.ts"), payload, now);
                }
                Op::Playlist { first, len, ended } => {
                    listed.extend(first..first + len);
                    session.on_playlist("index.m3u8", media("", first, len, ended), now);
                }
                Op::Advance(ms) => now += Duration::from_millis(ms),
            }
            session.poll(now, &mut out);
            for release in out.drain(..) {
                prop_assert!(listed.contains(&release.sequence));
                prop_assert_eq!(index_of(&release.payload), release.sequence);
                prop_assert!(seen.insert((release.epoch, release.sequence)));
                if let Some((epoch, sequence)) = last_by_epoch {
                    prop_assert!(release.epoch >= epoch);
                    if release.epoch == epoch {
                        prop_assert!(release.sequence > sequence);
                    }
                }
                last_by_epoch = Some((release.epoch, release.sequence));
            }
            prop_assert!(session.held_segments() <= limits().hold_segments);
            prop_assert!(session.held_bytes() <= limits().hold_bytes);
        }
        drop(session);
        prop_assert_eq!(budget.available_permits(), BUDGET);
    }

    /// A well-behaved encoder (each segment, then the playlist listing it)
    /// with retried uploads and resent playlists loses nothing and repeats
    /// nothing.
    #[test]
    fn a_sequential_encoder_with_retries_releases_every_segment_once(
        count in 1u64..30,
        window in 1u64..6,
        retries in proptest::collection::vec(proptest::bool::weighted(0.3), 30),
    ) {
        let t0 = Instant::now();
        let mut session = Session::new(limits());
        let mut out = Vec::new();
        for sequence in 0..count {
            let now = t0 + TARGET * u32::try_from(sequence).unwrap();
            let first = (sequence + 1).saturating_sub(window);
            let retry = retries[usize::try_from(sequence).unwrap()];
            for _ in 0..=u8::from(retry) {
                seg(&mut session, &format!("index{sequence}.ts"), now);
                session.on_playlist("index.m3u8", media("", first, sequence + 1 - first, false), now);
            }
            session.poll(now, &mut out);
        }
        let sequences: Vec<u64> = out.iter().map(|release| release.sequence).collect();
        prop_assert_eq!(sequences, (0..count).collect::<Vec<_>>());
        prop_assert_eq!(session.counters().skipped, 0);
        prop_assert_eq!(session.counters().epochs, 0);
    }
}
