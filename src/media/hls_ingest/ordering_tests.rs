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
    Payload::new(Bytes::copy_from_slice(tag.as_bytes()), None, None, None)
}

fn sized(len: usize) -> Payload {
    Payload::new(Bytes::from(vec![0u8; len]), None, None, None)
}

fn media(dir: &str, first: u64, len: u64, ended: bool) -> Playlist {
    media_with_map(dir, first, len, ended, None)
}

/// The same listing, every entry under `map` (the `EXT-X-MAP` in force).
fn media_with_map(dir: &str, first: u64, len: u64, ended: bool, map: Option<&str>) -> Playlist {
    let map = map.map(str::to_string);
    Playlist::Media(MediaPlaylist {
        target_duration: TARGET,
        media_sequence: first,
        segments: (first..first + len)
            .map(|sequence| SegmentEntry {
                sequence,
                path: format!("{dir}index{sequence}.ts"),
                duration: TARGET,
                discontinuity: false,
                map_uri: map.clone(),
            })
            .collect(),
        ended,
        map_uri: map,
        encryption: None,
        byte_range: false,
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
                audio_group: None,
            },
            Variant {
                bandwidth: 2_500_000,
                path: "high/index.m3u8".into(),
                audio_group: None,
            },
        ],
        audio: Vec::new(),
        session_key: None,
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

/// The listing with an `EXT-X-MAP` before every entry, as FFmpeg writes it.
fn with_map(playlist: Playlist, map: &str) -> Playlist {
    match playlist {
        Playlist::Media(mut media) => {
            media.map_uri = Some(map.to_string());
            for segment in &mut media.segments {
                segment.map_uri = Some(map.to_string());
            }
            Playlist::Media(media)
        }
        master => master,
    }
}

/// (sequence, payload, init payload, discontinuity) of what leaves now.
fn released_with_init(
    session: &mut Session,
    now: Instant,
) -> Vec<(u64, String, Option<String>, bool)> {
    let mut out = Vec::new();
    session.poll(now, &mut out);
    let text = |payload: &Payload| String::from_utf8(payload.bytes.to_vec()).unwrap();
    out.iter()
        .map(|release| {
            (
                release.sequence,
                text(&release.payload),
                release.init.as_ref().map(|init| text(&init.payload)),
                release.discontinuity,
            )
        })
        .collect()
}

/// FFmpeg uploads `init.mp4`, then the first segment, then the playlist
/// that names both. The init is held like any unlisted object, delivered
/// once with the first segment, and never released as a segment.
#[test]
fn an_init_uploaded_before_its_playlist_is_delivered_once_never_as_a_segment() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    assert_eq!(seg(&mut session, "init.mp4", t0), SegmentOutcome::Held);
    assert_eq!(seg(&mut session, "index0.ts", t0), SegmentOutcome::Held);
    session.on_playlist(
        "index.m3u8",
        with_map(media("", 0, 1, false), "init.mp4"),
        t0,
    );
    assert_eq!(session.map_uri(), Some("init.mp4"));
    assert_eq!(session.held_segments(), 0);
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(0, "index0.ts".into(), Some("init.mp4".into()), false)]
    );

    // A re-upload of the same init bytes is routed to the map, not held,
    // and changes nothing.
    assert_eq!(seg(&mut session, "init.mp4", t0), SegmentOutcome::Init);
    assert_eq!(seg(&mut session, "index1.ts", t0), SegmentOutcome::Held);
    session.on_playlist(
        "index.m3u8",
        with_map(media("", 0, 2, false), "init.mp4"),
        t0,
    );
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(1, "index1.ts".into(), None, false)]
    );
    assert_eq!(session.held_segments(), 0);
}

/// A listed segment whose init has not arrived waits (its upload got
/// `202`); the init releases it. Without the init it is skipped after a
/// target duration rather than piling up.
#[test]
fn a_segment_waits_for_its_init_then_is_released_with_it() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    assert_eq!(seg(&mut session, "index0.ts", t0), SegmentOutcome::Held);
    session.on_playlist(
        "index.m3u8",
        with_map(media("", 0, 2, false), "init.mp4"),
        t0,
    );
    assert!(released_with_init(&mut session, t0).is_empty());
    assert_eq!(session.next_deadline(), Some(t0 + TARGET));

    assert_eq!(seg(&mut session, "init.mp4", t0), SegmentOutcome::Init);
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(0, "index0.ts".into(), Some("init.mp4".into()), false)]
    );

    let mut orphan = Session::new(limits());
    seg(&mut orphan, "index0.ts", t0);
    orphan.on_playlist(
        "index.m3u8",
        with_map(media("", 0, 1, false), "init.mp4"),
        t0,
    );
    assert!(released_with_init(&mut orphan, t0 + TARGET / 2).is_empty());
    assert!(released_with_init(&mut orphan, t0 + TARGET).is_empty());
    assert_eq!(orphan.counters().skipped, 1);
    assert!(orphan.next_deadline().is_none());
}

/// The init the stream key applied before this session is delivered again,
/// without another upload: FFmpeg uploads its init once per run, and a
/// session that starts after an idle timeout still needs it.
#[test]
fn a_seeded_init_is_delivered_without_an_upload() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    session.seed_init("init.mp4", Bytes::from_static(b"init.mp4"));
    assert_eq!(seg(&mut session, "index0.ts", t0), SegmentOutcome::Held);
    session.on_playlist(
        "index.m3u8",
        with_map(media("", 0, 1, false), "init.mp4"),
        t0,
    );
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(0, "index0.ts".into(), Some("init.mp4".into()), false)],
        "the seeded init reaches the new session's demuxer"
    );

    // A playlist that names a different init does not get the seed.
    let mut other = Session::new(limits());
    other.seed_init("init.mp4", Bytes::from_static(b"init.mp4"));
    seg(&mut other, "index0.ts", t0);
    other.on_playlist(
        "index.m3u8",
        with_map(media("", 0, 1, false), "other.mp4"),
        t0,
    );
    assert!(released_with_init(&mut other, t0).is_empty());
    assert_eq!(other.counters().skipped, 0, "it waits, it is not skipped");
}

/// A playlist whose `EXT-X-MAP` changes mid-listing: each segment needs the
/// init that was in force for it, so the one listed before the change is
/// released without waiting for the new one.
#[test]
fn a_segment_needs_only_the_map_it_was_listed_under() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    seg(&mut session, "index0.ts", t0);
    seg(&mut session, "index1.ts", t0);

    let mut listing = match with_map(media("", 1, 1, false), "init2.mp4") {
        Playlist::Media(media) => media,
        master => panic!("media expected, got {master:?}"),
    };
    listing.media_sequence = 0;
    listing.segments.insert(
        0,
        SegmentEntry {
            sequence: 0,
            path: "index0.ts".into(),
            duration: TARGET,
            discontinuity: false,
            map_uri: None,
        },
    );
    session.on_playlist("index.m3u8", Playlist::Media(listing), t0);
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(0, "index0.ts".into(), None, false)],
        "no map yet for index0.ts: it does not wait"
    );

    assert_eq!(seg(&mut session, "init2.mp4", t0), SegmentOutcome::Init);
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(1, "index1.ts".into(), Some("init2.mp4".into()), false)]
    );
}

/// A voided map does not take the seed again when it is recreated: the
/// stale init cannot come back through a variant switch or an evicted entry.
#[test]
fn a_recreated_map_does_not_take_the_voided_seed_again() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    session.seed_init("init.mp4", Bytes::from_static(b"stale init"));
    let master = |bandwidth| {
        Playlist::Master(MasterPlaylist {
            variants: vec![Variant {
                bandwidth,
                path: format!("v{bandwidth}/index.m3u8"),
                audio_group: None,
            }],
            audio: Vec::new(),
            session_key: None,
        })
    };
    session.on_playlist("master.m3u8", master(800_000), t0);
    session.on_playlist(
        "v800000/index.m3u8",
        with_map(media("v800000/", 0, 1, false), "init.mp4"),
        t0,
    );
    assert!(
        session.init_ready(Some("init.mp4")),
        "the seed is delivered"
    );

    // A refused init upload voids the map...
    session.void_map("init.mp4");
    // ...and a variant switch forgets it entirely; the same URI comes back
    // with the new variant's playlist.
    session.on_playlist("master.m3u8", master(2_500_000), t0);
    session.on_playlist(
        "v2500000/index.m3u8",
        with_map(media("v2500000/", 0, 1, false), "init.mp4"),
        t0,
    );
    assert_eq!(
        seg(&mut session, "v2500000/index0.ts", t0),
        SegmentOutcome::Attached
    );
    assert!(
        released_with_init(&mut session, t0).is_empty(),
        "the recreated map must wait for a real init"
    );
    assert!(
        released_with_init(&mut session, t0 + TARGET).is_empty(),
        "and the stale seed must not come back"
    );
    assert_eq!(session.counters().skipped, 1);
}

/// A variant switch forgets the map of the variant it leaves: the new
/// variant's first fMP4 segment is normally uploaded before its playlist,
/// and the old variant's map must not refuse it as "no EXT-X-MAP".
#[test]
fn a_variant_switch_forgets_the_old_map_until_the_new_playlist_arrives() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    let master = |bandwidth| {
        Playlist::Master(MasterPlaylist {
            variants: vec![Variant {
                bandwidth,
                path: format!("v{bandwidth}/index.m3u8"),
                audio_group: None,
            }],
            audio: Vec::new(),
            session_key: None,
        })
    };
    // The first variant's playlist names an init.
    session.on_playlist("master.m3u8", master(800_000), t0);
    session.on_playlist(
        "v800000/index.m3u8",
        with_map(media("v800000/", 0, 1, false), "init.mp4"),
        t0,
    );
    assert_eq!(session.map_uri(), Some("init.mp4"));
    assert!(!session.map_required());

    // A higher-bandwidth variant appears: its segment arrives before its
    // playlist, so no map is known for it yet.
    session.on_playlist("master.m3u8", master(2_500_000), t0);
    assert_eq!(session.map_uri(), None);
    assert!(
        !session.map_required(),
        "the new variant's map is not known yet, so nothing is refused"
    );
    assert_eq!(
        seg(&mut session, "v2500000/index0.m4s", t0),
        SegmentOutcome::Held
    );

    // Its own playlist then names the map, as a fresh upload would.
    assert_eq!(
        session.on_playlist(
            "v2500000/index.m3u8",
            with_map(media("v2500000/", 0, 1, false), "init.mp4"),
            t0
        ),
        PlaylistOutcome::Accepted
    );
    assert!(!session.map_required());
    assert_eq!(session.map_uri(), Some("init.mp4"));
}

/// An encoder restart with a new `EXT-X-MAP` (new URI, or new bytes at the
/// same URI) delivers the new init with the next segment and flags the
/// discontinuity that resets the demuxer's timeline.
#[test]
fn a_new_init_is_delivered_with_a_discontinuity() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    seg(&mut session, "init.mp4", t0);
    seg(&mut session, "index0.ts", t0);
    session.on_playlist(
        "index.m3u8",
        with_map(media("", 0, 1, false), "init.mp4"),
        t0,
    );
    assert_eq!(released_with_init(&mut session, t0).len(), 1);

    // A new epoch whose playlist names a new map; its init was uploaded first.
    assert_eq!(seg(&mut session, "init2.mp4", t0), SegmentOutcome::Held);
    seg(&mut session, "v2/index0.ts", t0);
    session.on_playlist(
        "index.m3u8",
        with_map(media("v2/", 0, 1, false), "init2.mp4"),
        t0,
    );
    assert_eq!(session.counters().epochs, 1);
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(0, "v2/index0.ts".into(), Some("init2.mp4".into()), true)]
    );

    // Same URI, different bytes (`-hls_fmp4_init_resend` after a restart).
    let resent = Payload::new(Bytes::from_static(b"other init"), None, None, None);
    assert_eq!(
        session.on_segment("init2.mp4".to_string(), resent, t0),
        SegmentOutcome::Init
    );
    seg(&mut session, "v2/index1.ts", t0);
    session.on_playlist(
        "index.m3u8",
        with_map(media("v2/", 0, 2, false), "init2.mp4"),
        t0,
    );
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(1, "v2/index1.ts".into(), Some("other init".into()), true)]
    );
}

/// Going back to an earlier map delivers its init again. The demuxer holds one
/// init, so a stream that goes content map → ad map → content map — the shape
/// server-side ad insertion produces, and the shape the pull's docs name —
/// must deliver the content map's init again: without it, the content's
/// segments are demuxed with the ad map's tracks, `avcC` parameter sets and
/// AAC config, and a differing track id skips every sample.
#[test]
fn a_return_to_an_earlier_map_delivers_its_init_again() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    // The content map's init, uploaded before the playlist that names it.
    assert_eq!(seg(&mut session, "content.mp4", t0), SegmentOutcome::Held);
    assert_eq!(seg(&mut session, "index0.ts", t0), SegmentOutcome::Held);
    session.on_playlist(
        "index.m3u8",
        with_map(media("", 0, 1, false), "content.mp4"),
        t0,
    );
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(0, "index0.ts".into(), Some("content.mp4".into()), false)],
        "the first init replaces nothing"
    );

    // The ad break: a second map, its init uploaded before its playlist.
    assert_eq!(seg(&mut session, "ad.mp4", t0), SegmentOutcome::Held);
    assert_eq!(seg(&mut session, "index1.ts", t0), SegmentOutcome::Held);
    session.on_playlist("index.m3u8", with_map(media("", 1, 1, false), "ad.mp4"), t0);
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(1, "index1.ts".into(), Some("ad.mp4".into()), true)],
        "the ad map's init replaces the content map's"
    );

    // Back to the content map, with no re-upload of its init.
    assert_eq!(seg(&mut session, "index2.ts", t0), SegmentOutcome::Held);
    session.on_playlist(
        "index.m3u8",
        with_map(media("", 2, 1, false), "content.mp4"),
        t0,
    );
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(2, "index2.ts".into(), Some("content.mp4".into()), true)],
        "the content map's init is delivered again, as a replacement"
    );
}

/// The demuxer holds bytes, not URIs: the same init listed under a second map
/// is already in it, so the switch delivers nothing and flags no discontinuity.
/// A discontinuity re-anchors at what is a seamless boundary.
#[test]
fn the_same_init_under_a_second_map_is_not_delivered_again() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    assert_eq!(seg(&mut session, "content.mp4", t0), SegmentOutcome::Held);
    assert_eq!(seg(&mut session, "index0.ts", t0), SegmentOutcome::Held);
    session.on_playlist(
        "index.m3u8",
        with_map(media("", 0, 1, false), "content.mp4"),
        t0,
    );
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(0, "index0.ts".into(), Some("content.mp4".into()), false)]
    );

    // A second map whose bytes are the ones the demuxer already holds.
    assert_eq!(
        session.on_segment("ad.mp4".to_string(), payload("content.mp4"), t0),
        SegmentOutcome::Held
    );
    assert_eq!(seg(&mut session, "index1.ts", t0), SegmentOutcome::Held);
    session.on_playlist("index.m3u8", with_map(media("", 1, 1, false), "ad.mp4"), t0);
    assert_eq!(
        released_with_init(&mut session, t0),
        vec![(1, "index1.ts".into(), None, false)],
        "identical bytes need no delivery and are not a discontinuity"
    );
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
    Payload::new(Bytes::from(bytes), Some(permit), None, None)
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
