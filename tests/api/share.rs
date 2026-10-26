use super::*;

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Content-Type", "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header("Cookie", cookie);
    }
    let resp = app
        .clone()
        .oneshot(
            builder
                .body(axum::body::Body::from(body.unwrap_or("").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    (status, headers, body_bytes(resp).await.to_vec())
}

#[tokio::test]
async fn a_share_link_opens_its_pipeline_preview_to_anyone_until_revoked() {
    let (app, cookie) = authenticated_app().await;
    let (_, _, created) = call(
        &app,
        "POST",
        "/api/v1/pipelines",
        Some(&cookie),
        Some(r#"{"name":"Lobby cam","streamKey":"key_share_123456789"}"#),
    )
    .await;
    let pipeline = serde_json::from_slice::<serde_json::Value>(&created).unwrap()["pipeline"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, _, other) = call(
        &app,
        "POST",
        "/api/v1/pipelines",
        Some(&cookie),
        Some(r#"{"name":"Private","streamKey":"key_share_223456789"}"#),
    )
    .await;
    let other = serde_json::from_slice::<serde_json::Value>(&other).unwrap()["pipeline"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _, body) = call(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipeline}/share-links"),
        Some(&cookie),
        Some(r#"{"label":"Website","lifetimeHours":24}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let link: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let token = link["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 48);

    // The watch page: public, embeddable, names the pipeline, counts a view.
    let (status, headers, page) = call(&app, "GET", &format!("/watch/{token}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::CONTENT_SECURITY_POLICY],
        "frame-ancestors *"
    );
    let page = String::from_utf8(page).unwrap();
    assert!(page.contains("Lobby cam"));
    assert!(page.contains(&format!("../hls/{pipeline}/master.m3u8?t={token}")));
    // The player it imports is served to a viewer with no session.
    let player = page
        .split("import Hls from \"../")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("the watch page imports hls.js");
    let (status, _, script) = call(&app, "GET", &format!("/{player}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "{player}");
    assert!(String::from_utf8_lossy(&script).contains("isSupported"));
    let (_, _, listed) = call(
        &app,
        "GET",
        &format!("/api/v1/pipelines/{pipeline}/share-links"),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&listed).unwrap()["links"][0]["views"],
        1
    );

    // HLS: the token admits its own pipeline, not another, and nothing
    // without it. (No publisher: past the gate, the preview is simply absent.)
    let gate = |status: StatusCode| status != StatusCode::UNAUTHORIZED;
    assert!(gate(
        call(
            &app,
            "GET",
            &format!("/hls/{pipeline}/master.m3u8?t={token}"),
            None,
            None
        )
        .await
        .0
    ));
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/hls/{pipeline}/master.m3u8"),
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/hls/{other}/master.m3u8?t={token}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/hls/{pipeline}/master.m3u8?t=nope"),
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );

    // Revoked: the page and the stream close at once.
    let id = link["id"].as_str().unwrap();
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/v1/share-links/{id}"),
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", &format!("/watch/{token}"), None, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/hls/{pipeline}/master.m3u8?t={token}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn only_an_admin_makes_share_links() {
    let (app, cookie) = authenticated_app().await;
    let (_, _, created) = call(
        &app,
        "POST",
        "/api/v1/pipelines",
        Some(&cookie),
        Some(r#"{"name":"P","streamKey":"key_share_323456789"}"#),
    )
    .await;
    let pipeline = serde_json::from_slice::<serde_json::Value>(&created).unwrap()["pipeline"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        &app,
        "POST",
        "/api/v1/auth/view-as",
        Some(&cookie),
        Some(r#"{"role":"operator"}"#),
    )
    .await;
    let (status, _, _) = call(
        &app,
        "POST",
        &format!("/api/v1/pipelines/{pipeline}/share-links"),
        Some(&cookie),
        Some(r#"{"label":"x"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/v1/pipelines/{pipeline}/share-links"),
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
