use super::*;

/// The catalog moq-cli 0.14.7 publishes for the interop fixture (captured
/// from moq-relay 0.17.7's `/fetch/<broadcast>/catalog.json`), trimmed to the
/// fields the verdict reads.
fn moq_cli_catalog() -> Value {
    json!({
        "video": {"renditions": {"0.avc3": {
            "codec": "avc3.640028", "codedWidth": 1920, "codedHeight": 1080,
            "container": {"kind": "legacy"}
        }}},
        "audio": {"renditions": {"1.aac": {
            "codec": "mp4a.40.2", "sampleRate": 48000, "numberOfChannels": 2,
            "description": "1190", "container": {"kind": "legacy"}
        }}}
    })
}

#[test]
fn an_h264_and_aac_hang_catalog_with_hex_descriptions_passes() {
    assert_eq!(catalog_failures(&moq_cli_catalog()), Vec::<String>::new());

    let mut avc1 = moq_cli_catalog();
    avc1["video"]["renditions"]["0.avc3"]["codec"] = json!("avc1.64001f");
    avc1["video"]["renditions"]["0.avc3"]["description"] = json!("01640028ffe1");
    assert_eq!(catalog_failures(&avc1), Vec::<String>::new());
}

#[test]
fn a_catalog_with_a_non_hex_or_missing_avc1_description_fails() {
    let mut avc1 = moq_cli_catalog();
    avc1["video"]["renditions"]["0.avc3"]["codec"] = json!("avc1.64001f");
    // avc1 keeps its parameter sets out of band: no description, no decoder.
    assert_eq!(
        catalog_failures(&avc1).len(),
        1,
        "{:?}",
        catalog_failures(&avc1)
    );

    // Base64 (the other common encoding), odd-length and empty hex.
    for description in ["AWQAKP/h", "119", ""] {
        let mut catalog = moq_cli_catalog();
        catalog["audio"]["renditions"]["1.aac"]["description"] = json!(description);
        assert_eq!(catalog_failures(&catalog).len(), 1, "{description:?}");
    }
}

#[test]
fn a_catalog_missing_a_track_or_its_audio_config_fails() {
    let mut no_audio = moq_cli_catalog();
    no_audio["audio"]["renditions"] = json!({});
    assert_eq!(catalog_failures(&no_audio).len(), 1);

    let mut hevc_only = moq_cli_catalog();
    hevc_only["video"]["renditions"]["0.avc3"]["codec"] = json!("hev1.1.6.L93.B0");
    assert_eq!(catalog_failures(&hevc_only).len(), 1);

    let mut no_rate = moq_cli_catalog();
    no_rate["audio"]["renditions"]["1.aac"]
        .as_object_mut()
        .unwrap()
        .remove("sampleRate");
    assert_eq!(catalog_failures(&no_rate).len(), 1);

    assert_eq!(catalog_failures(&json!({})).len(), 2);
}

const LIFE_ONE: &str = "\
2026-10-18T05:33:00.770166Z  INFO conn{id=0 remote=127.0.0.1:45201 session=\"94\"}: moq_relay::connection: session accepted transport=quic role=Some(Publisher) tier= root= publish=** subscribe=**
2026-10-18T05:33:00.770590Z  INFO conn{id=0 remote=127.0.0.1:45201 session=\"94\"}: moq_relay::connection: negotiated version=moq-lite-06 transport=quic
2026-10-18T05:33:03.744862Z  INFO conn{id=1 remote=127.0.0.1:52562 session=\"fd\"}: moq_relay::connection: session accepted transport=quic role=Some(Subscriber) tier= root= publish=** subscribe=**
2026-10-18T05:33:03.744958Z  INFO conn{id=1 remote=127.0.0.1:52562 session=\"fd\"}: moq_relay::connection: negotiated version=moq-lite-06 transport=quic
2026-10-18T05:33:03.800000Z  INFO conn{id=1 remote=127.0.0.1:52562 session=\"fd\"}: moq_net::lite::publisher: track info started broadcast=live/egress.hang track=catalog.json
";

#[test]
fn relay_sessions_join_accept_and_negotiation_lines_per_relay_life() {
    // Connection ids restart at zero in the second life; the role may be absent.
    let log = format!(
        "{RELAY_START_MARKER}1\n{LIFE_ONE}{RELAY_START_MARKER}2\n\
         \u{1b}[2m2026\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m conn{{id=0 remote=127.0.0.1:40000}}: moq_relay::connection: session accepted transport=webtransport role=None tier= root=\n\
         2026 INFO conn{{id=0 remote=127.0.0.1:40000}}: moq_relay::connection: negotiated version=moq-lite-03 transport=webtransport\n"
    );
    let sessions = relay_sessions(&log);
    assert_eq!(
        sessions,
        vec![
            RelaySession {
                life: 1,
                conn: 0,
                role: Some("Publisher".to_string()),
                version: Some("moq-lite-06".to_string()),
                transport: Some("quic".to_string()),
            },
            RelaySession {
                life: 1,
                conn: 1,
                role: Some("Subscriber".to_string()),
                version: Some("moq-lite-06".to_string()),
                transport: Some("quic".to_string()),
            },
            RelaySession {
                life: 2,
                conn: 0,
                role: None,
                version: Some("moq-lite-03".to_string()),
                transport: Some("webtransport".to_string()),
            },
        ]
    );
}

#[test]
fn a_relay_life_passes_only_when_every_session_negotiated_lite_06() {
    let log = format!("{RELAY_START_MARKER}1\n{LIFE_ONE}{RELAY_START_MARKER}2\n");
    let sessions = relay_sessions(&log);
    assert!(sessions_on_version(&sessions, 1, LITE_VERSION).0);
    // A life nobody connected to proves nothing.
    assert!(!sessions_on_version(&sessions, 2, LITE_VERSION).0);

    let mut older = sessions.clone();
    older[1].version = Some("moq-lite-02".to_string());
    assert!(!sessions_on_version(&older, 1, LITE_VERSION).0);
    // Accepted but never negotiated (the handshake died) is not a pass.
    let mut unfinished = sessions;
    unfinished[0].version = None;
    assert!(!sessions_on_version(&unfinished, 1, LITE_VERSION).0);
}

#[test]
fn ietf_catalog_requires_loc_and_both_decoder_configurations() {
    let catalog = json!({
        "version": 1,
        "tracks": [
            {"name": "video", "role": "video", "packaging": "loc",
             "codec": "avc1.640028", "initData": "AWQAKP/h"},
            {"name": "audio", "role": "audio", "packaging": "loc",
             "codec": "mp4a.40.2", "samplerate": 48000,
             "channelConfig": "2", "initData": "EZA="}
        ]
    });
    assert!(profile_catalog_failures(&catalog, true).is_empty());
    for (field, value) in [
        ("packaging", json!("legacy")),
        ("initData", json!("not base64")),
        ("samplerate", json!(0)),
        ("channelConfig", json!("")),
    ] {
        let mut invalid = catalog.clone();
        invalid["tracks"][1][field] = value;
        assert!(
            !profile_catalog_failures(&invalid, true).is_empty(),
            "{field}"
        );
    }
    let mut no_video = catalog.clone();
    no_video["tracks"][0]["role"] = json!("audio");
    assert!(!profile_catalog_failures(&no_video, true).is_empty());
    assert!(!profile_catalog_failures(&moq_cli_catalog(), true).is_empty());
}

#[test]
fn ietf_sessions_reject_lite_fallback_and_accept_the_cli_draft_name() {
    let log = format!("{RELAY_START_MARKER}1\n{LIFE_ONE}");
    for draft in IetfDraft::ALL {
        assert!(!sessions_on_version(&relay_sessions(&log), 1, draft.version()).0);
        let ietf = log.replace(LITE_VERSION, draft.version());
        assert!(sessions_on_version(&relay_sessions(&ietf), 1, draft.version()).0);
        // A session on another draft is not this one.
        for other in IetfDraft::ALL.into_iter().filter(|other| *other != draft) {
            assert!(!sessions_on_version(&relay_sessions(&ietf), 1, other.version()).0);
        }
    }
}

/// A Playwright JSON report in the shape `--reporter=json` writes: a file
/// suite holding specs, each with one test per project.
fn playwright_report() -> Value {
    let test = |project: &str, status: &str, annotations: Value, error: Option<&str>| {
        let result_status = match status {
            "expected" => "passed",
            "skipped" => "skipped",
            _ => "failed",
        };
        json!({
            "projectName": project,
            "status": status,
            "annotations": annotations,
            "results": [{
                "status": result_status,
                "error": error.map(|message| json!({"message": message})),
            }],
        })
    };
    json!({
        "suites": [{
            "title": "watch.moq.ts",
            "specs": [],
            "suites": [{
                "title": "nested",
                "specs": [
                    {"title": "plays origin", "tests": [
                        test("chromium", "expected", json!([
                            {"type": "moq-protocol", "description": "moq-lite-06/webtransport"},
                            {"type": "moq-playback", "description": "{\"videoFrames\":290}"},
                            {"type": "skip", "description": "not ours"},
                        ]), None),
                        test("firefox", "skipped", json!([
                            {"type": "moq-browser-version", "description": "154.0"},
                            {"type": "moq-protocol", "description": "moq-lite-02/websocket"},
                        ]), None),
                    ]},
                    {"title": "plays relay", "tests": [
                        test("chromium", "unexpected", json!([]), Some("decoded video frames in 10 s")),
                    ]},
                    {"title": "refuses origin without a token", "tests": [
                        test("chromium", "expected", json!([
                            {"type": "moq-refusal", "description": "Error: unauthorized"},
                        ]), None),
                    ]},
                ],
            }],
        }],
    })
}

#[test]
fn browser_outcomes_read_nested_specs_and_only_moq_annotations() {
    let outcomes = browser_outcomes(&playwright_report());
    assert_eq!(outcomes.len(), 4);
    let origin = &outcomes[0];
    assert_eq!(
        (origin.project.as_str(), origin.title.as_str()),
        ("chromium", "plays origin")
    );
    assert_eq!(
        origin.annotations["moq-protocol"],
        json!("moq-lite-06/webtransport")
    );
    // JSON annotations are kept structured; foreign ones are dropped.
    assert_eq!(
        origin.annotations["moq-playback"]["videoFrames"],
        json!(290)
    );
    assert!(!origin.annotations.contains_key("skip"));
    // A version is text, not the number `154.0`.
    assert_eq!(
        outcomes[1].annotations["moq-browser-version"],
        json!("154.0")
    );
    assert_eq!(
        outcomes[2].error.as_deref(),
        Some("decoded video frames in 10 s")
    );
}

#[test]
fn browser_rows_pass_fail_fallback_and_missing_pairs() {
    let outcomes = browser_outcomes(&playwright_report());
    let rows: Vec<(String, BrowserVerdict)> =
        browser_rows(&outcomes, &[("origin", true), ("relay", false)])
            .into_iter()
            .map(|(id, verdict, _)| (id, verdict))
            .collect();
    assert_eq!(
        rows,
        vec![
            ("browser-chromium-origin".to_string(), BrowserVerdict::Pass),
            (
                "browser-chromium-origin-no-token".to_string(),
                BrowserVerdict::Pass
            ),
            ("browser-chromium-relay".to_string(), BrowserVerdict::Fail),
            // An older Firefox: recorded as a fallback, never a lite-06 pass.
            (
                "browser-firefox-origin".to_string(),
                BrowserVerdict::Fallback
            ),
            // The report has no Firefox refusal or relay result at all.
            (
                "browser-firefox-origin-no-token".to_string(),
                BrowserVerdict::Fail
            ),
            ("browser-firefox-relay".to_string(), BrowserVerdict::Fail),
        ]
    );
}

#[test]
fn an_origin_grant_is_subscribed_on_the_bound_listener_with_its_token() {
    // Restream may advertise another host or port; the path, other query
    // pairs and the token are kept.
    let grant = json!({
        "token": "tok.en-1",
        "url": "https://restream.example:4443/session?a=1&token=stale",
        "broadcast": "pipe-1",
        "expiresAt": "2026-10-18T06:00:00Z",
    });
    assert_eq!(
        OriginGrant::parse(&grant, 45123),
        Ok(OriginGrant {
            url: "https://127.0.0.1:45123/session?a=1&token=tok.en-1".to_string(),
            token: "tok.en-1".to_string(),
            broadcast: "pipe-1".to_string(),
        })
    );
    for bad in [
        json!({"url": "https://h/", "broadcast": "b"}),
        json!({"token": "", "url": "https://h/", "broadcast": "b"}),
        json!({"token": "t", "broadcast": "b"}),
        json!({"token": "t", "url": "https://h/"}),
        json!({"token": "t", "url": "moqt://h/", "broadcast": "b"}),
        json!({"token": "t", "url": "not a url", "broadcast": "b"}),
    ] {
        assert!(OriginGrant::parse(&bad, 1).is_err(), "{bad}");
    }
}

#[test]
fn token_variants_drop_or_replace_only_the_token() {
    let url = "https://127.0.0.1:1/?a=1&token=good";
    assert_eq!(
        with_token(url, None).as_deref(),
        Ok("https://127.0.0.1:1/?a=1")
    );
    assert_eq!(
        with_token("https://127.0.0.1:1/?token=good", None).as_deref(),
        Ok("https://127.0.0.1:1/")
    );
    assert_eq!(
        with_token(url, Some("bad")).as_deref(),
        Ok("https://127.0.0.1:1/?a=1&token=bad")
    );
    for token in ["abcA", "abcd", "A"] {
        let changed = tampered(token);
        assert_ne!(changed, token);
        assert_eq!(changed.len(), token.len());
        assert_eq!(changed[..token.len() - 1], token[..token.len() - 1]);
    }
}

#[test]
fn the_cli_offers_only_lite_06_over_quic_and_puts_the_stage_last() {
    let ca = PathBuf::from("/tmp/run/tls/ca.pem");
    let target = CliTarget {
        url: "https://localhost:4443/",
        ca: &ca,
        broadcast: "live/sk.hang",
        version: LITE_VERSION,
    };
    assert_eq!(
        target.args(EXPORT_TS),
        [
            "--log-level",
            "warn",
            "--connect",
            "https://localhost:4443/",
            "--connect-tls-root",
            "/tmp/run/tls/ca.pem",
            "--connect-version",
            "moq-lite-06",
            "--connect-websocket-enabled=false",
            "--broadcast",
            "live/sk.hang",
            "export",
            "--catalog-format",
            "hang",
            "ts",
        ]
    );
    assert_eq!(
        target.args(IMPORT_TS).last().map(String::as_str),
        Some("ts")
    );
}

#[test]
fn ietf_cli_pins_the_draft_and_reads_msf_without_hang_fallback() {
    let ca = PathBuf::from("/tmp/run/tls/ca.pem");
    for draft in IetfDraft::ALL {
        let target = CliTarget {
            url: "https://127.0.0.1:4443/",
            ca: &ca,
            broadcast: "live/egress.hang",
            version: draft.version(),
        };
        let args = target.args(EXPORT_TS);
        let offered: Vec<_> = args
            .windows(2)
            .filter(|pair| pair[0] == "--connect-version")
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(offered, [draft.version()]);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--catalog-format", "msf"])
        );
        assert!(!args.iter().any(|arg| arg == LITE_VERSION || arg == "hang"));
    }
}

#[test]
fn options_accept_browsers_a_draft_and_the_netns_opt_out_only() {
    let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    assert_eq!(
        DevOptions::parse(&[]),
        Ok(DevOptions {
            browsers: false,
            ietf: None
        })
    );
    assert_eq!(
        DevOptions::parse(&args(&["--no-netns", "--browsers"])),
        Ok(DevOptions {
            browsers: true,
            ietf: None
        })
    );
    for (number, alpn, version) in [
        ("16", "moqt-16", "moq-transport-16"),
        ("18", "moqt-18", "moq-transport-18"),
        ("19", "moqt-19", "moq-transport-19"),
    ] {
        let ietf = DevOptions::parse(&args(&["--ietf", number, "--browsers"])).unwrap();
        assert!(ietf.browsers);
        assert_eq!((ietf.alpn(), ietf.version()), (alpn, version));
    }
    // A draft neither side speaks, or none at all, is refused.
    for bad in [
        &["--ietf", "17"][..],
        &["--ietf"],
        &["--ietf", "--browsers"],
        &["--browser"],
    ] {
        assert!(DevOptions::parse(&args(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn moq_dev_interop_is_a_catalog_command_off_the_default_suite() {
    let spec = mode_spec("moq-dev-interop").expect("moq-dev-interop must be in modes.json");
    assert!(
        spec.requires_port_namespace,
        "it binds harness ports and must stay isolated by default"
    );
    assert!(
        !spec.suite_default,
        "it needs downloaded peers, so it is opt-in"
    );
    assert!(!spec.requires_bench_profile);
    assert!(
        supported_mode_names()
            .iter()
            .any(|name| name == "moq-dev-interop")
    );
}
