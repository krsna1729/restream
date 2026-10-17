#![allow(clippy::disallowed_methods)] // test code: raw std locks are fine
mod support;

use axum::http::StatusCode;
use http_body_util::BodyExt;
use restream::db;
use tower::ServiceExt;

use support::{authenticated_app, json_request, response_json};

/// The raw response body, so a test can assert a secret never appears in any
/// serialized form (not merely that a named field is absent).
async fn response_text(response: axum::response::Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    String::from_utf8(bytes.to_vec()).expect("utf-8 body")
}

async fn request_body(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<&str>,
) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(json_request(method, uri, Some(cookie), body))
        .await
        .expect("input response");
    let status = response.status();
    (status, response_text(response).await)
}

async fn post_input(
    app: &axum::Router,
    cookie: &str,
    pipeline_id: &str,
    body: &str,
) -> (StatusCode, String) {
    request_body(
        app,
        "POST",
        &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
        cookie,
        Some(body),
    )
    .await
}

async fn patch_input(
    app: &axum::Router,
    cookie: &str,
    pipeline_id: &str,
    input_id: &str,
    body: &str,
) -> (StatusCode, String) {
    request_body(
        app,
        "PATCH",
        &format!("/api/v1/pipelines/{pipeline_id}/inputs/{input_id}"),
        cookie,
        Some(body),
    )
    .await
}

fn parse(body: &str) -> serde_json::Value {
    serde_json::from_str(body).expect("json body")
}

async fn create_pipeline(app: &axum::Router, cookie: &str) -> String {
    let response = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/pipelines",
            Some(cookie),
            Some(r#"{"name":"Event"}"#),
        ))
        .await
        .expect("pipeline response");
    assert_eq!(response.status(), StatusCode::CREATED);
    response_json(response).await["pipeline"]["id"]
        .as_str()
        .expect("pipeline id")
        .to_string()
}

#[tokio::test]
async fn pipeline_inputs_list_exposes_primary_ingest_urls() {
    let (app, pool, cookie) = authenticated_app().await;
    db::set_ingest_host(&pool, "ingest.example.com")
        .await
        .expect("ingest host");
    let pipeline_id = create_pipeline(&app, &cookie).await;

    let response = app
        .clone()
        .oneshot(json_request(
            "GET",
            &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
            Some(&cookie),
            None,
        ))
        .await
        .expect("inputs response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let inputs = body["inputs"].as_array().expect("inputs array");
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0]["role"], "primary");
    assert_eq!(inputs[0]["selected"], true);
    assert_eq!(inputs[0]["runtime"]["connected"], false);
    assert_eq!(
        inputs[0]["previewUrl"],
        format!(
            "/hls/inputs/{}/master.m3u8",
            inputs[0]["id"].as_str().expect("input id")
        )
    );
    let key = inputs[0]["streamKey"].as_str().expect("stream key");
    assert_eq!(
        inputs[0]["ingestUrls"]["rtmp"],
        format!("rtmp://ingest.example.com:1935/live/{key}")
    );
    assert_eq!(
        inputs[0]["ingestUrls"]["srt"],
        format!("srt://ingest.example.com:10080?streamid=publish:{key}")
    );
}

#[tokio::test]
async fn adding_inputs_enforces_pipeline_limit_and_unique_credentials() {
    let (app, _, cookie) = authenticated_app().await;
    let pipeline_id = create_pipeline(&app, &cookie).await;

    let mut keys = Vec::new();
    for label in ["Encoder B", "Encoder C", "Encoder D"] {
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
                Some(&cookie),
                Some(&serde_json::json!({ "label": label }).to_string()),
            ))
            .await
            .expect("create input response");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response_json(response).await;
        keys.push(
            body["input"]["streamKey"]
                .as_str()
                .expect("stream key")
                .to_string(),
        );
    }

    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 3);
    let response = app
        .clone()
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
            Some(&cookie),
            Some(r#"{"label":"Encoder E"}"#),
        ))
        .await
        .expect("limit response");
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn promotion_moves_selection_without_changing_input_role() {
    let (app, _, cookie) = authenticated_app().await;
    let pipeline_id = create_pipeline(&app, &cookie).await;
    let create_response = app
        .clone()
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
            Some(&cookie),
            Some(r#"{"label":"Backup encoder"}"#),
        ))
        .await
        .expect("create input response");
    let created = response_json(create_response).await;
    let input_id = created["input"]["id"]
        .as_str()
        .expect("input id")
        .to_string();

    let response = app
        .clone()
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/pipelines/{pipeline_id}/inputs/{input_id}/promote"),
            Some(&cookie),
            None,
        ))
        .await
        .expect("promotion response");

    assert_eq!(response.status(), StatusCode::OK);
    let promoted = response_json(response).await;
    assert_eq!(promoted["input"]["selected"], true);
    assert_eq!(promoted["input"]["role"], "backup");
    assert_eq!(promoted["connected"], false);
    let delete_response = app
        .clone()
        .oneshot(json_request(
            "DELETE",
            &format!("/api/v1/pipelines/{pipeline_id}/inputs/{input_id}"),
            Some(&cookie),
            None,
        ))
        .await
        .expect("delete response");
    assert_eq!(delete_response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn keyed_input_create_still_behaves_as_before() {
    let (app, _, cookie) = authenticated_app().await;
    let pipeline_id = create_pipeline(&app, &cookie).await;

    let (status, body) = post_input(
        &app,
        &cookie,
        &pipeline_id,
        r#"{"label":"Venue encoder B"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let value = parse(&body);
    let input = &value["input"];
    assert_eq!(input["kind"], "keyed");
    assert_eq!(input["sourceUrl"], serde_json::Value::Null);
    assert_eq!(input["variantRule"], "highestBandwidth");
    assert_eq!(input["authKind"], "none");
    assert_eq!(input["authUsername"], serde_json::Value::Null);
    assert!(input["ingestUrls"]["rtmp"].is_string());
    assert!(input["ingestUrls"]["srt"].is_string());
    assert!(!input["streamKey"].as_str().expect("stream key").is_empty());
}

#[tokio::test]
async fn pull_input_round_trips_and_never_returns_credentials() {
    let (app, _, cookie) = authenticated_app().await;
    let pipeline_id = create_pipeline(&app, &cookie).await;

    let (status, body) = post_input(
        &app,
        &cookie,
        &pipeline_id,
        r#"{"label":"Origin","kind":"hlsPull","sourceUrl":"https://origin.example/live/master.m3u8","variantRule":"lowestBandwidth","auth":{"kind":"bearer","token":"super-secret-token"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(
        !body.contains("super-secret-token"),
        "a bearer token must never appear in a response body"
    );

    let value = parse(&body);
    let input = &value["input"];
    assert_eq!(input["kind"], "hlsPull");
    assert_eq!(
        input["sourceUrl"],
        "https://origin.example/live/master.m3u8"
    );
    assert_eq!(input["variantRule"], "lowestBandwidth");
    assert_eq!(input["authKind"], "bearer");
    assert_eq!(input["authUsername"], serde_json::Value::Null);
    assert_eq!(input["ingestUrls"], serde_json::Value::Null);
    assert!(
        input["previewUrl"]
            .as_str()
            .expect("preview url")
            .ends_with("/master.m3u8")
    );
    let input_id = input["id"].as_str().expect("input id").to_string();

    let (status, body) = request_body(
        &app,
        "GET",
        &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("super-secret-token"),
        "a bearer token must never appear in a list response body"
    );
    let listed = parse(&body);
    let pull = listed["inputs"]
        .as_array()
        .expect("inputs array")
        .iter()
        .find(|input| input["id"] == input_id.as_str())
        .expect("pull input listed");
    assert_eq!(pull["kind"], "hlsPull");
    assert_eq!(pull["sourceUrl"], "https://origin.example/live/master.m3u8");
    assert_eq!(pull["variantRule"], "lowestBandwidth");
    assert_eq!(pull["authKind"], "bearer");
    assert_eq!(pull["ingestUrls"], serde_json::Value::Null);
}

#[tokio::test]
async fn pull_input_header_auth_values_never_appear_in_responses() {
    let (app, _, cookie) = authenticated_app().await;
    let pipeline_id = create_pipeline(&app, &cookie).await;

    let (status, body) = post_input(
        &app,
        &cookie,
        &pipeline_id,
        r#"{"label":"Origin","kind":"hlsPull","sourceUrl":"https://origin.example/master.m3u8","auth":{"kind":"headers","headers":[{"name":"Cookie","value":"session=header-secret"}]}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(
        !body.contains("header-secret"),
        "a header value is a credential and must never appear in a response body"
    );

    let value = parse(&body);
    let input = &value["input"];
    assert_eq!(input["authKind"], "headers");
    assert_eq!(input["authUsername"], serde_json::Value::Null);
    assert_eq!(input["sourceUrl"], "https://origin.example/master.m3u8");
}

/// A signed-cookie origin is configured by pasting the cookie string, which
/// is a credential like any other: stored, reported as a kind, and never
/// returned.
#[tokio::test]
async fn pull_input_cookie_auth_round_trips_without_returning_the_string() {
    let (app, _, cookie) = authenticated_app().await;
    let pipeline_id = create_pipeline(&app, &cookie).await;

    let (status, body) = post_input(
        &app,
        &cookie,
        &pipeline_id,
        r#"{"label":"Signed","kind":"hlsPull","sourceUrl":"https://signed.example/master.m3u8","auth":{"kind":"cookies","cookies":"CloudFront-Policy=p;CloudFront-Signature=cookie-secret;CloudFront-Key-Pair-Id=k"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        !body.contains("cookie-secret"),
        "a cookie string is a credential and must never appear in a response body"
    );

    let value = parse(&body);
    let input_id = value["input"]["id"].as_str().unwrap().to_string();
    assert_eq!(value["input"]["authKind"], "cookies");
    assert_eq!(value["input"]["authUsername"], serde_json::Value::Null);

    // The list must not return it either: the whole row is serialized there.
    let response = app
        .clone()
        .oneshot(json_request(
            "GET",
            &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
            Some(&cookie),
            None,
        ))
        .await
        .expect("inputs response");
    assert_eq!(response.status(), StatusCode::OK);
    let list = response_text(response).await;
    assert!(
        !list.contains("cookie-secret"),
        "the list must not carry a stored cookie string"
    );

    // The kind survives an update that does not mention auth.
    let (status, body) = patch_input(
        &app,
        &cookie,
        &pipeline_id,
        &input_id,
        r#"{"enabled":false}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["input"]["authKind"], "cookies");
    assert!(!body.contains("cookie-secret"));
}

#[tokio::test]
async fn patch_keeps_replaces_and_clears_pull_auth() {
    let (app, _, cookie) = authenticated_app().await;
    let pipeline_id = create_pipeline(&app, &cookie).await;

    let (_, body) = post_input(
        &app,
        &cookie,
        &pipeline_id,
        r#"{"label":"Origin","kind":"hlsPull","sourceUrl":"https://origin.example/master.m3u8","auth":{"kind":"bearer","token":"first-token"}}"#,
    )
    .await;
    let input_id = parse(&body)["input"]["id"]
        .as_str()
        .expect("input id")
        .to_string();

    // `auth` absent keeps the stored credential, and the stored URL, too.
    let (status, body) = patch_input(
        &app,
        &cookie,
        &pipeline_id,
        &input_id,
        r#"{"label":"Renamed"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("first-token"));
    let value = parse(&body);
    let input = &value["input"];
    assert_eq!(input["label"], "Renamed");
    assert_eq!(input["kind"], "hlsPull");
    assert_eq!(input["sourceUrl"], "https://origin.example/master.m3u8");
    assert_eq!(input["authKind"], "bearer");

    // A different variant replaces it wholesale.
    let (status, body) = patch_input(
        &app,
        &cookie,
        &pipeline_id,
        &input_id,
        r#"{"auth":{"kind":"basic","username":"operator","password":"patch-password"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("patch-password"));
    let value = parse(&body);
    let input = &value["input"];
    assert_eq!(input["authKind"], "basic");
    assert_eq!(input["authUsername"], "operator");

    // `{"kind":"none"}` clears it.
    let (status, body) = patch_input(
        &app,
        &cookie,
        &pipeline_id,
        &input_id,
        r#"{"auth":{"kind":"none"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value = parse(&body);
    let input = &value["input"];
    assert_eq!(input["authKind"], "none");
    assert_eq!(input["authUsername"], serde_json::Value::Null);
}

#[tokio::test]
async fn pull_input_validation_rejects_bad_configuration() {
    let (app, _, cookie) = authenticated_app().await;
    let pipeline_id = create_pipeline(&app, &cookie).await;

    let long_url = format!("https://origin.example/{}", "a".repeat(2048));
    let long_name = format!("x-{}", "a".repeat(64));
    let long_value = "v".repeat(1025);
    let many_headers: Vec<serde_json::Value> = (0..9)
        .map(|index| serde_json::json!({"name": format!("x-h{index}"), "value": "v"}))
        .collect();

    let cases = [
        (
            serde_json::json!({"label":"x","kind":"yolo","sourceUrl":"https://o.example/p.m3u8"}),
            "unknown kind is rejected rather than defaulting",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","variantRule":"bogus"}),
            "unknown variant rule is rejected",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull"}),
            "a pull input requires sourceUrl",
        ),
        (
            serde_json::json!({"label":"x","kind":"keyed","sourceUrl":"https://o.example/p.m3u8"}),
            "a keyed input cannot carry sourceUrl",
        ),
        (
            serde_json::json!({"label":"x","kind":"keyed","auth":{"kind":"bearer","token":"t"}}),
            "a keyed input cannot carry auth",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"ftp://o.example/p.m3u8"}),
            "scheme must be http or https",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"not a url"}),
            "sourceUrl must be absolute",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":long_url}),
            "sourceUrl must fit MAX_URL_LEN",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"yolo"}}),
            "unknown auth kind is rejected",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"bearer"}}),
            "bearer auth requires a token",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"basic","username":"u"}}),
            "basic auth requires a password",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"none","token":"t"}}),
            "none auth carries no payload",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"headers","headers":many_headers}}),
            "at most eight headers",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"headers","headers":[{"name":"bad name","value":"v"}]}}),
            "header name must be an HTTP token",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"headers","headers":[{"name":long_name,"value":"v"}]}}),
            "header name must fit MAX_HEADER_NAME_LEN",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"headers","headers":[{"name":"x-token","value":long_value}]}}),
            "header value must fit MAX_HEADER_VALUE_LEN",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"headers","headers":[{"name":"x-token","value":"a\r\nb"}]}}),
            "header value must not contain CR or LF",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"cookies"}}),
            "a cookies auth requires the cookie string",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"cookies","cookies":""}}),
            "a cookie string must not be empty",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"cookies","cookies":"a=1\r\nb=2"}}),
            "a cookie string must be one header line",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"bearer","cookies":"a=1"}}),
            "bearer auth carries no cookie string",
        ),
        // A header the client itself sets: accepting it would break framing in
        // ways the operator cannot see from the dashboard.
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"headers","headers":[{"name":"Host","value":"evil.example"}]}}),
            "a header that belongs to the connection is refused",
        ),
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"headers","headers":[{"name":"Content-Length","value":"5"}]}}),
            "a framing header is refused whatever its case",
        ),
        // `HeaderValue` refuses DEL, and the puller would refuse it on every
        // request, so the save must not accept it.
        (
            serde_json::json!({"label":"x","kind":"hlsPull","sourceUrl":"https://o.example/p.m3u8","auth":{"kind":"headers","headers":[{"name":"x-token","value":"bad\u{7f}"}]}}),
            "a header value the HTTP client refuses is refused at save",
        ),
    ];

    for (body, description) in cases {
        let (status, response) = post_input(&app, &cookie, &pipeline_id, &body.to_string()).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "expected 400 for {description}; got {status} {response}"
        );
        assert!(
            parse(&response)["error"].is_string(),
            "expected a JSON error message for {description}"
        );
    }
}

#[tokio::test]
async fn patch_validation_rejects_keyed_input_carrying_a_pull_source() {
    let (app, _, cookie) = authenticated_app().await;
    let pipeline_id = create_pipeline(&app, &cookie).await;

    let response = app
        .clone()
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
            Some(&cookie),
            Some(r#"{"label":"Backup"}"#),
        ))
        .await
        .expect("create input response");
    let created = response_json(response).await;
    let input_id = created["input"]["id"]
        .as_str()
        .expect("input id")
        .to_string();

    let (status, response) = patch_input(
        &app,
        &cookie,
        &pipeline_id,
        &input_id,
        r#"{"sourceUrl":"https://o.example/p.m3u8"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
}
