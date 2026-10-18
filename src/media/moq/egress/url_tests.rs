use super::*;

#[test]
fn port_defaults_to_443_and_path_keeps_its_leading_slash() {
    let url = parse_moq_url("moqt://relay.example/live").unwrap();
    assert_eq!(
        url,
        MoqUrl {
            host: "relay.example".to_string(),
            port: 443,
            path: "/live".to_string(),
            broadcast: None,
            ca_file: None,
            insecure: false,
        }
    );
}

#[test]
fn explicit_port_and_nested_path_are_kept() {
    let url = parse_moq_url("moqt://127.0.0.1:8893/team/live/").unwrap();
    assert_eq!(url.host, "127.0.0.1");
    assert_eq!(url.port, 8893);
    assert_eq!(url.path, "/team/live");
}

#[test]
fn a_url_without_a_path_names_the_relay_root() {
    for url in ["moqt://relay.example", "moqt://relay.example/"] {
        assert_eq!(parse_moq_url(url).unwrap().path, "", "{url}");
    }
    let url = parse_moq_url("moqt://relay.example/?jwt=abc&broadcast=show").unwrap();
    assert_eq!(url.path, "?jwt=abc");
    assert_eq!(url.broadcast.as_deref(), Some("show"));
}

#[test]
fn ipv6_host_loses_its_brackets() {
    let url = parse_moq_url("moqt://[::1]:4443/live").unwrap();
    assert_eq!(url.host, "::1");
    assert_eq!(url.port, 4443);
}

#[test]
fn restream_query_keys_are_taken_out_of_the_relay_path() {
    let url = parse_moq_url(
        "moqt://relay.example:8893/live?token=a%2Fb&ca=%2Fetc%2Frelay%20ca.pem&insecure=1&broadcast=team%2Fshow&x=1",
    )
    .unwrap();
    assert_eq!(url.ca_file.as_deref(), Some("/etc/relay ca.pem"));
    assert!(url.insecure);
    assert_eq!(url.broadcast.as_deref(), Some("team/show"));
    // Relay pairs keep their original encoding and order.
    assert_eq!(url.path, "/live?token=a%2Fb&x=1");
}

#[test]
fn only_restream_keys_leave_no_query_on_the_path() {
    let url = parse_moq_url("moqt://relay.example/live?insecure=false&ca=ca.pem").unwrap();
    assert_eq!(url.path, "/live");
    assert!(!url.insecure);
    assert_eq!(url.ca_file.as_deref(), Some("ca.pem"));
    assert_eq!(url.broadcast, None);
}

#[test]
fn invalid_urls_are_rejected_with_their_reason() {
    for (url, error) in [
        ("not a url", MoqUrlError::Malformed),
        ("rtmp://relay.example/live/key", MoqUrlError::NotMoqt),
        ("moq://relay.example/live", MoqUrlError::NotMoqt),
        (
            "moqt://user:pass@relay.example/live",
            MoqUrlError::Credentials,
        ),
        (
            "moqt://relay.example/live?insecure=yes",
            MoqUrlError::InvalidInsecure,
        ),
        ("moqt://relay.example/live?ca=", MoqUrlError::EmptyCa),
        (
            "moqt://relay.example/live?broadcast=",
            MoqUrlError::EmptyBroadcast,
        ),
        (
            "moqt://relay.example/live?broadcast=%2F",
            MoqUrlError::EmptyBroadcast,
        ),
    ] {
        assert_eq!(parse_moq_url(url), Err(error), "{url}");
    }
}
