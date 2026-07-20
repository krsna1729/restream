#[test]
fn streamid_getsockopt_length_must_stay_within_buffer() {
    let mut buf = [0u8; 8];
    buf[..5].copy_from_slice(b"key\0x");

    assert_eq!(
        streamid_from_getsockopt_buffer(&buf, 5),
        Some("key\0x".trim_matches('\0').to_string())
    );
    assert_eq!(
        streamid_from_getsockopt_buffer(&buf, 0),
        Some(String::new())
    );
    assert_eq!(streamid_from_getsockopt_buffer(&buf, -1), None);
    assert_eq!(streamid_from_getsockopt_buffer(&buf, 9), None);
}

#[test]
fn parses_srt_stream_ids_from_common_tools() {
    let cases = [
        (
            "publish:key01?latency=240000",
            SrtConnectionMode::Publish,
            "key01",
        ),
        ("publisher:key02", SrtConnectionMode::Publish, "key02"),
        ("key03", SrtConnectionMode::Publish, "key03"),
        ("read:key04", SrtConnectionMode::Read, "key04"),
        ("play:key05", SrtConnectionMode::Read, "key05"),
        ("subscriber:key06", SrtConnectionMode::Read, "key06"),
        (
            "#!::r=key07,m=publish,latency=240000",
            SrtConnectionMode::Publish,
            "key07",
        ),
        ("#!::r=key08,m=request", SrtConnectionMode::Read, "key08"),
    ];

    for (input, mode, key) in cases {
        let parsed = parse_srt_stream_id(input);
        assert_eq!(parsed.mode, mode, "input={}", input);
        assert_eq!(parsed.stream_key, key, "input={}", input);
    }
}

#[test]
fn srt_stream_ids_normalize_plain_publish_keys_before_registration() {
    let cases = [
        "publish:key01",
        "publisher:key01?latency=240000",
        "#!::r=key01,m=publish,latency=240000",
    ];

    for input in cases {
        let parsed = parse_srt_stream_id(input);
        assert_eq!(parsed.mode, SrtConnectionMode::Publish, "input={input}");
        assert_eq!(parsed.stream_key, "key01", "input={input}");
    }
}

#[test]
fn srt_egress_preroll_is_reserved_for_1080p_variants() {
    assert_eq!(
        startup_policy::srt_egress_keyframe_preroll_packets("source"),
        0
    );
    assert_eq!(
        startup_policy::srt_egress_keyframe_preroll_packets("atrack:0"),
        0
    );
    assert_eq!(
        startup_policy::srt_egress_keyframe_preroll_packets("720p+atrack:0"),
        0
    );
    assert_eq!(
        startup_policy::srt_egress_keyframe_preroll_packets("1080p"),
        32
    );
    assert_eq!(
        startup_policy::srt_egress_keyframe_preroll_packets("1080p+atrack:1"),
        32
    );
    assert_eq!(
        startup_policy::srt_egress_keyframe_preroll_packets("1080p60+atrack:1"),
        0
    );
}

#[test]
fn srt_stream_ids_normalize_plain_read_keys_before_auth() {
    let cases = [
        "read:key02",
        "play:key02",
        "subscriber:key02?latency=240000",
        "#!::r=key02,m=request",
    ];

    for input in cases {
        let parsed = parse_srt_stream_id(input);
        assert_eq!(parsed.mode, SrtConnectionMode::Read, "input={input}");
        assert_eq!(parsed.stream_key, "key02", "input={input}");
    }
}

#[test]
fn srt_stream_ids_keep_slashes_as_literal_key_data() {
    let parsed = parse_srt_stream_id("publish:tenant/key01");
    assert_eq!(parsed.mode, SrtConnectionMode::Publish);
    assert_eq!(parsed.stream_key, "tenant/key01");

    let parsed = parse_srt_stream_id("#!::r=tenant%2Fkey02,m=request");
    assert_eq!(parsed.mode, SrtConnectionMode::Read);
    assert_eq!(parsed.stream_key, "tenant/key02");
}

#[test]
fn egress_url_parses_simple_target() {
    let u = parse_srt_egress_url("srt://192.168.1.5:9000");
    assert_eq!(u.host_port, "192.168.1.5:9000");
    assert!(u.streamid.is_empty());
    assert!(u.bond_addrs.is_empty());
}

#[test]
fn egress_url_parses_streamid() {
    let u = parse_srt_egress_url("srt://host:9000?streamid=publish:key1");
    assert_eq!(u.host_port, "host:9000");
    assert_eq!(u.streamid, "publish:key1");
    assert!(u.bond_addrs.is_empty());
}

// --- Regression: issue #6 (Round 5) — SRT stream ID percent-decode ---
// Before the fix, percent-encoded characters in the streamid query parameter
// were passed through raw. Percent-encoded stream IDs would be compared against DB
// stream keys verbatim, causing silent auth failure.
#[test]
fn percent_decode_basic() {
    assert_eq!(percent_decode("publish:key%2Done"), "publish:key-one");
    assert_eq!(percent_decode("hello%20world"), "hello world");
    assert_eq!(percent_decode("no_encoding"), "no_encoding");
    assert_eq!(percent_decode("%41%42%43"), "ABC"); // A=0x41, B=0x42, C=0x43
}

#[test]
fn percent_decode_incomplete_sequence_passthrough() {
    // A truncated %XX at the end should not panic.
    assert_eq!(percent_decode("foo%2"), "foo%2");
    assert_eq!(percent_decode("foo%"), "foo%");
}

#[test]
fn egress_url_percent_decodes_streamid() {
    // Percent-encoded streamid characters must be decoded before use.
    let u = parse_srt_egress_url("srt://host:9000?streamid=publish%3Amykey");
    assert_eq!(
        u.streamid, "publish:mykey",
        "percent-encoded streamid must be decoded in egress URL"
    );
}

#[test]
fn egress_url_parses_bond_addresses() {
    let u =
        parse_srt_egress_url("srt://primary:9000?streamid=live/out&bond=backup1:9000,backup2:9000");
    assert_eq!(u.host_port, "primary:9000");
    assert_eq!(u.streamid, "live/out");
    assert_eq!(u.bond_addrs, vec!["backup1:9000", "backup2:9000"]);
}

#[test]
fn egress_url_bond_only_no_streamid() {
    let u = parse_srt_egress_url("srt://10.0.0.1:4200?bond=10.0.0.2:4200");
    assert_eq!(u.host_port, "10.0.0.1:4200");
    assert!(u.streamid.is_empty());
    assert_eq!(u.bond_addrs, vec!["10.0.0.2:4200"]);
}

