use super::*;

#[tokio::test]
async fn status_returns_version_info() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let engine = &json;
    assert!(engine["restream"]["version"].is_string());
    assert!(engine["restream"]["commit"].is_string());
    assert!(engine["restream"]["buildTimestamp"].is_string());
    assert!(engine["restream"]["nativeBuildId"].is_string());
    assert_ne!(
        engine["restream"]["nativeBuildId"], engine["restream"]["commit"],
        "native build id must identify native inputs, not reuse the source commit"
    );
    assert!(engine.get("ffmpeg").is_none());
    assert!(engine["toolchain"]["rustc"].is_string());
    assert!(engine["nativeLibraries"]["ffmpeg"]["version"].is_string());
    assert!(engine["nativeLibraries"]["ffmpeg"]["configuration"].is_string());
    assert!(engine["nativeLibraries"]["srt"]["version"].is_string());
    assert!(engine["nativeLibraries"]["mbedtls"]["version"].is_string());
    assert!(engine["nativeLibraries"]["sqlite"]["version"].is_string());
    assert!(engine["nativeLibraries"]["x264"]["version"].is_string());
    assert!(engine["nativeLibraries"]["x265"]["version"].is_string());
    assert_eq!(engine["sbom"]["format"], "CycloneDX");
    assert_eq!(engine["sbom"]["specVersion"], "1.5");
    assert_eq!(engine["sbom"]["licensesIncluded"], true);
    assert!(engine["sbom"]["componentCount"].as_u64().unwrap() > 20);
    assert!(engine["os"]["platform"].is_string());
    assert!(engine["os"]["hostname"].is_string());
    assert!(engine["os"]["cpu"]["logicalCpus"].as_u64().unwrap() > 0);
    assert!(engine["os"]["cpu"]["flags"].is_array());
}

#[tokio::test]
async fn status_sbom_is_authenticated_cyclonedx_with_licenses() {
    let (app, _) = test_app().await;

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/engine/sbom")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let cookie = login(&app).await;
    let response = app
        .oneshot(auth_req("GET", "/api/v1/engine/sbom", &cookie, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/vnd.cyclonedx+json; version=1.5"
    );
    let json = body_json(response).await;

    assert_eq!(json["bomFormat"], "CycloneDX");
    assert_eq!(json["specVersion"], "1.5");
    assert_eq!(json["metadata"]["component"]["name"], "restream");
    assert_eq!(
        json["metadata"]["component"]["licenses"][0]["expression"],
        "MIT"
    );
    assert_eq!(
        json["metadata"]["component"]["properties"]
            .as_array()
            .unwrap()
            .iter()
            .find(|property| property["name"] == "restream:nativeBuildId")
            .unwrap()["value"],
        json["metadata"]["properties"]
            .as_array()
            .unwrap()
            .iter()
            .find(|property| property["name"] == "restream:nativeBuildId")
            .unwrap()["value"]
    );

    let components = json["components"].as_array().unwrap();
    assert!(components.len() > 20);
    assert!(components.iter().all(|component| {
        component["licenses"]
            .as_array()
            .is_some_and(|licenses| !licenses.is_empty())
    }));
    assert!(
        !components
            .iter()
            .any(|component| component["name"] == "criterion")
    );
    assert!(
        !components
            .iter()
            .any(|component| component["name"] == "pulp")
    );
    for build_only in ["proc-macro2", "quote", "serde_derive", "syn"] {
        assert!(
            !components
                .iter()
                .any(|component| component["name"] == build_only),
            "build-only crate leaked into runtime SBOM: {build_only}"
        );
    }
    assert!(!components.iter().any(|component| {
        component["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("windows-"))
    }));
    for name in [
        "libavcodec",
        "libavformat",
        "libavfilter",
        "libswscale",
        "libswresample",
        "libavutil",
        "libsrt",
        "libmbedtls",
        "libmbedx509",
        "libmbedcrypto",
        "SQLite",
        "x264",
        "x265",
        "libstdc++",
        "libgcc",
        "Rust standard library",
        "tokio",
        "axum",
        "sqlx",
    ] {
        let component = components
            .iter()
            .find(|component| component["name"] == name)
            .unwrap_or_else(|| panic!("missing SBOM component {name}"));
        assert!(component["version"].is_string());
        assert!(
            component["licenses"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
        );
    }

    for (name, expected_inputs) in [
        ("libsrt", &["lib/libsrt.a", "lib/pkgconfig/srt.pc"][..]),
        (
            "libavcodec",
            &["lib/libavcodec.a", "lib/pkgconfig/libavcodec.pc"][..],
        ),
        (
            "libmbedcrypto",
            &["lib/libmbedcrypto.a", "lib/pkgconfig/mbedcrypto.pc"][..],
        ),
        ("x264", &["lib/libx264.a", "lib/pkgconfig/x264.pc"][..]),
    ] {
        let component = components
            .iter()
            .find(|component| component["name"] == name)
            .unwrap_or_else(|| panic!("missing native SBOM component {name}"));
        assert!(
            component["hashes"]
                .as_array()
                .is_some_and(|hashes| hashes.iter().any(|hash| {
                    hash["alg"] == "SHA-256"
                        && hash["content"]
                            .as_str()
                            .is_some_and(|content| content.len() == 64)
                })),
            "native component {name} should include a static archive SHA-256 hash"
        );
        let properties = component["properties"].as_array().unwrap();
        for input in expected_inputs {
            assert!(
                properties
                    .iter()
                    .any(|property| property["name"] == "restream:nativeInput"
                        && property["value"] == *input),
                "native component {name} should list input {input}"
            );
            assert!(
                properties.iter().any(|property| {
                    property["name"] == "restream:nativeInputSha256"
                        && property["value"]
                            .as_str()
                            .is_some_and(|value| value.starts_with(&format!("{input}=")))
                }),
                "native component {name} should list input hash for {input}"
            );
        }
    }

    let dependencies = json["dependencies"].as_array().unwrap();
    let app_ref = json["metadata"]["component"]["bom-ref"].as_str().unwrap();
    assert!(
        dependencies.iter().any(|dependency| {
            dependency["ref"] == app_ref
                && dependency["dependsOn"].as_array().is_some_and(|refs| {
                    refs.iter().any(|reference| {
                        reference
                            .as_str()
                            .is_some_and(|reference| reference.starts_with("native:libsrt@"))
                    })
                })
        }),
        "SBOM dependencies should link the application to native components"
    );
    let libsrt_ref = components
        .iter()
        .find(|component| component["name"] == "libsrt")
        .unwrap()["bom-ref"]
        .as_str()
        .unwrap();
    assert!(
        dependencies.iter().any(|dependency| {
            dependency["ref"] == libsrt_ref
                && dependency["dependsOn"].as_array().is_some_and(|refs| {
                    refs.iter().any(|reference| {
                        reference
                            .as_str()
                            .is_some_and(|reference| reference.starts_with("native:libmbedcrypto@"))
                    })
                })
        }),
        "SBOM dependencies should link libsrt to Mbed TLS crypto"
    );

    let cargo_component = components
        .iter()
        .find(|component| component["name"] == "tokio")
        .expect("tokio should be present in runtime SBOM");
    assert!(
        cargo_component["hashes"]
            .as_array()
            .is_some_and(|hashes| hashes.iter().any(|hash| hash["alg"] == "SHA-256")),
        "Cargo runtime components should include lockfile checksums"
    );
}

// --- Reconciler backoff unit test ---

#[test]
fn reconciler_exponential_backoff_values() {
    // Verify the backoff formula: min(5 * 2^retries, 300) seconds
    // retries=1 → 10s, retries=2 → 20s, retries=3 → 40s, retries=4 → 80s,
    // retries=5 → 160s, retries=6 → 320 → capped at 300s
    let backoff = |retries: u32| -> u64 { (5u64 << retries.min(6)).min(300) };
    assert_eq!(backoff(1), 10);
    assert_eq!(backoff(2), 20);
    assert_eq!(backoff(3), 40);
    assert_eq!(backoff(4), 80);
    assert_eq!(backoff(5), 160);
    assert_eq!(backoff(6), 300); // 5*64=320 capped to 300
    assert_eq!(backoff(7), 300); // min(6) saturates
    assert_eq!(backoff(10), 300);
}

// ─── Engineer telemetry endpoint tests ──────────────────────────────────────

#[tokio::test]
async fn engine_telemetry_returns_structured_response() {
    let (app, cookie) = authenticated_app().await;

    let resp = app
        .oneshot(auth_req("GET", "/api/v1/engine/telemetry", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert!(body["generatedAt"].is_string());
    assert!(body["ingests"].is_array());
    assert!(body["stages"].is_array());
    assert!(body["egresses"].is_array());
    assert!(body["activeTranscoderBuffers"].is_number());
}

#[tokio::test]
async fn pipeline_telemetry_returns_structured_response() {
    let (app, cookie) = authenticated_app().await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"TelPipe","streamKey":"telkey_6c71124cde80358ca7c13081"}"#),
        ))
        .await
        .unwrap();
    let pipe = body_json(resp).await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap();

    let resp = app
        .oneshot(auth_req(
            "GET",
            &format!("/api/v1/pipelines/{pid}/telemetry"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert!(body["generatedAt"].is_string());
    assert_eq!(body["pipelineId"].as_str().unwrap(), pid);
    assert!(body["stages"].is_array());
    assert!(body["egresses"].is_array());
}

#[tokio::test]
async fn engine_telemetry_requires_auth() {
    let (app, _) = authenticated_app().await;

    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/api/v1/engine/telemetry")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn stage_telemetry_returns_structured_response() {
    let (app, _, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;
    let stage_key = StageKey::new("telemetry-pipe", StageKind::video_preset("720p"));
    let metrics = engine.get_or_create_stage_metrics(stage_key.clone()).await;
    metrics.record_in(123);
    metrics.record_out(45);
    metrics.record_processing(9);

    let resp = app
        .oneshot(auth_req(
            "GET",
            &format!("/api/v1/stages/{stage_key}/telemetry"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert!(body["generatedAt"].is_string());
    assert_eq!(body["stageKey"].as_str().unwrap(), stage_key.to_string());
    assert_eq!(body["pipelineId"].as_str().unwrap(), "telemetry-pipe");
    assert_eq!(body["kind"].as_str().unwrap(), "video:720p");
    assert_eq!(body["metrics"]["packetsIn"].as_u64().unwrap(), 1);
    assert_eq!(body["metrics"]["packetsOut"].as_u64().unwrap(), 1);
    assert_eq!(body["metrics"]["bytesIn"].as_u64().unwrap(), 123);
    assert_eq!(body["metrics"]["bytesOut"].as_u64().unwrap(), 45);
    assert_eq!(body["metrics"]["processingUs"].as_u64().unwrap(), 9);
}

#[tokio::test]
async fn stage_telemetry_returns_404_for_unknown_stage() {
    let (app, cookie) = authenticated_app().await;

    let resp = app
        .oneshot(auth_req(
            "GET",
            "/api/v1/stages/nonexistent:source/telemetry",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[cfg(not(feature = "agent-plane"))]
#[tokio::test]
async fn pipeline_alerts_requires_auth_and_returns_array() {
    let (app, cookie) = authenticated_app().await;

    let unauth = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/pipelines/nonexistent/alerts")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let pipeline = body_json(
        app.clone()
            .oneshot(auth_req(
                "POST",
                "/api/v1/pipelines",
                &cookie,
                Some(r#"{"name":"alert-test","streamKey":"sk-alert"}"#),
            ))
            .await
            .unwrap(),
    )
    .await;
    let pid = pipeline["pipeline"]["id"].as_str().unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            &format!("/api/v1/pipelines/{pid}/alerts"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["alerts"].is_array());
    assert!(body["generatedAt"].is_string());
}

#[tokio::test]
async fn aggregate_alerts_requires_auth_and_returns_array() {
    let (app, cookie) = authenticated_app().await;

    let unauth = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/alerts")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/alerts", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["alerts"].is_array());
    assert!(body["generatedAt"].is_string());
}

// ── coverage gap: metrics/system ────────────────────────────────────────

#[tokio::test]
async fn metrics_system_requires_auth_and_returns_structured_data() {
    let (app, cookie) = authenticated_app().await;

    let unauth = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics/system")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/metrics/system", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["cpu"]["usagePercent"].is_number());
    assert!(body["cpu"]["cores"].is_number());
    assert!(body["memory"]["totalBytes"].is_number());
    assert!(body["memory"]["usedBytes"].is_number());
    assert!(body["disk"]["totalBytes"].is_number());
    assert!(body["generatedAt"].is_string());
}

#[tokio::test]
async fn engine_resource_map_requires_auth_and_returns_structured_data() {
    let (app, cookie) = authenticated_app().await;

    let unauth = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/engine/resource-map")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/engine/resource-map",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["scope"]["kind"].as_str(), Some("runtime"));
    assert_eq!(body["view"].as_str(), Some("grouped"));
    assert_eq!(body["limits"]["topN"].as_u64(), Some(25));
    assert!(body["limits"]["totalNodeCount"].is_number());
    assert!(body["limits"]["truncatedNodeCount"].is_number());
    assert!(body["memoryAccounting"].is_null());
    assert!(body["summary"]["processThreadCount"].is_number());
    assert!(body["summary"]["srtSenderThreads"].is_number());
    assert!(body["nodes"].as_array().is_some_and(|nodes| {
        nodes
            .iter()
            .any(|node| node["memory"]["confidence"].as_str() == Some("measured"))
    }));
    assert!(body["attribution"]["derived"].is_array());

    let detail = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/engine/resource-map?view=detail&top_n=1",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let detail_body = body_json(detail).await;
    assert_eq!(detail_body["view"].as_str(), Some("detail"));
    assert_eq!(detail_body["limits"]["topN"].as_u64(), Some(1));
    assert!(
        detail_body["nodes"]
            .as_array()
            .is_some_and(|nodes| nodes.len() <= 1)
    );
    assert!(detail_body["memoryAccounting"].is_object());
}
