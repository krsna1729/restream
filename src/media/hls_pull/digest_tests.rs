//! Digest computation against the RFC examples, and challenge selection.

use super::*;

/// RFC 7616 §3.9.1: the same request answered with MD5 and with SHA-256.
#[test]
fn the_rfc_7616_example_is_answered_with_both_algorithms() {
    let challenge = |algorithm| Challenge {
        realm: "http-auth@example.org".to_string(),
        nonce: "7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v".to_string(),
        opaque: Some("FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS".to_string()),
        algorithm,
        qop_auth: true,
        stale: false,
    };
    let cnonce = "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ";
    for (algorithm, expected) in [
        (Algorithm::Md5, "8ca523f5e9506fed4657c9700eebdbec"),
        (
            Algorithm::Sha256,
            "753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1",
        ),
    ] {
        let header = authorization(
            &challenge(algorithm),
            "Mufasa",
            "Circle of Life",
            "GET",
            "/dir/index.html",
            1,
            cnonce,
        );
        assert!(
            header.contains(&format!("response=\"{expected}\"")),
            "{algorithm:?}: {header}"
        );
        assert!(header.contains("qop=auth, nc=00000001"), "{header}");
        assert!(
            header.contains("opaque=\"FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS\""),
            "{header}"
        );
    }
}

/// RFC 2617 §3.5: MD5 with `qop=auth`.
#[test]
fn the_rfc_2617_example_is_answered() {
    let challenge = Challenge {
        realm: "testrealm@host.com".to_string(),
        nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".to_string(),
        opaque: None,
        algorithm: Algorithm::Md5,
        qop_auth: true,
        stale: false,
    };
    let header = authorization(
        &challenge,
        "Mufasa",
        "Circle Of Life",
        "GET",
        "/dir/index.html",
        1,
        "0a4f113b",
    );
    assert!(
        header.contains("response=\"6629fae49393a05397450978507c4ef1\""),
        "{header}"
    );
}

#[test]
fn a_challenge_is_parsed_with_quoted_commas_and_escapes() {
    let selected = select([
        r#"Digest realm="a, \"b\"", qop="auth,auth-int", nonce="n1", opaque="o", algorithm=MD5, stale=TRUE"#,
    ]);
    assert_eq!(
        selected,
        Ok(Challenge {
            realm: "a, \"b\"".to_string(),
            nonce: "n1".to_string(),
            opaque: Some("o".to_string()),
            algorithm: Algorithm::Md5,
            qop_auth: true,
            stale: true,
        })
    );
}

/// SHA-256 is chosen over MD5 when an origin offers both (RFC 7616 §3.7), and
/// a Basic challenge beside them is not a Digest one.
#[test]
fn sha_256_is_preferred_over_md5() {
    let selected = select([
        r#"Basic realm="r""#,
        r#"Digest realm="r", nonce="n", algorithm=MD5, qop="auth""#,
        r#"Digest realm="r", nonce="n", algorithm=SHA-256, qop="auth""#,
    ])
    .expect("a Digest challenge is answerable");
    assert_eq!(selected.algorithm, Algorithm::Sha256);
}

#[test]
fn a_challenge_the_pull_cannot_answer_is_refused_with_its_reason() {
    assert_eq!(select([r#"Basic realm="r""#]), Err(Refusal::NotDigest));
    assert_eq!(
        select([r#"Digest realm="r", nonce="n", algorithm=SHA-512-256"#]),
        Err(Refusal::Unsupported(
            "the origin's Digest algorithm SHA-512-256 is not supported".to_string()
        ))
    );
    assert_eq!(
        select([r#"Digest realm="r", nonce="n", qop="auth-int""#]),
        Err(Refusal::Unsupported(
            "the origin's Digest qop auth-int is not supported".to_string()
        ))
    );
}
