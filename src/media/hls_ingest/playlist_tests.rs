//! The playlist parser's tests: media and master playlists, URI resolution,
//! and the tags the ingest reads or refuses.

use super::*;
use proptest::prelude::*;

fn media(text: &str) -> MediaPlaylist {
    match parse(text.as_bytes(), "live/index.m3u8") {
        Ok(Playlist::Media(playlist)) => playlist,
        other => panic!("expected media playlist, got {other:?}"),
    }
}

/// The tags that say the media cannot be fetched whole and in the clear
/// are recorded as data. Push ignores them, as it always has; the pull is
/// what refuses a playlist that carries one.
#[test]
fn encryption_and_byte_ranges_are_recorded_as_data() {
    let playlist = media(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n\
         #EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://key\",IV=0x1\n\
         #EXTINF:2.0,\nindex0.ts\n",
    );
    assert_eq!(
        playlist.encryption,
        Some(Encryption {
            method: "SAMPLE-AES".to_string(),
            session: false,
        }),
        "push still parses an encrypted playlist, and knows its method"
    );
    assert!(!playlist.byte_range);

    // `METHOD=NONE` clears the key for the segments listed after it, but
    // it does not un-encrypt the entry listed before it: that one is
    // under AES-128, and the record says so.
    let playlist = media(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n\
         #EXTINF:2.0,\nindex0.ts\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:2.0,\nindex1.ts\n",
    );
    assert_eq!(
        playlist.encryption,
        Some(Encryption {
            method: "AES-128".to_string(),
            session: false,
        }),
        "an encrypted entry anywhere in the list is recorded"
    );

    // The first method a listed entry was under wins, so the reason names
    // the key that entry actually used.
    let playlist = media(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"a\"\n\
         #EXTINF:2.0,\nindex0.ts\n#EXT-X-KEY:METHOD=AES-128,URI=\"b\"\n\
         #EXTINF:2.0,\nindex1.ts\n",
    );
    assert_eq!(
        playlist.encryption,
        Some(Encryption {
            method: "SAMPLE-AES".to_string(),
            session: false,
        }),
        "the first method listed with a segment is the one recorded"
    );

    // A key that is never in force where an entry is listed is not an
    // encrypted playlist: `METHOD=NONE` first, then no key at all.
    let playlist = media(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=NONE\n\
         #EXTINF:2.0,\nindex0.ts\n",
    );
    assert_eq!(playlist.encryption, None);

    let playlist = media(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-BYTERANGE:1200@0\n\
         #EXTINF:2.0,\nindex0.ts\n",
    );
    assert!(playlist.byte_range);
    assert_eq!(playlist.encryption, None);

    // A map's own `BYTERANGE` is a slice too.
    let playlist = media(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"800@0\"\n\
         #EXTINF:2.0,\nindex0.m4s\n",
    );
    assert!(playlist.byte_range);

    // A master's session key is recorded, and a master stays a master.
    let master = match parse(
        "#EXTM3U\n#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"key\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=800000\nlow/index.m3u8\n"
            .as_bytes(),
        "live/index.m3u8",
    ) {
        Ok(Playlist::Master(master)) => master,
        other => panic!("expected a master playlist, got {other:?}"),
    };
    assert_eq!(
        master.session_key,
        Some(Encryption {
            method: "AES-128".to_string(),
            session: true,
        })
    );

    // A tag the RFC cannot mean anything without is recorded all the
    // same, never refused: push ignored both before, and a parse error
    // here would change what push accepts.
    let playlist = media(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:URI=\"key\"\n#EXTINF:2.0,\nindex0.ts\n",
    );
    assert_eq!(
        playlist.encryption,
        Some(Encryption {
            method: String::new(),
            session: false,
        }),
        "a key tag naming no METHOD is still a key"
    );
    let playlist =
        media("#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-BYTERANGE:\n#EXTINF:2.0,\nindex0.ts\n");
    assert!(playlist.byte_range);
}

#[test]
fn media_playlist_lists_segments_by_sequence_resolved_to_the_playlist_directory() {
    let playlist = media(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:7\n\
         #EXTINF:2.002,\nindex7.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:1.5,title\n\
         ../other/index8.ts?token=1\n#EXT-X-ENDLIST\n",
    );
    assert_eq!(playlist.target_duration, Duration::from_secs(2));
    assert_eq!(playlist.media_sequence, 7);
    assert!(playlist.ended);
    assert_eq!(
        playlist.segments,
        vec![
            SegmentEntry {
                sequence: 7,
                path: "live/index7.ts".into(),
                duration: Duration::from_secs_f64(2.002),
                discontinuity: false,
                map_uri: None,
            },
            SegmentEntry {
                sequence: 8,
                path: "other/index8.ts".into(),
                duration: Duration::from_secs_f64(1.5),
                discontinuity: true,
                map_uri: None,
            },
        ]
    );
}

#[test]
fn master_playlist_variants_carry_bandwidth_and_resolved_paths() {
    let text = "#EXTM3U\r\n#EXT-X-STREAM-INF:CODECS=\"avc1.64001f,mp4a.40.2\",BANDWIDTH=800000\r\n\
                low/index.m3u8\r\n#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720\r\n\
                high/index.m3u8\r\n";
    let Ok(Playlist::Master(master)) = parse(text.as_bytes(), "master.m3u8") else {
        panic!("expected master playlist");
    };
    assert_eq!(master.variants.len(), 2);
    assert_eq!(
        master.highest_bandwidth(),
        Some(&Variant {
            bandwidth: 2_500_000,
            path: "high/index.m3u8".into(),
            audio_group: None,
            codecs: Vec::new(),
        })
    );
    assert_eq!(master.variants[0].path, "low/index.m3u8");
    assert_eq!(
        master.lowest_bandwidth(),
        Some(&Variant {
            bandwidth: 800_000,
            path: "low/index.m3u8".into(),
            audio_group: None,
            codecs: vec!["avc1.64001f".into(), "mp4a.40.2".into()],
        })
    );
}

/// A master whose audio is a group of `EXT-X-MEDIA` renditions, as
/// packaged live HLS writes it: one rendition per language, each with its
/// own playlist URI, and the variant naming the group.
#[test]
fn master_playlist_reads_the_audio_group_and_the_variant_that_names_it() {
    let text = "#EXTM3U\n\
        #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Track 1 (en)\",LANGUAGE=\"en\",\
        DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"2\",URI=\"audio/en/index.m3u8\"\n\
        #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Track 2 (de)\",LANGUAGE=\"de\",\
        DEFAULT=NO,AUTOSELECT=YES,URI=\"audio/de/index.m3u8\"\n\
        #EXT-X-STREAM-INF:BANDWIDTH=2500000,AUDIO=\"audio\"\n\
        video/index.m3u8\n";
    let Ok(Playlist::Master(master)) = parse(text.as_bytes(), "master.m3u8") else {
        panic!("expected master playlist");
    };
    assert_eq!(
        master.audio,
        vec![
            AudioRendition {
                group_id: "audio".into(),
                name: "Track 1 (en)".into(),
                default: true,
                autoselect: true,
                path: Some("audio/en/index.m3u8".into()),
            },
            AudioRendition {
                group_id: "audio".into(),
                name: "Track 2 (de)".into(),
                default: false,
                autoselect: true,
                path: Some("audio/de/index.m3u8".into()),
            },
        ]
    );
    let variant = master.highest_bandwidth().expect("one variant");
    assert_eq!(variant.audio_group.as_deref(), Some("audio"));
    assert_eq!(
        master
            .audio_rendition(variant)
            .map(|audio| audio.name.as_str()),
        Some("Track 1 (en)"),
        "DEFAULT=YES wins"
    );
}

#[test]
fn audio_rendition_selection_prefers_default_then_autoselect_then_the_first() {
    let rendition = |name: &str, default: bool, autoselect: bool| AudioRendition {
        group_id: "audio".into(),
        name: name.into(),
        default,
        autoselect,
        path: Some(format!("audio/{name}.m3u8")),
    };
    let variant = Variant {
        bandwidth: 1,
        path: "video/index.m3u8".into(),
        audio_group: Some("audio".into()),
        codecs: Vec::new(),
    };
    let chosen = |audio: Vec<AudioRendition>| {
        let master = MasterPlaylist {
            variants: vec![variant.clone()],
            audio,
            session_key: None,
        };
        master
            .audio_rendition(&variant)
            .map(|audio| audio.name.clone())
    };

    assert_eq!(
        chosen(vec![
            rendition("first", false, false),
            rendition("default", true, false),
            rendition("autoselect", false, true),
        ])
        .as_deref(),
        Some("default"),
        "DEFAULT=YES outranks AUTOSELECT=YES and the first listed"
    );
    assert_eq!(
        chosen(vec![
            rendition("first", false, false),
            rendition("autoselect", false, true),
        ])
        .as_deref(),
        Some("autoselect"),
        "AUTOSELECT=YES outranks the first listed"
    );
    assert_eq!(
        chosen(vec![
            rendition("first", false, false),
            rendition("second", false, false),
        ])
        .as_deref(),
        Some("first"),
        "the first listed wins when neither flag is set"
    );
    assert_eq!(
        chosen(vec![
            rendition("first-default", true, false),
            rendition("second-default", true, false),
        ])
        .as_deref(),
        Some("first-default"),
        "the first of a tie wins"
    );
    assert_eq!(
        chosen(vec![]),
        None,
        "a group with no member selects nothing"
    );
    // A variant that names no group has no rendition to select.
    let no_group = MasterPlaylist {
        variants: vec![Variant {
            bandwidth: 1,
            path: "video/index.m3u8".into(),
            audio_group: None,
            codecs: Vec::new(),
        }],
        audio: vec![rendition("audio", true, true)],
        session_key: None,
    };
    assert_eq!(no_group.audio_rendition(&no_group.variants[0]), None);
}

#[test]
fn audio_renditions_without_a_uri_are_muxed_into_the_variant() {
    // `URI` is optional: without it the group's audio is muxed into the
    // variants that name the group, so there is no playlist to follow.
    let text = "#EXTM3U\n\
        #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Track 1\",DEFAULT=YES\n\
        #EXT-X-STREAM-INF:BANDWIDTH=100,AUDIO=\"audio\"\n\
        video/index.m3u8\n";
    let Ok(Playlist::Master(master)) = parse(text.as_bytes(), "master.m3u8") else {
        panic!("expected master playlist");
    };
    assert_eq!(master.audio.len(), 1);
    assert_eq!(master.audio[0].path, None);
    assert_eq!(
        master
            .audio_rendition(master.highest_bandwidth().expect("one variant"))
            .and_then(|audio| audio.path.as_deref()),
        None
    );
}

#[test]
fn non_audio_media_renditions_are_ignored() {
    let text = "#EXTM3U\n\
        #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",\
        URI=\"subs/en/index.m3u8\"\n\
        #EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",NAME=\"CC1\"\n\
        #EXT-X-STREAM-INF:BANDWIDTH=100\n\
        video/index.m3u8\n";
    let Ok(Playlist::Master(master)) = parse(text.as_bytes(), "master.m3u8") else {
        panic!("expected master playlist");
    };
    assert!(master.audio.is_empty(), "{:?}", master.audio);
}

#[test]
fn a_malformed_audio_rendition_is_an_error() {
    // `GROUP-ID` is required on `EXT-X-MEDIA`.
    assert_eq!(
        parse(
            b"#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,NAME=\"Track 1\",URI=\"a.m3u8\"\n\
              #EXT-X-STREAM-INF:BANDWIDTH=1\nvideo/index.m3u8\n",
            "master.m3u8"
        ),
        Err(PlaylistError::InvalidTag {
            line: 2,
            tag: "EXT-X-MEDIA",
        })
    );
    // A rendition URI resolves like any other: it may not leave the
    // namespace.
    assert_eq!(
        parse(
            b"#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",URI=\"../../a.m3u8\"\n\
              #EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"audio\"\nvideo/index.m3u8\n",
            "master.m3u8"
        ),
        Err(PlaylistError::InvalidUri { line: 2 })
    );
}

#[test]
fn unknown_tags_and_comments_are_ignored() {
    let playlist = media(
        "#EXTM3U\n# a comment\n#EXT-X-TARGETDURATION:4\n#EXT-X-PROGRAM-DATE-TIME:2026-01-01T00:00:00Z\n\
         #EXT-X-UNKNOWN:x\n#EXTINF:4,\nseg.ts\n",
    );
    assert_eq!(playlist.segments.len(), 1);
    assert!(!playlist.ended);
}

#[test]
fn map_tag_resolves_the_init_uri_against_the_playlist_directory() {
    let playlist = media(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\"\n\
         #EXTINF:2,\nindex7.ts\n",
    );
    assert_eq!(playlist.map_uri.as_deref(), Some("live/init.mp4"));

    let playlist = media("#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nindex7.ts\n");
    assert_eq!(playlist.map_uri, None);
}

#[test]
fn later_map_tag_overwrites_an_earlier_one() {
    let playlist = media(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"a/init.mp4\"\n\
         #EXTINF:2,\nindex7.ts\n#EXT-X-MAP:URI=\"b/init2.mp4\"\n#EXTINF:2,\nindex8.ts\n",
    );
    assert_eq!(playlist.map_uri.as_deref(), Some("live/b/init2.mp4"));
    // A mid-listing change is recorded per segment: index7.ts belongs to
    // the map that was in force when it was listed.
    let maps: Vec<Option<&str>> = playlist
        .segments
        .iter()
        .map(|segment| segment.map_uri.as_deref())
        .collect();
    assert_eq!(
        maps,
        vec![Some("live/a/init.mp4"), Some("live/b/init2.mp4")]
    );
}

#[test]
fn malformed_map_tags_are_errors() {
    assert_eq!(
        parse(
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:BYTERANGE=\"100@200\"\n\
              #EXTINF:2,\nindex7.ts\n",
            "live/index.m3u8"
        ),
        Err(PlaylistError::InvalidTag {
            line: 3,
            tag: "EXT-X-MAP",
        })
    );
    assert_eq!(
        parse(
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"../../init.mp4\"\n\
              #EXTINF:2,\nindex7.ts\n",
            "live/index.m3u8"
        ),
        Err(PlaylistError::InvalidUri { line: 3 })
    );
}

#[test]
fn malformed_playlists_are_errors() {
    let path = "index.m3u8";
    let cases: [(&[u8], PlaylistError); 9] = [
        (b"\xff\xfe", PlaylistError::NotUtf8),
        (b"#EXTINF:2,\na.ts\n", PlaylistError::MissingHeader),
        (
            b"#EXTM3U\n#EXTINF:2,\na.ts\n",
            PlaylistError::MissingTargetDuration,
        ),
        (
            b"#EXTM3U\n#EXT-X-TARGETDURATION:x\n",
            PlaylistError::InvalidTag {
                line: 2,
                tag: "EXT-X-TARGETDURATION",
            },
        ),
        (
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:-1,\na.ts\n",
            PlaylistError::InvalidTag {
                line: 3,
                tag: "EXTINF",
            },
        ),
        (
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\na.ts\n",
            PlaylistError::SegmentWithoutDuration { line: 3 },
        ),
        (
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\n../../a.ts\n",
            PlaylistError::InvalidUri { line: 4 },
        ),
        (
            b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\n",
            PlaylistError::VariantWithoutUri,
        ),
        (
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:18446744073709551615\n\
              #EXTINF:2,\na.ts\n#EXTINF:2,\nb.ts\n",
            PlaylistError::SequenceOverflow,
        ),
    ];
    for (bytes, expected) in cases {
        assert_eq!(parse(bytes, path), Err(expected));
    }
    assert_eq!(
        parse(
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nhttp://cdn/a.ts\n",
            path
        ),
        Err(PlaylistError::InvalidUri { line: 4 })
    );
    assert_eq!(
        parse(
            b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-STREAM-INF:BANDWIDTH=1\nv.m3u8\n",
            path
        ),
        Err(PlaylistError::MixedMasterAndMedia)
    );
}

#[test]
fn object_paths_normalize_inside_the_key_namespace() {
    assert_eq!(normalize_path("a/./b//c.ts").as_deref(), Some("a/b/c.ts"));
    assert_eq!(normalize_path("a/../b.ts").as_deref(), Some("b.ts"));
    assert_eq!(normalize_path("../b.ts"), None);
    assert_eq!(normalize_path("a\\b.ts"), None);
    assert_eq!(normalize_path("a\0.ts"), None);
    assert_eq!(normalize_path("/"), None);
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = parse(&bytes, "v/index.m3u8");
    }

    #[test]
    fn arbitrary_tag_soup_never_panics(
        lines in proptest::collection::vec(
            prop_oneof![
                Just("#EXTM3U".to_string()),
                "#EXT-X-TARGETDURATION:[-0-9.e]{0,8}",
                "#EXT-X-MEDIA-SEQUENCE:[0-9]{0,22}",
                "#EXTINF:[-0-9.,a-z]{0,10}",
                Just("#EXT-X-DISCONTINUITY".to_string()),
                Just("#EXT-X-ENDLIST".to_string()),
                "#EXT-X-STREAM-INF:[A-Z=0-9,\"]{0,24}",
                "#EXT-X-MEDIA:[A-Z=0-9,\"/.]{0,32}",
                "#EXT-X-MAP:[A-Z=0-9,\"]{0,24}",
                "[./a-z0-9?#]{0,12}",
            ],
            0..40,
        )
    ) {
        let _ = parse(lines.join("\n").as_bytes(), "a/b/index.m3u8");
    }
}
