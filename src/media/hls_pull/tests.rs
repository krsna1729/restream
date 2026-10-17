//! HLS pull ingest: the fetch plans, the parser's refusal rules and the
//! guards that are pure functions or state. What needs a real origin is in
//! `tests_origin.rs`, which also owns the log capture these tests borrow.

use std::time::Duration;

use super::fetch::redirect_refusal;
use super::tests_origin::{AUDIO_MASTER, CapturedMessages};
use super::*;
use crate::domain::pipeline_input::{HeaderPair, PullAuth, VariantRule};
use crate::media::hls_ingest::playlist;
use crate::media::hls_ingest::test_support::authenticated;
use tracing_subscriber::layer::SubscriberExt as _;

// ---- pure plans -----------------------------------------------------------

#[test]
fn refresh_cadence_halves_after_an_empty_reload() {
    let target = Duration::from_secs(6);
    assert_eq!(refresh_delay(target, true), Duration::from_secs(6));
    assert_eq!(refresh_delay(target, false), Duration::from_secs(3));
    assert_eq!(
        refresh_delay(Duration::from_secs(1), false),
        Duration::from_millis(500)
    );
}

#[test]
fn variant_choice_follows_both_rules() {
    let master = MasterPlaylist {
        variants: vec![
            Variant {
                bandwidth: 500_000,
                path: "low.m3u8".to_string(),
                audio_group: None,
            },
            Variant {
                bandwidth: 2_000_000,
                path: "mid.m3u8".to_string(),
                audio_group: None,
            },
            Variant {
                bandwidth: 2_000_000,
                path: "high.m3u8".to_string(),
                audio_group: None,
            },
        ],
        audio: Vec::new(),
        session_key: None,
    };
    assert_eq!(
        choose_variant(&master, VariantRule::HighestBandwidth)
            .unwrap()
            .path,
        "mid.m3u8",
        "the first of a tie wins"
    );
    assert_eq!(
        choose_variant(&master, VariantRule::LowestBandwidth)
            .unwrap()
            .path,
        "low.m3u8"
    );
    assert!(
        choose_variant(
            &MasterPlaylist {
                variants: vec![],
                audio: Vec::new(),
                session_key: None,
            },
            VariantRule::LowestBandwidth
        )
        .is_none()
    );
}

fn parsed_master(text: &str) -> MasterPlaylist {
    match playlist::parse_with(text.as_bytes(), |uri| {
        Some(format!("https://origin.example/live/{uri}"))
    }) {
        Ok(Playlist::Master(master)) => master,
        other => panic!("expected a master playlist, got {other:?}"),
    }
}

/// Report `master`'s audio plan and return every line it emitted.
fn reported(master: &MasterPlaylist) -> Vec<String> {
    let variant = master.highest_bandwidth().expect("one variant");
    let messages = CapturedMessages::default();
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(messages.clone()));
    let guard = tracing::dispatcher::set_default(&dispatch);
    report_audio_rendition(master, variant, &authenticated());
    drop(guard);
    messages.matching("HLS pull")
}

#[test]
fn audio_plan_states_what_the_chosen_variant_does_with_audio() {
    // No `AUDIO` attribute: the variant's own segments carry its audio.
    let master = parsed_master("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=2000000\nvideo/index.m3u8\n");
    let variant = master.highest_bandwidth().expect("one variant");
    assert_eq!(audio_plan(&master, variant), AudioPlan::Muxed);
    assert_eq!(audio_plan(&master, variant).reason(), None);

    // A group member with no `URI` is muxed into the variant as well.
    let master = parsed_master(
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Track 1\",DEFAULT=YES\n\
         #EXT-X-STREAM-INF:BANDWIDTH=2000000,AUDIO=\"audio\"\nvideo/index.m3u8\n",
    );
    let variant = master.highest_bandwidth().expect("one variant");
    assert_eq!(audio_plan(&master, variant), AudioPlan::Muxed);

    // A separate rendition: the group's `DEFAULT=YES` member, with its URI.
    let master = parsed_master(AUDIO_MASTER);
    let variant = master.highest_bandwidth().expect("one variant");
    let plan = audio_plan(&master, variant);
    assert_eq!(
        plan,
        AudioPlan::Separate {
            group: "audio".to_string(),
            name: "Track 1 (en)".to_string(),
            url: "https://origin.example/live/audio/0/index.m3u8".to_string(),
        }
    );
    let reason = plan.reason().expect("a separate rendition is stated");
    assert!(reason.contains("\"audio\""), "{reason}");
    assert!(reason.contains("Track 1 (en)"), "{reason}");
    assert!(reason.contains("video-only"), "{reason}");

    // A group the master does not define is a broken master, stated as such.
    let master = parsed_master(
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=2000000,AUDIO=\"missing\"\nvideo/index.m3u8\n",
    );
    let variant = master.highest_bandwidth().expect("one variant");
    let plan = audio_plan(&master, variant);
    assert_eq!(
        plan,
        AudioPlan::MissingGroup {
            group: "missing".to_string(),
        }
    );
    let reason = plan.reason().expect("a missing group is stated");
    assert!(reason.contains("\"missing\""), "{reason}");
    assert!(reason.contains("defines no audio rendition"), "{reason}");
}

#[test]
fn a_separate_audio_rendition_is_reported_once_naming_it() {
    let lines = reported(&parsed_master(AUDIO_MASTER));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("\"audio\""), "{}", lines[0]);
    assert!(lines[0].contains("Track 1 (en)"), "{}", lines[0]);
    assert!(lines[0].contains("video-only"), "{}", lines[0]);
}

#[test]
fn audio_the_variant_carries_itself_is_not_reported() {
    for text in [
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=2000000\nvideo/index.m3u8\n",
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Track 1\",DEFAULT=YES\n\
         #EXT-X-STREAM-INF:BANDWIDTH=2000000,AUDIO=\"audio\"\nvideo/index.m3u8\n",
    ] {
        let lines = reported(&parsed_master(text));
        assert!(
            lines.is_empty(),
            "nothing is lost, so nothing is logged: {lines:?}"
        );
    }
}

#[test]
fn a_variant_naming_a_missing_group_is_reported_with_its_reason() {
    let lines = reported(&parsed_master(
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=2000000,AUDIO=\"missing\"\nvideo/index.m3u8\n",
    ));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("\"missing\""), "{}", lines[0]);
    assert!(
        lines[0].contains("defines no audio rendition"),
        "{}",
        lines[0]
    );
}

#[test]
fn backoff_grows_and_caps() {
    assert_eq!(backoff(1), Duration::from_millis(200));
    assert_eq!(backoff(2), Duration::from_millis(400));
    assert_eq!(backoff(3), Duration::from_millis(800));
    assert_eq!(backoff(5), Duration::from_millis(3_200));
    assert_eq!(backoff(6), Duration::from_secs(5), "capped");
    assert_eq!(backoff(64), Duration::from_secs(5));
}

#[test]
fn a_remote_playlist_is_rewritten_into_the_push_namespace() {
    let base = Url::parse("https://origin.example/live/index.m3u8?token=abc").unwrap();
    let remote = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:7\n\
                  #EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2.002,\nseg7.m4s\n\
                  #EXT-X-DISCONTINUITY\n#EXTINF:1.5,\nseg8.m4s?part=2\n#EXT-X-ENDLIST\n";
    let Playlist::Media(media) = parse_remote(&base, remote.as_bytes()).unwrap() else {
        panic!("expected a media playlist");
    };
    assert_eq!(
        media.segments[0].path, "https://origin.example/live/seg7.m4s",
        "relative URIs resolve against the playlist URL"
    );
    assert_eq!(
        media.segments[1].path, "https://origin.example/live/seg8.m4s?part=2",
        "a signed query survives"
    );

    let text = push_playlist_text(&media);
    let local = playlist::parse(text.as_bytes(), PLAYLIST_PATH).unwrap();
    let Playlist::Media(local) = local else {
        panic!("the rewritten playlist parses as media");
    };
    assert_eq!(local.media_sequence, 7);
    assert_eq!(local.target_duration, Duration::from_secs(2));
    assert!(local.ended);
    let map = local
        .segments
        .first()
        .and_then(|segment| segment.map_uri.clone())
        .expect("the map survives");
    assert!(map.starts_with("pull/m") && map.ends_with(".mp4"));
    assert_eq!(
        local
            .segments
            .iter()
            .map(|segment| (
                segment.sequence,
                segment.path.as_str(),
                segment.discontinuity
            ))
            .collect::<Vec<_>>(),
        vec![(7, "pull/s7.m4s", false), (8, "pull/s8.m4s", true),]
    );
    assert_eq!(
        media.segments[0].map_uri.as_deref(),
        Some("https://origin.example/live/init.mp4")
    );
}

#[test]
fn auth_variants_validate_once_and_apply_their_scheme() {
    assert!(RequestAuth::new(&PullAuth::None).is_ok());
    assert!(
        RequestAuth::new(&PullAuth::Bearer {
            token: "a b".to_string()
        })
        .is_ok()
    );
    assert!(
        RequestAuth::new(&PullAuth::Bearer {
            token: "bad\nvalue".to_string()
        })
        .is_err()
    );
    assert!(
        RequestAuth::new(&PullAuth::Basic {
            username: "user".to_string(),
            password: "p@ss".to_string(),
        })
        .is_ok()
    );
    assert!(
        RequestAuth::new(&PullAuth::Headers {
            headers: vec![HeaderPair {
                name: "X-Token".to_string(),
                value: "secret".to_string(),
            }],
        })
        .is_ok()
    );
    assert!(
        RequestAuth::new(&PullAuth::Headers {
            headers: vec![HeaderPair {
                name: "bad header".to_string(),
                value: "secret".to_string(),
            }],
        })
        .is_err()
    );
    assert!(
        RequestAuth::new(&PullAuth::Cookies {
            cookies: "CloudFront-Policy=abc; CloudFront-Signature=def".to_string(),
        })
        .is_ok()
    );
    assert!(
        RequestAuth::new(&PullAuth::Cookies {
            cookies: "a=b\nCookie: c=d".to_string(),
        })
        .is_err(),
        "a line break would split the request"
    );
}

#[test]
fn a_live_window_is_entered_three_target_durations_from_its_end() {
    let base = Url::parse("https://origin.example/live/index.m3u8").unwrap();
    let window = |entries: usize| {
        let mut text =
            String::from("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:100\n");
        for offset in 0..entries {
            text.push_str(&format!("#EXTINF:1.0,\nseg{offset}.ts\n"));
        }
        match parse_remote(&base, text.as_bytes()).unwrap() {
            Playlist::Media(media) => media,
            other => panic!("expected a media playlist, got {other:?}"),
        }
    };
    assert_eq!(
        live_start(&window(10)),
        106,
        "the last entry at least three target durations from the end (RFC 8216 6.3.3)"
    );
    assert_eq!(
        live_start(&window(3)),
        100,
        "a window no longer than the horizon starts at its first entry"
    );
    assert_eq!(live_start(&window(0)), 100);
}

#[test]
fn a_resigned_map_uri_keeps_one_identity() {
    let first = map_name("https://cdn.example/live/init.mp4?hdnts=st=1~exp=2~acl=*");
    let resigned = map_name("https://cdn.example/live/init.mp4?hdnts=st=9~exp=10~acl=*");
    assert_eq!(
        first, resigned,
        "the query is the credential, not the map's identity"
    );
    assert_ne!(
        first,
        map_name("https://cdn.example/live/init2.mp4?hdnts=st=1~exp=2~acl=*"),
        "another path is another map"
    );
    assert_ne!(
        first,
        map_name("https://other.example/live/init.mp4?hdnts=st=1~exp=2~acl=*"),
        "another host is another map"
    );
    assert_ne!(
        first,
        map_name("http://cdn.example/live/init.mp4?hdnts=st=1~exp=2~acl=*"),
        "another scheme is another map"
    );
}

/// Every case goes through a URL, because that is how the guard is called: an
/// IPv6 literal arrives only from `Url::host`, and `host_str` hands it back
/// bracketed.
fn refused(url: &str) -> bool {
    host_refused(&Url::parse(url).expect("a test URL parses"))
}

#[test]
fn private_and_link_local_hosts_are_refused() {
    for url in [
        "http://127.0.0.1/s.ts",
        "http://127.1.2.3/s.ts",
        "http://0.0.0.0/s.ts",
        "http://10.1.2.3/s.ts",
        "http://172.16.0.1/s.ts",
        "http://172.31.255.254/s.ts",
        "http://192.168.1.1/s.ts",
        "http://169.254.169.254/s.ts",
        "http://[::1]/s.ts",
        "http://[fe80::1]/s.ts",
        "http://[fd00::1]/s.ts",
        "http://[fd00:ec2::254]/s.ts",
        "http://[::ffff:127.0.0.1]/s.ts",
        "http://[::ffff:169.254.169.254]/s.ts",
        "http://localhost/s.ts",
        "http://LOCALHOST/s.ts",
    ] {
        assert!(refused(url), "{url} is a refused range");
    }
    for url in [
        "http://93.184.216.34/s.ts",
        "http://172.32.0.1/s.ts",
        "http://8.8.8.8/s.ts",
        "http://[2606:4700::1111]/s.ts",
        "http://origin.example/s.ts",
    ] {
        assert!(!refused(url), "{url} is routable");
    }
}

#[test]
fn a_playlist_pointing_at_a_private_address_is_refused() {
    let base = Url::parse("https://origin.example/live/index.m3u8").unwrap();
    let text = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1.0,\n\
                http://169.254.169.254/latest/meta-data/\n";
    let parsed = parse_remote(&base, text.as_bytes()).unwrap();
    match refuse_private_targets(&parsed, false) {
        Err(PullError::Fatal(reason)) => {
            assert!(reason.contains("169.254.169.254"), "{reason}");
            assert!(reason.contains("private address"), "{reason}");
        }
        other => panic!("a private target is refused, got {other:?}"),
    }
    assert!(
        refuse_private_targets(&parsed, true).is_ok(),
        "a source that is itself private may point there"
    );

    // A map URI is a target too.
    let text = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n\
                #EXT-X-MAP:URI=\"http://10.0.0.5/init.mp4\"\n#EXTINF:1.0,\nseg0.m4s\n";
    let parsed = parse_remote(&base, text.as_bytes()).unwrap();
    assert!(refuse_private_targets(&parsed, false).is_err());

    // An IPv6 literal is a target like any other, and its brackets are not a
    // reason to miss it.
    let text = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1.0,\nhttp://[fd00::1]/s.ts\n";
    let parsed = parse_remote(&base, text.as_bytes()).unwrap();
    match refuse_private_targets(&parsed, false) {
        Err(PullError::Fatal(reason)) => assert!(reason.contains("fd00::1"), "{reason}"),
        other => panic!("an IPv6 private target is refused, got {other:?}"),
    }
}

#[test]
fn a_redirect_out_of_the_origin_or_to_the_clear_is_refused() {
    let source = Url::parse("https://origin.example/live/index.m3u8").unwrap();
    let same_origin = Url::parse("https://origin.example/live/other.m3u8").unwrap();
    assert_eq!(
        redirect_refusal(&source, &same_origin, true),
        None,
        "a redirect inside the origin is followed"
    );
    assert_eq!(
        redirect_refusal(
            &source,
            &Url::parse("https://cdn.example/live/x.m3u8").unwrap(),
            true
        ),
        Some("redirect leaves the source origin")
    );
    assert_eq!(
        redirect_refusal(
            &source,
            &Url::parse("https://origin.example:8443/x.m3u8").unwrap(),
            false
        ),
        Some("redirect leaves the source origin"),
        "another port is another origin"
    );
    let downgrade = Url::parse("http://origin.example/live/other.m3u8").unwrap();
    assert_eq!(
        redirect_refusal(&source, &downgrade, true),
        Some("redirect would send credentials over http")
    );
    // Without credentials, an https→http hop on the same host and port is
    // still another origin (RFC 6454), so it is not followed either.
    let secure = Url::parse("https://origin.example:8080/x.m3u8").unwrap();
    let clear = Url::parse("http://origin.example:8080/x.m3u8").unwrap();
    assert_eq!(
        redirect_refusal(&secure, &clear, false),
        Some("redirect leaves the source origin")
    );
}
