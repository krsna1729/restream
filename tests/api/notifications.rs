use super::*;

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(auth_req(method, uri, cookie, body))
        .await
        .unwrap();
    let status = resp.status();
    let bytes = body_bytes(resp).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// A local receiver that records each request's signature header and body.
async fn receiver() -> (
    String,
    tokio::sync::mpsc::UnboundedReceiver<(Option<String>, Vec<u8>)>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |headers: axum::http::HeaderMap, body: bytes::Bytes| {
            let tx = tx.clone();
            async move {
                let signature = headers
                    .get("x-restream-signature")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string);
                tx.send((signature, body.to_vec())).unwrap();
                StatusCode::NO_CONTENT
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, rx)
}

#[tokio::test]
async fn a_webhook_test_arrives_signed_with_the_channel_secret_and_is_logged() {
    let (app, admin) = authenticated_app().await;
    let (url, mut received) = receiver().await;
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/notifications/channels",
        &admin,
        Some(&serde_json::json!({ "kind": "webhook", "name": "pager", "target": url, "minSeverity": "critical" }).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["channel"]["id"].as_str().unwrap().to_string();
    let secret = created["secret"].as_str().unwrap().to_string();
    assert!(
        created["channel"].get("secret").is_none(),
        "the channel listing never carries the secret"
    );

    let (status, delivery) = call(
        &app,
        "POST",
        &format!("/api/v1/notifications/channels/{id}/test"),
        &admin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{delivery}");
    assert_eq!(
        (delivery["ok"].as_bool(), delivery["detail"].as_str()),
        (Some(true), Some("HTTP 204"))
    );

    let (signature, body) = received.recv().await.unwrap();
    let signature = signature.expect("signed");
    let (t, v1) = signature
        .strip_prefix("t=")
        .unwrap()
        .split_once(",v1=")
        .unwrap();
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    let expected: Vec<u8> = (0..v1.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&v1[i..i + 2], 16).unwrap())
        .collect();
    ring::hmac::verify(&key, &[t.as_bytes(), b".", &body].concat(), &expected)
        .expect("the receiver can verify it");
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        (json["schema"].as_str(), json["event"].as_str()),
        (Some("restream.notify.v1"), Some("test"))
    );

    let (_, log) = call(
        &app,
        "GET",
        "/api/v1/notifications/deliveries",
        &admin,
        None,
    )
    .await;
    assert_eq!(log["deliveries"][0]["targetName"], "pager");
}

#[tokio::test]
async fn any_member_manages_their_own_push_but_only_an_admin_sees_channels() {
    let (app, admin) = authenticated_app().await;
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/users",
        &admin,
        Some(r#"{"username":"noc","password":"correct-horse-1","role":"viewer"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    r#"{"username":"noc","password":"correct-horse-1"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let viewer = resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    assert_eq!(
        call(&app, "GET", "/api/v1/notifications/channels", &viewer, None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, key) = call(&app, "GET", "/api/v1/notifications/push/key", &viewer, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        key["publicKey"].as_str().unwrap().len(),
        87,
        "an uncompressed P-256 key, base64url"
    );
    // The same key on every read: it is stored, not regenerated.
    assert_eq!(
        call(&app, "GET", "/api/v1/notifications/push/key", &admin, None)
            .await
            .1,
        key
    );

    let subscribe = |endpoint: &str| {
        serde_json::json!({ "endpoint": endpoint, "keys": { "p256dh": "BKEY", "auth": "AUTH" }, "minSeverity": "critical" }).to_string()
    };
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/notifications/push/subscriptions",
        &viewer,
        Some(&subscribe("https://10.0.0.1/x")),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "never an address literal");
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/notifications/push/subscriptions",
        &viewer,
        Some(&subscribe("https://push.example.com/abc")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, mine) = call(
        &app,
        "GET",
        "/api/v1/notifications/push/subscriptions",
        &viewer,
        None,
    )
    .await;
    assert_eq!(mine["subscriptions"].as_array().unwrap().len(), 1);
    let (_, admins) = call(
        &app,
        "GET",
        "/api/v1/notifications/push/subscriptions",
        &admin,
        None,
    )
    .await;
    assert!(
        admins["subscriptions"].as_array().unwrap().is_empty(),
        "each member sees their own"
    );
    let id = created["id"].as_str().unwrap();
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/v1/notifications/push/subscriptions/{id}"),
            &admin,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND,
        "not the admin's to remove"
    );
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/v1/notifications/push/subscriptions/{id}"),
            &viewer,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
}
