//! HLS pull ingest: the fetch plans, the parser's refusal rules and the
//! guards that are pure functions or state. What needs a real origin is in
//! `tests_origin.rs`, which also owns the log capture these tests borrow.

use std::time::Duration;

use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use super::fetch::{log_url, redirect_refusal};
use super::tests_origin::{AUDIO_MASTER, CapturedMessages, source};
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
    // A hostile playlist cannot set the cadence: a near-zero target duration
    // is polled at the floor, not as fast as round-trip time allows.
    assert_eq!(
        refresh_delay(Duration::from_millis(1), true),
        MIN_REFRESH_INTERVAL
    );
    assert_eq!(
        refresh_delay(Duration::from_micros(500), false),
        MIN_REFRESH_INTERVAL
    );
    assert_eq!(MIN_REFRESH_INTERVAL, Duration::from_millis(500));
}

/// The origin a credential is scoped to is RFC 6454's: scheme, host *and*
/// port. Another host, another port or the other scheme is another place the
/// credential would be presented, and it is presented at none of them.
#[test]
fn a_credential_is_scoped_to_the_source_origin_including_its_scheme() {
    let source = Url::parse("https://origin.example/live/index.m3u8").unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("x-api-key"),
        HeaderValue::from_static("KEY"),
    );
    headers.insert(
        HeaderName::from_static("authorization"),
        HeaderValue::from_static("Bearer TOKEN"),
    );
    let scope = AuthScope::new(&source, RequestAuth::Headers(headers));
    let client = Client::new();
    let applied = |raw: &str| {
        scope
            .apply(&Url::parse(raw).unwrap(), client.get(raw))
            .build()
            .unwrap()
            .headers()
            .contains_key("x-api-key")
    };
    assert!(
        applied("https://origin.example/live/seg0.ts"),
        "the source's own origin"
    );
    assert!(
        applied("https://origin.example/other/seg0.ts"),
        "any path in that origin"
    );
    assert!(
        !applied("http://origin.example/live/seg0.ts"),
        "the same host over http is another origin: an https source must not          send its credential in the clear"
    );
    assert!(
        !applied("https://origin.example:8443/live/seg0.ts"),
        "another port is another origin"
    );
    assert!(
        !applied("https://cdn.example/live/seg0.ts"),
        "another host is another origin"
    );
    // Nothing configured: nothing to scope.
    let bare = AuthScope::new(&source, RequestAuth::None);
    assert!(!bare.is_configured());
}

/// A retry burst is six failures — every burst, not just the later ones — and
/// then the quiet period, from which the next burst starts at the first
/// attempt again.
#[test]
fn a_retry_burst_is_six_failures_and_the_quiet_period() {
    let mut attempts = 0;
    let mut delays = Vec::new();
    for _ in 0..MAX_ATTEMPTS {
        delays.push(retry_delay(&mut attempts));
    }
    assert_eq!(
        delays,
        vec![
            backoff(1),
            backoff(2),
            backoff(3),
            backoff(4),
            backoff(5),
            QUIET_PERIOD,
        ],
        "the sixth failure starts the quiet period"
    );
    assert_eq!(
        attempts, 0,
        "and the next burst starts from the first attempt"
    );
    assert_eq!(
        retry_delay(&mut attempts),
        backoff(1),
        "a later burst is six failures too"
    );
    assert_eq!(attempts, 1);
}

/// Every pull log line renders a URL without its query: an origin that signs
/// its URLs — Akamai `hdnts`, CloudFront `Policy`/`Signature`/`Key-Pair-Id`,
/// S3 `X-Amz-Signature` — puts its credential there, and none of those keys
/// are on `redact_url`'s allowlist.
#[test]
fn a_log_url_drops_the_whole_query() {
    for signed in [
        "https://cdn.example/live/seg0.ts?hdnts=st=1~exp=2~acl=*~hmac=SECRETSIG",
        "https://cdn.example/live/seg0.ts?Policy=SECRETPOLICY&Signature=SECRETSIG&Key-Pair-Id=SECRETKEY",
        "https://bucket.s3.example/live/seg0.ts?X-Amz-Signature=SECRETSIG&X-Amz-Credential=SECRETKEY",
        "https://cdn.example/live/seg0.ts?token=SECRETTOKEN",
    ] {
        let rendered = log_url(signed);
        assert!(!rendered.contains("SECRET"), "{rendered}");
        assert!(
            !rendered.contains('?') || rendered.ends_with("?<redacted>"),
            "{rendered}"
        );
        assert!(
            rendered.contains("cdn.example") || rendered.contains("s3.example"),
            "{rendered}"
        );
        assert!(rendered.contains("live/seg0.ts"), "{rendered}");
        assert!(rendered.contains("?<redacted>"), "{rendered}");
    }
    // Nothing to drop: the URL is rendered as it stands, and a URL that is
    // not one keeps the part before the query.
    assert_eq!(
        log_url("https://cdn.example/live/index.m3u8"),
        "https://cdn.example/live/index.m3u8"
    );
    assert_eq!(
        log_url("https://cdn.example:8443/live/index.m3u8?hdnts=x"),
        "https://cdn.example:8443/live/index.m3u8?<redacted>"
    );
    assert_eq!(log_url("not a url?secret=x"), "not a url?<redacted>");
    // Akamai's path form carries the token in a path segment, so a segment
    // with a `=` in it prints as the marker and the rest of the path stays.
    assert_eq!(
        log_url("https://cdn.example/hdntl=exp=1~acl=*~hmac=SECRETSIG/live/seg0.ts"),
        "https://cdn.example/<redacted>/live/seg0.ts"
    );
    assert_eq!(
        log_url("https://cdn.example/live/seg0.ts?hdntl=exp=1~hmac=SECRETSIG"),
        "https://cdn.example/live/seg0.ts?<redacted>"
    );
    // A signed source URL is what `PullSource`'s Debug renders, and the
    // credentials it carries are write-only.
    let mut signed_source = source(
        "https://cdn.example/live/index.m3u8?hdnts=st=1~exp=2~hmac=SECRETSIG".to_string(),
        VariantRule::HighestBandwidth,
    );
    signed_source.auth = PullAuth::Bearer {
        token: "SECRETTOKEN".to_string(),
    };
    let debugged = format!("{signed_source:?}");
    assert!(!debugged.contains("SECRET"), "{debugged}");
    assert!(debugged.contains("?<redacted>"), "{debugged}");
    assert!(debugged.contains("cdn.example"), "{debugged}");
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
        107,
        "the entry closest to the end whose start is still three target durations \
         before the playlist ends (RFC 8216 6.3.3)"
    );
    // Three 6 s entries fill three 6 s target durations exactly: the earliest
    // of them is the edge, not the one before it.
    let six = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:0\n\
               #EXTINF:6.0,\nseg0.ts\n#EXTINF:6.0,\nseg1.ts\n#EXTINF:6.0,\nseg2.ts\n\
               #EXTINF:6.0,\nseg3.ts\n";
    match parse_remote(&base, six.as_bytes()).unwrap() {
        Playlist::Media(media) => assert_eq!(
            live_start(&media),
            1,
            "seg1, seg2 and seg3 are the last three target durations"
        ),
        other => panic!("expected a media playlist, got {other:?}"),
    }
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
        "http://100.64.0.1/s.ts",
        "http://100.100.100.200/s.ts",
        "http://100.127.255.254/s.ts",
        "http://[64:ff9b::7f00:1]/s.ts",
        "http://[64:ff9b::a9fe:a9fe]/s.ts",
        "http://localhost/s.ts",
        "http://LOCALHOST/s.ts",
        "http://localhost./s.ts",
        "http://a.localhost/s.ts",
        "http://a.b.localhost/s.ts",
        "http://LOCALHOST./s.ts",
    ] {
        assert!(refused(url), "{url} is a refused range");
    }
    for url in [
        "http://93.184.216.34/s.ts",
        "http://172.32.0.1/s.ts",
        "http://8.8.8.8/s.ts",
        "http://100.63.255.254/s.ts",
        "http://100.128.0.1/s.ts",
        "http://[2606:4700::1111]/s.ts",
        "http://[64:ff9b::808:808]/s.ts",
        "http://origin.example/s.ts",
        "http://notlocalhost.example/s.ts",
        "http://localhost.example/s.ts",
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
fn a_redirect_is_refused_for_credentials_a_private_target_or_a_downgrade() {
    let source = Url::parse("https://origin.example/live/index.m3u8").unwrap();
    let same_origin = Url::parse("https://origin.example/live/other.m3u8").unwrap();
    let other_port = Url::parse("https://origin.example:8443/x.m3u8").unwrap();
    let public = Url::parse("https://cdn.example/live/x.m3u8").unwrap();
    let private = Url::parse("https://10.0.0.5/x.m3u8").unwrap();
    // https so that only the address rule, not the downgrade rule, applies.
    let loopback = Url::parse("https://127.0.0.1/x.m3u8").unwrap();
    let downgrade = Url::parse("http://origin.example/live/other.m3u8").unwrap();

    assert_eq!(
        redirect_refusal(&source, &same_origin, true, false),
        None,
        "a redirect inside the origin is followed"
    );
    // Credentials: another origin is another place they would be presented,
    // so the hop is refused however public the target is.
    assert_eq!(
        redirect_refusal(&source, &public, true, false),
        Some("redirect would carry credentials to another origin")
    );
    assert_eq!(
        redirect_refusal(&source, &other_port, true, false),
        Some("redirect would carry credentials to another origin"),
        "another port is another origin"
    );
    // Without credentials, the target's address decides.
    assert_eq!(
        redirect_refusal(&source, &public, false, false),
        None,
        "a credential-less pull may follow its nearest edge"
    );
    assert_eq!(
        redirect_refusal(&source, &private, false, false),
        Some("redirect points at a private address")
    );
    assert_eq!(
        redirect_refusal(&source, &loopback, false, false),
        Some("redirect points at a private address")
    );
    // A source that is itself in one of those ranges may be redirected within
    // them: the same allowance the playlist's own URIs get.
    assert_eq!(redirect_refusal(&source, &loopback, false, true), None);
    // A downgrade is refused for every pull: an https source must not be
    // silently weakened.
    assert_eq!(
        redirect_refusal(&source, &downgrade, true, false),
        Some("redirect would take the pull to http")
    );
    assert_eq!(
        redirect_refusal(&source, &downgrade, false, false),
        Some("redirect would take the pull to http")
    );
    let secure = Url::parse("https://origin.example:8080/x.m3u8").unwrap();
    let clear = Url::parse("http://origin.example:8080/x.m3u8").unwrap();
    assert_eq!(
        redirect_refusal(&secure, &clear, false, false),
        Some("redirect would take the pull to http"),
        "the same host and port over the clear is still a downgrade"
    );
}
