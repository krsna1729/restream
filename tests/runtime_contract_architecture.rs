#[test]
fn selected_runtime_contracts_stay_ready_for_extraction() {
    for (name, source) in [
        (
            "runtime/capacity.rs",
            include_str!("../src/runtime/capacity.rs"),
        ),
        ("runtime/graph.rs", include_str!("../src/runtime/graph.rs")),
        (
            "runtime/output.rs",
            include_str!("../src/runtime/output.rs"),
        ),
        ("runtime/stage.rs", include_str!("../src/runtime/stage.rs")),
    ] {
        for forbidden in [
            "crate::media::",
            "crate::application::",
            "crate::api::",
            "restream::media::",
            "restream::application::",
            "restream::api::",
            "sqlx",
            "axum",
            "serde_json::",
            "use serde_json",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} must remain a domain/std/serde-only contract; found {forbidden}"
            );
        }
    }
}

#[test]
fn runtime_module_has_no_upward_compatibility_facades() {
    let runtime_mod = include_str!("../src/runtime/mod.rs");

    assert!(
        !runtime_mod.contains("pub mod snapshots"),
        "runtime must not re-export media-owned snapshot DTOs"
    );
    assert!(
        !runtime_mod.contains("pub mod health"),
        "runtime must not carry an empty health placeholder"
    );
}

#[test]
fn hls_preview_consumes_typed_runtime_state() {
    let hls_preview = include_str!("../src/application/hls_preview.rs");

    assert!(
        !hls_preview.contains("api_runtime_views") && !hls_preview.contains("serde_json"),
        "HLS preview application policy must consume typed runtime state, not API projection JSON"
    );
}
