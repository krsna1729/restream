//! A variant and its separate audio rendition followed together: each in its
//! own sequence space, sharing the hold.

use std::time::Instant;

use bytes::Bytes;

use super::super::tests::{media, with_map};
use super::super::*;
use crate::hls_ingest::playlist::{AudioRendition, MasterPlaylist, Playlist, Variant};

fn limits() -> SessionLimits {
    SessionLimits {
        hold_segments: 8,
        hold_bytes: 10_000,
        idle_floor: IDLE_FLOOR,
    }
}

fn seg(session: &mut Session, name: &str, now: Instant) -> SegmentOutcome {
    let payload = Payload::new(Bytes::copy_from_slice(name.as_bytes()), None, None, None);
    session.on_segment(name.to_string(), payload, now)
}

/// Every release, as (rendition, sequence, bytes, carries an init).
fn released(session: &mut Session, now: Instant) -> Vec<(usize, u64, String, bool)> {
    let mut out = Vec::new();
    session.poll(now, &mut out);
    out.into_iter()
        .map(|release| {
            (
                release.rendition,
                release.sequence,
                String::from_utf8(release.payload.bytes.to_vec()).unwrap(),
                release.init.is_some(),
            )
        })
        .collect()
}

fn audio(name: &str, default: bool, path: Option<&str>) -> AudioRendition {
    AudioRendition {
        group_id: "aud".into(),
        name: name.into(),
        default,
        autoselect: default,
        path: path.map(str::to_string),
    }
}

/// Two variants of group `aud`, whose default member is `audio/` (a separate
/// playlist) and whose other member is `audio-fr/`.
fn master(default_audio: Option<&str>) -> Playlist {
    let variant = |bandwidth, path: &str| Variant {
        bandwidth,
        path: path.into(),
        audio_group: Some("aud".into()),
        codecs: Vec::new(),
    };
    Playlist::Master(MasterPlaylist {
        variants: vec![
            variant(800_000, "low/index.m3u8"),
            variant(2_500_000, "high/index.m3u8"),
        ],
        audio: vec![
            audio("fr", false, Some("audio-fr/index.m3u8")),
            audio("en", true, default_audio),
        ],
        session_key: None,
    })
}

#[test]
fn a_separate_audio_rendition_is_followed_with_its_variant() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    // Segments arrive before the playlists that list them, in both
    // renditions, with the same sequence numbers.
    for name in ["high/index0.ts", "audio/index0.ts", "audio-fr/index0.ts"] {
        assert_eq!(seg(&mut session, name, t0), SegmentOutcome::Held);
    }
    session.on_playlist("master.m3u8", master(Some("audio/index.m3u8")), t0);
    assert_eq!(session.renditions(), 2);
    for (path, outcome) in [
        ("high/index.m3u8", PlaylistOutcome::Accepted),
        ("audio/index.m3u8", PlaylistOutcome::Accepted),
        ("low/index.m3u8", PlaylistOutcome::OtherVariant),
        ("audio-fr/index.m3u8", PlaylistOutcome::OtherVariant),
    ] {
        let dir = path.trim_end_matches("index.m3u8");
        assert_eq!(
            session.on_playlist(path, media(dir, 0, 2, false), t0),
            outcome,
            "{path}"
        );
    }
    assert_eq!(
        seg(&mut session, "audio-fr/index1.ts", t0),
        SegmentOutcome::OtherVariant,
        "the group's other member is not followed"
    );
    assert_eq!(
        seg(&mut session, "audio/index1.ts", t0),
        SegmentOutcome::Attached
    );
    assert_eq!(
        seg(&mut session, "high/index1.ts", t0),
        SegmentOutcome::Attached
    );

    let mut releases = released(&mut session, t0);
    releases.sort();
    assert_eq!(
        releases,
        vec![
            (0, 0, "high/index0.ts".into(), false),
            (0, 1, "high/index1.ts".into(), false),
            (1, 0, "audio/index0.ts".into(), false),
            (1, 1, "audio/index1.ts".into(), false),
        ]
    );
    assert!(session.counters().duplicates == 0 && session.counters().skipped == 0);

    // The session ends once both renditions have ended.
    session.on_playlist("high/index.m3u8", media("high/", 0, 2, true), t0);
    assert!(!session.finished(), "the audio rendition has not ended");
    session.on_playlist("audio/index.m3u8", media("audio/", 0, 2, true), t0);
    assert!(session.finished());
}

/// Each rendition's `EXT-X-MAP` init goes to its own demuxer: both are
/// delivered, neither replaces the other (no discontinuity), and a seeded
/// init returns to the rendition that applied it.
#[test]
fn each_rendition_delivers_its_own_init() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    session.seed_init(1, "audio/init.mp4", Bytes::from_static(b"audio init"));
    session.on_playlist("master.m3u8", master(Some("audio/index.m3u8")), t0);
    seg(&mut session, "high/init.mp4", t0);
    session.on_playlist(
        "high/index.m3u8",
        with_map(media("high/", 0, 1, false), "high/init.mp4"),
        t0,
    );
    session.on_playlist(
        "audio/index.m3u8",
        with_map(media("audio/", 0, 1, false), "audio/init.mp4"),
        t0,
    );
    seg(&mut session, "high/index0.ts", t0);
    seg(&mut session, "audio/index0.ts", t0);

    let mut out = Vec::new();
    session.poll(t0, &mut out);
    let mut seen: Vec<(usize, String, bool)> = out
        .iter()
        .map(|release| {
            let init = release
                .init
                .as_ref()
                .expect("each first segment brings its init");
            (
                release.rendition,
                String::from_utf8(init.payload.bytes.to_vec()).unwrap(),
                !matches!(release.reanchor, Reanchor::None),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            (0, "high/init.mp4".into(), false),
            (1, "audio init".into(), false),
        ]
    );
}

/// A variant whose group's member has no URI carries its own audio: one
/// rendition, as without a group.
#[test]
fn muxed_audio_is_one_rendition() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    session.on_playlist("master.m3u8", master(None), t0);
    assert_eq!(session.renditions(), 1);
    assert_eq!(
        session.on_playlist("audio-fr/index.m3u8", media("audio-fr/", 0, 1, false), t0),
        PlaylistOutcome::OtherVariant,
        "the default member is muxed; the other member is not followed"
    );
}

/// FFmpeg writes its master after every variant's first playlist, so the
/// audio rendition's playlist can be followed first, as rendition 0. When the
/// master names it the audio rendition it moves to rendition 1 whole: its
/// listing stays, and the init its encoder uploaded once is delivered again
/// to rendition 1's demuxer instead of being lost.
#[test]
fn a_rendition_followed_before_its_master_keeps_its_init() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    seg(&mut session, "audio/init.mp4", t0);
    seg(&mut session, "audio/index0.ts", t0);
    session.on_playlist(
        "audio/index.m3u8",
        with_map(media("audio/", 0, 2, false), "audio/init.mp4"),
        t0,
    );
    let first = released(&mut session, t0);
    assert_eq!(
        first,
        vec![(0, 0, "audio/index0.ts".into(), true)],
        "before the master the audio playlist is all there is"
    );
    seg(&mut session, "high/init.mp4", t0);

    session.on_playlist("master.m3u8", master(Some("audio/index.m3u8")), t0);
    session.on_playlist(
        "high/index.m3u8",
        with_map(media("high/", 0, 1, false), "high/init.mp4"),
        t0,
    );
    seg(&mut session, "high/index0.ts", t0);
    seg(&mut session, "audio/index1.ts", t0);

    let mut releases = released(&mut session, t0);
    releases.sort();
    assert_eq!(
        releases,
        vec![
            (0, 0, "high/index0.ts".into(), true),
            (1, 1, "audio/index1.ts".into(), true),
        ],
        "the audio continues at its next sequence, its init delivered to its new demuxer"
    );
}

/// Mark one segment of a listing with `EXT-X-DISCONTINUITY`, as an SSAI ad
/// break does in both the variant's and the audio rendition's playlist.
fn with_discontinuity(mut playlist: Playlist, sequence: u64) -> Playlist {
    if let Playlist::Media(media) = &mut playlist {
        for segment in &mut media.segments {
            if segment.sequence == sequence {
                segment.discontinuity = true;
            }
        }
    }
    playlist
}

/// The reanchor mark of every release, as (rendition, sequence, mark).
fn reanchors(session: &mut Session, now: Instant) -> Vec<(usize, u64, Reanchor)> {
    let mut out = Vec::new();
    session.poll(now, &mut out);
    out.iter()
        .map(|release| (release.rendition, release.sequence, release.reanchor))
        .collect()
}

/// `EXT-X-DISCONTINUITY` on the same entry of both playlists is one timeline
/// generation: each rendition counts it, so the publisher sees the same
/// generation on both.
#[test]
fn a_timeline_break_on_both_renditions_counts_the_same_generation() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    session.on_playlist("master.m3u8", master(Some("audio/index.m3u8")), t0);
    for name in [
        "high/index0.ts",
        "high/index1.ts",
        "high/index2.ts",
        "audio/index0.ts",
        "audio/index1.ts",
        "audio/index2.ts",
    ] {
        seg(&mut session, name, t0);
    }
    // An SSAI break marked on the same entry of both playlists.
    session.on_playlist(
        "high/index.m3u8",
        with_discontinuity(media("high/", 0, 3, false), 1),
        t0,
    );
    session.on_playlist(
        "audio/index.m3u8",
        with_discontinuity(media("audio/", 0, 3, false), 1),
        t0,
    );

    let marks = reanchors(&mut session, t0);
    assert_eq!(
        marks,
        vec![
            (0, 0, Reanchor::None),
            (0, 1, Reanchor::Timeline(1)),
            (0, 2, Reanchor::None),
            (1, 0, Reanchor::None),
            (1, 1, Reanchor::Timeline(1)),
            (1, 2, Reanchor::None),
        ],
        "the same break is generation 1 on both renditions"
    );
}

/// A variant switch re-anchors rendition 0 but keeps the clock: its next
/// release is a `Clock` mark, not a new generation. The audio rendition's
/// playlist is unchanged, so its generation stays where it was.
#[test]
fn a_variant_switch_keeps_the_clock() {
    let t0 = Instant::now();
    let mut session = Session::new(limits());
    let variant = |bandwidth, path: &str| Variant {
        bandwidth,
        path: path.into(),
        audio_group: Some("aud".into()),
        codecs: Vec::new(),
    };
    let same_audio = |path: &str| {
        Playlist::Master(MasterPlaylist {
            variants: vec![variant(2_500_000, path)],
            audio: vec![audio("en", true, Some("audio/index.m3u8"))],
            session_key: None,
        })
    };
    session.on_playlist("master.m3u8", same_audio("high/index.m3u8"), t0);
    for name in ["high/index0.ts", "high/index1.ts", "audio/index0.ts"] {
        seg(&mut session, name, t0);
    }
    session.on_playlist("high/index.m3u8", media("high/", 0, 2, false), t0);
    session.on_playlist("audio/index.m3u8", media("audio/", 0, 1, false), t0);
    assert!(
        reanchors(&mut session, t0)
            .iter()
            .all(|(_, _, mark)| *mark == Reanchor::None)
    );

    // The master selects another variant on the same clock.
    session.on_playlist("master.m3u8", same_audio("alt/index.m3u8"), t0);
    seg(&mut session, "alt/index0.ts", t0);
    session.on_playlist("alt/index.m3u8", media("alt/", 0, 1, false), t0);
    assert_eq!(
        reanchors(&mut session, t0),
        vec![(0, 0, Reanchor::Clock)],
        "the switched variant re-anchors without a new generation"
    );
}
