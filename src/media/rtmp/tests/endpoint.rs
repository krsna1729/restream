#[test]
fn parse_rtmp_url_standard_forms() {
    // Default port
    let parts = parse_rtmp_url("rtmp://a.example.com/live/mykey").unwrap();
    assert_eq!(parts.host, "a.example.com");
    assert_eq!(parts.port, 1935);
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "mykey");
    assert!(!parts.tls);

    // Explicit port
    let parts = parse_rtmp_url("rtmp://a.example.com:19350/stream/abc").unwrap();
    assert_eq!(parts.host, "a.example.com");
    assert_eq!(parts.port, 19350);
    assert_eq!(parts.app, "stream");
    assert_eq!(parts.stream_key, "abc");
    assert!(!parts.tls);

    // rtmps:// (TLS) — same parsing, different default port behaviour (still 1935 if omitted)
    let parts = parse_rtmp_url("rtmps://live-api-s.facebook.com:443/rtmp/FB-STREAM-KEY").unwrap();
    assert_eq!(parts.host, "live-api-s.facebook.com");
    assert_eq!(parts.port, 443);
    assert_eq!(parts.app, "rtmp");
    assert_eq!(parts.stream_key, "FB-STREAM-KEY");
    assert!(parts.tls);

    // Stream key containing slashes is NOT split — key gets everything after first slash in path
    let parts = parse_rtmp_url("rtmp://host/app/key/subpart").unwrap();
    assert_eq!(parts.app, "app");
    assert_eq!(parts.stream_key, "key/subpart");
    assert!(!parts.tls);

    // Unrecognised scheme → None
    assert!(parse_rtmp_url("https://host/live/key").is_none());

    // Missing path separator → None (can't split app/key)
    assert!(parse_rtmp_url("rtmp://host/noapp").is_none());
}

// --- Regression: issue #5 (Round 5) — IPv6 RTMP URL parsing ---
// Before the fix, `host_port.find(':')` landed inside the IPv6 brackets
// (first `:` in `[::1]:1935` is at position 2, inside the brackets),
// causing the host to be parsed as `[` and port parsing to fail.
#[test]
fn parse_rtmp_url_ipv6_literal() {
    let result = parse_rtmp_url("rtmp://[::1]:1935/live/mykey");
    assert!(result.is_some(), "IPv6 URL must parse successfully");
    let parts = result.unwrap();
    assert_eq!(parts.host, "::1");
    assert_eq!(parts.port, 1935);
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "mykey");
    assert!(!parts.tls);
}

#[test]
fn parse_rtmp_url_ipv6_default_port() {
    let result = parse_rtmp_url("rtmp://[2001:db8::1]/live/mykey");
    assert!(
        result.is_some(),
        "IPv6 URL without port must use default 1935"
    );
    let parts = result.unwrap();
    assert_eq!(parts.host, "2001:db8::1");
    assert_eq!(parts.port, 1935);
    assert!(!parts.tls);
}

#[test]
fn parse_rtmp_url_ipv4_unchanged() {
    // Ensure the IPv4 path still works correctly after the IPv6 fix.
    let result = parse_rtmp_url("rtmp://192.168.1.1:1935/live/key");
    assert!(result.is_some());
    let parts = result.unwrap();
    assert_eq!(parts.host, "192.168.1.1");
    assert_eq!(parts.port, 1935);
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "key");
    assert!(!parts.tls);
}

// --- Adversarial: percent-encoded app/stream key must reach the
// destination RTMP server decoded, not still escaped ---
// (found via a "hunting" pass on egress_transport.rs — path_segments()
// returns raw percent-encoded segments; forwarding them unescaped as the
// AMF-level app/stream key would corrupt any push target whose key
// contains a URL-reserved character.)

#[test]
fn parse_rtmp_url_percent_encoded_stream_key_slash() {
    // %2F inside a single path segment must decode to a literal '/' in the
    // stream key, not stay as the three-character escape.
    let parts = parse_rtmp_url("rtmp://host/live/part1%2Fpart2").unwrap();
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "part1/part2");
}

#[test]
fn parse_rtmp_url_percent_encoded_app_and_space() {
    let parts = parse_rtmp_url("rtmp://host/my%20app/key%2Bvalue").unwrap();
    assert_eq!(parts.app, "my app");
    assert_eq!(parts.stream_key, "key+value");
}

#[test]
fn parse_rtmp_url_invalid_percent_sequence_does_not_panic() {
    // A stray '%' not followed by two hex digits is invalid percent-encoding;
    // decoding must degrade gracefully (lossy) rather than panic.
    let parts = parse_rtmp_url("rtmp://host/live/key%zz").unwrap();
    assert_eq!(parts.stream_key, "key%zz");
}

#[test]
fn parse_rtmp_url_trailing_slash_yields_trailing_slash_key() {
    // Documents current behaviour: a trailing path separator becomes a
    // trailing '/' in the stream key rather than being trimmed or rejected.
    let parts = parse_rtmp_url("rtmp://host/live/key/").unwrap();
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "key/");
}

#[test]
fn parse_rtmp_url_ignores_query_and_fragment() {
    let parts = parse_rtmp_url("rtmp://host/live/key?token=abc#frag").unwrap();
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "key");
}

#[test]
fn parse_rtmp_url_drops_embedded_userinfo() {
    // Credentials embedded in the URL (rtmp://user:pass@host/...) must not
    // leak into app/stream_key and must not change the resolved host.
    let parts = parse_rtmp_url("rtmp://user:pass@host/live/key").unwrap();
    assert_eq!(parts.host, "host");
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "key");
}

#[test]
fn parse_rtmp_url_rejects_empty_authority() {
    assert!(parse_rtmp_url("rtmp:///live/key").is_none());
}

#[test]
fn parse_rtmp_url_rejects_out_of_range_port() {
    assert!(parse_rtmp_url("rtmp://host:999999/live/key").is_none());
}

#[test]
fn parse_rtmp_url_rejects_unterminated_ipv6_literal() {
    assert!(parse_rtmp_url("rtmp://[::1/live/key").is_none());
}

#[test]
fn parse_rtmp_url_case_insensitive_scheme() {
    let parts = parse_rtmp_url("RTMP://host/live/key").unwrap();
    assert!(!parts.tls);
    let parts = parse_rtmp_url("RTMPS://host/live/key").unwrap();
    assert!(parts.tls);
}

#[test]
fn parse_rtmp_url_trims_surrounding_whitespace() {
    let parts = parse_rtmp_url(" rtmp://host/live/key ").unwrap();
    assert_eq!(parts.host, "host");
    assert_eq!(parts.app, "live");
    assert_eq!(parts.stream_key, "key");
}

