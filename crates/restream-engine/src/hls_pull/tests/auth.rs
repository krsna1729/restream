//! HLS pull ingest: how the pull reaches an origin — the resolver guard that
//! refuses names resolving into private ranges, and HTTP Digest answers to an
//! origin's challenge. Split from `guards.rs` for the 999-line cap.

use super::super::fetch::{AuthScope, RequestAuth, build_client};
use super::super::*;
use super::origin::{Origin, media_playlist};
use restream_domain::domain::pipeline_input::PullAuth;

// ---- the resolver guard ----------------------------------------------------

/// A *name* that resolves into a refused range is refused by the client's
/// resolver, so no request is issued: the literal guard sees only addresses,
/// and `localhost` stands here for any name an attacker points at an internal
/// address. The client connects to the addresses the resolver returned, so
/// the name cannot rebind between the check and the connect.
#[tokio::test]
async fn a_name_that_resolves_to_a_private_address_is_refused_before_any_request() {
    let origin = Origin::new();
    origin.serve("/live/index.m3u8", media_playlist(&["seg0.ts"]));
    let base = origin.spawn().await;
    let port = Url::parse(&base).unwrap().port().unwrap();
    let url = Url::parse(&format!("http://localhost:{port}/live/index.m3u8")).unwrap();
    let scope = AuthScope::new(&url, RequestAuth::new(&PullAuth::None).unwrap());

    let refused =
        super::super::fetch::fetch_small(&build_client(false, false), &url, &scope, 4096).await;
    match refused {
        Err(super::super::fetch::FetchError::PrivateTarget(reason)) => {
            assert!(reason.contains("localhost"), "{reason}")
        }
        other => panic!("a private name is refused by the resolver, got {other:?}"),
    }
    assert_eq!(
        origin.count("/live/index.m3u8"),
        0,
        "no request reaches a refused address"
    );
    assert!(
        matches!(
            PullError::from(super::super::fetch::FetchError::PrivateTarget(String::new())),
            PullError::Fatal(_)
        ),
        "a refused target is final, not retried every backoff"
    );

    // A pull whose own source is private may reach it.
    let allowed =
        super::super::fetch::fetch_small(&build_client(false, true), &url, &scope, 4096).await;
    assert!(
        allowed.is_ok(),
        "a private pull reaches its origin: {allowed:?}"
    );
    assert_eq!(origin.count("/live/index.m3u8"), 1);
}

/// A source whose name resolves into a refused range is a private pull.
#[tokio::test]
async fn a_source_named_into_a_private_range_is_a_private_pull() {
    let url = Url::parse("http://localhost:9/live/index.m3u8").unwrap();
    assert!(super::super::fetch::source_resolves_private(&url).await);
    let public = Url::parse("http://192.0.2.1/live/index.m3u8").unwrap();
    assert!(
        !super::super::fetch::source_resolves_private(&public).await,
        "a literal is judged by the literal guard, not resolved"
    );
}

// ---- HTTP Digest -----------------------------------------------------------

/// A local origin that challenges with Digest. It accepts an `Authorization:
/// Digest` naming its current nonce, and records every `Authorization` it
/// sees. The response hash itself is checked against the RFC examples in
/// `digest_tests`; this origin checks the exchange around it.
#[derive(Clone, Default)]
struct DigestOrigin {
    /// The `WWW-Authenticate` parameters after `Digest `, without the nonce.
    challenge: String,
    /// Nonces in the order the origin issues them: the first challenge uses
    /// the first, and a stale answer is told to move to the next.
    nonces: Vec<&'static str>,
    /// Every answer is refused without `stale`: the credentials are wrong.
    reject_all: bool,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

async fn digest_origin(
    axum::extract::State(origin): axum::extract::State<DigestOrigin>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let authorization = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let answered = {
        let mut seen = restream_sync::lock::lock(&origin.seen);
        seen.push(authorization.clone());
        seen.iter()
            .filter(|value| value.starts_with("Digest "))
            .count()
    };
    let current = origin.nonces[answered.saturating_sub(1).min(origin.nonces.len() - 1)];
    let latest = *origin.nonces.last().unwrap();
    if !origin.reject_all
        && authorization.starts_with("Digest ")
        && authorization.contains(&format!("nonce=\"{latest}\""))
    {
        return (axum::http::StatusCode::OK, media_playlist(&["seg0.ts"])).into_response();
    }
    let stale = authorization.starts_with("Digest ") && !origin.reject_all && current != latest;
    let nonce = if stale {
        origin.nonces[answered.min(origin.nonces.len() - 1)]
    } else {
        current
    };
    let mut value = format!("Digest {}, nonce=\"{nonce}\"", origin.challenge);
    if stale {
        value.push_str(", stale=true");
    }
    (
        axum::http::StatusCode::UNAUTHORIZED,
        [
            (
                axum::http::header::WWW_AUTHENTICATE,
                "Basic realm=\"pull\"".to_string(),
            ),
            (axum::http::header::WWW_AUTHENTICATE, value),
        ],
    )
        .into_response()
}

async fn fetch_with_digest(
    origin: DigestOrigin,
) -> (
    Result<Vec<u8>, super::super::fetch::FetchError>,
    Vec<String>,
) {
    let app = axum::Router::new()
        .fallback(axum::routing::get(digest_origin))
        .with_state(origin.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = Url::parse(&format!("http://{address}/live/index.m3u8?token=1")).unwrap();
    let auth = RequestAuth::new(&PullAuth::Basic {
        username: "Mufasa".to_string(),
        password: "Circle of Life".to_string(),
    })
    .unwrap();
    let scope = AuthScope::new(&url, auth);
    let result =
        super::super::fetch::fetch_small(&build_client(true, true), &url, &scope, 4096).await;
    let seen = restream_sync::lock::lock(&origin.seen).clone();
    (result, seen)
}

/// A `basic` credential answers a Digest challenge, as FFmpeg's client does,
/// with each supported algorithm and `qop`: the second request carries the
/// Digest answer for the challenged URI.
#[tokio::test]
async fn a_digest_challenge_is_answered_with_each_supported_algorithm() {
    for (challenge, algorithm, qop) in [
        (r#"realm="r", qop="auth", algorithm=MD5"#, "MD5", true),
        (
            r#"realm="r", qop="auth", algorithm=MD5-sess"#,
            "MD5-sess",
            true,
        ),
        (
            r#"realm="r", qop="auth,auth-int", algorithm=SHA-256"#,
            "SHA-256",
            true,
        ),
        (
            r#"realm="r", qop="auth", algorithm=SHA-256-sess"#,
            "SHA-256-sess",
            true,
        ),
        (r#"realm="r""#, "MD5", false),
    ] {
        let (result, seen) = fetch_with_digest(DigestOrigin {
            challenge: challenge.to_string(),
            nonces: vec!["n1"],
            ..DigestOrigin::default()
        })
        .await;
        assert!(result.is_ok(), "{challenge}: {result:?} after {seen:?}");
        assert_eq!(seen.len(), 2, "{challenge}: {seen:?}");
        assert!(
            seen[0].starts_with("Basic "),
            "the first request is as configured"
        );
        let answer = &seen[1];
        for field in [
            "username=\"Mufasa\"".to_string(),
            "realm=\"r\"".to_string(),
            "nonce=\"n1\"".to_string(),
            "uri=\"/live/index.m3u8?token=1\"".to_string(),
            format!("algorithm={algorithm},"),
        ] {
            assert!(
                answer.contains(&field),
                "{challenge}: {field} missing from {answer}"
            );
        }
        assert_eq!(answer.contains("qop=auth, nc=00000001"), qop, "{answer}");
    }
}

/// A `stale=true` challenge (the nonce expired, the credentials did not) is
/// answered once more with the new nonce, and its count starts again.
#[tokio::test]
async fn a_stale_nonce_is_answered_again_with_the_new_one() {
    let (result, seen) = fetch_with_digest(DigestOrigin {
        challenge: r#"realm="r", qop="auth""#.to_string(),
        nonces: vec!["n1", "n2"],
        ..DigestOrigin::default()
    })
    .await;
    assert!(result.is_ok(), "{result:?} after {seen:?}");
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(seen[1].contains("nonce=\"n1\""), "{}", seen[1]);
    assert!(seen[2].contains("nonce=\"n2\""), "{}", seen[2]);
    assert!(seen[2].contains("nc=00000001"), "{}", seen[2]);
}

/// Wrong credentials: the Digest answer is refused without `stale`, and the
/// `401` stands after one answer rather than being retried.
#[tokio::test]
async fn a_refused_digest_answer_is_not_repeated() {
    let (result, seen) = fetch_with_digest(DigestOrigin {
        challenge: r#"realm="r", qop="auth""#.to_string(),
        nonces: vec!["n1"],
        reject_all: true,
        ..DigestOrigin::default()
    })
    .await;
    assert!(
        matches!(result, Err(super::super::fetch::FetchError::Status(401))),
        "{result:?}"
    );
    assert_eq!(
        seen.len(),
        2,
        "one Digest answer, then the 401 stands: {seen:?}"
    );
}

/// A Digest challenge the pull cannot answer ends it with the reason.
#[tokio::test]
async fn an_unsupported_digest_challenge_is_refused_with_its_reason() {
    let (result, seen) = fetch_with_digest(DigestOrigin {
        challenge: r#"realm="r", algorithm=SHA-512-256"#.to_string(),
        nonces: vec!["n1"],
        ..DigestOrigin::default()
    })
    .await;
    match result {
        Err(super::super::fetch::FetchError::Auth(reason)) => {
            assert!(reason.contains("SHA-512-256"), "{reason}")
        }
        other => panic!("an unanswerable challenge is refused, got {other:?}"),
    }
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(matches!(
        PullError::from(super::super::fetch::FetchError::Auth(String::new())),
        PullError::Fatal(_)
    ));
}

/// An origin that issues a new nonce *without* `stale=true` (it restarted and
/// forgot its nonces) does not wedge the pull: that request's `401` stands,
/// and the next request answers the new challenge.
#[tokio::test]
async fn a_nonce_rotated_without_stale_is_answered_on_the_next_request() {
    let nonce = Arc::new(std::sync::Mutex::new("n1"));
    let served = nonce.clone();
    let app =
        axum::Router::new().fallback(axum::routing::get(move |headers: axum::http::HeaderMap| {
            let current = *restream_sync::lock::lock(&served);
            async move {
                use axum::response::IntoResponse as _;
                let authorization = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                if authorization.contains(&format!("nonce=\"{current}\"")) {
                    return (axum::http::StatusCode::OK, media_playlist(&["seg0.ts"]))
                        .into_response();
                }
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    [(
                        axum::http::header::WWW_AUTHENTICATE,
                        format!("Digest realm=\"r\", qop=\"auth\", nonce=\"{current}\""),
                    )],
                )
                    .into_response()
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = Url::parse(&format!("http://{address}/live/index.m3u8")).unwrap();
    let auth = RequestAuth::new(&PullAuth::Basic {
        username: "Mufasa".to_string(),
        password: "Circle of Life".to_string(),
    })
    .unwrap();
    let scope = AuthScope::new(&url, auth);
    let client = build_client(true, true);
    let fetch = || super::super::fetch::fetch_small(&client, &url, &scope, 4096);

    assert!(fetch().await.is_ok(), "the first challenge is answered");
    *restream_sync::lock::lock(&nonce) = "n2";
    assert!(
        matches!(
            fetch().await,
            Err(super::super::fetch::FetchError::Status(401))
        ),
        "a non-stale refusal is not re-sent within the request"
    );
    assert!(
        fetch().await.is_ok(),
        "the next request answers the origin's new nonce"
    );
}

/// A username a Digest header cannot carry ends the pull with the reason,
/// instead of failing at the request builder on every backoff.
#[tokio::test]
async fn a_digest_username_outside_ascii_is_refused_with_its_reason() {
    let origin = DigestOrigin {
        challenge: r#"realm="r", qop="auth""#.to_string(),
        nonces: vec!["n1"],
        ..DigestOrigin::default()
    };
    let app = axum::Router::new()
        .fallback(axum::routing::get(digest_origin))
        .with_state(origin.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = Url::parse(&format!("http://{address}/live/index.m3u8")).unwrap();
    let auth = RequestAuth::new(&PullAuth::Basic {
        username: "Müfasa".to_string(),
        password: "pw".to_string(),
    })
    .unwrap();
    let scope = AuthScope::new(&url, auth);
    let result =
        super::super::fetch::fetch_small(&build_client(true, true), &url, &scope, 4096).await;
    match result {
        Err(super::super::fetch::FetchError::Auth(reason)) => {
            assert!(reason.contains("username*"), "{reason}")
        }
        other => panic!("a non-ASCII Digest username is refused, got {other:?}"),
    }
    assert_eq!(
        restream_sync::lock::lock(&origin.seen).len(),
        1,
        "no Digest answer was sent"
    );
}
