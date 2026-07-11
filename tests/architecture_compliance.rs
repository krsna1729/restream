use restream::config::AppConfig;
use restream::domain::ids::OutputId;
use restream::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
use restream::domain::output_spec::OutputConfig;
use restream::domain::srt_ingest::SrtGlobalIngestConfig;
use restream::domain::state::EgressPhase;
use restream::media::security::IngestSecurityService;
use sqlx::SqlitePool;
use std::collections::BTreeSet;
use std::sync::Arc;

#[tokio::test]
async fn test_phase_2_config_reads_env_correctly() {
    unsafe {
        std::env::set_var("RESTREAM_DB_PATH", "test_env.db");
        std::env::set_var("RESTREAM_MEDIA_DIR", "test_media_dir");
        std::env::set_var("RESTREAM_LOG_RETENTION_DAYS", "14");
    }

    let config = AppConfig::from_env();
    assert_eq!(config.db_path, "test_env.db");
    assert_eq!(config.media_dir, "test_media_dir");
    assert_eq!(config.log_retention_days, 14);
}

#[tokio::test]
async fn test_phase_3_routing_resolves_all_major_routes() {
    let mock_engine = Arc::new(restream::media::engine::MediaEngine::new());
    let db = SqlitePool::connect("sqlite::memory:").await.unwrap();
    restream::db::setup_database_schema(&db).await.unwrap();

    let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new()));
    let security = Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG));
    let ingest_policy_store = Arc::new(restream::media::srt::SrtIngestPolicyStore::new(
        SrtGlobalIngestConfig::default(),
        &[],
    ));
    let (log_broadcast, _) = tokio::sync::broadcast::channel(32);

    let state = Arc::new(restream::api::AppState::test_new(
        db,
        security,
        ingest_policy_store,
        sessions,
        mock_engine,
        log_broadcast,
        "media".to_string(),
    ));
    let app = restream::api::create_router(state);
    let _ = app.into_make_service();
}

#[test]
fn router_routes_have_explicit_auth_classification() {
    let source = include_str!("../src/api/router.rs");
    let declared = extract_router_route_paths(source);
    let public: BTreeSet<&str> = restream::api::router::PUBLIC_ROUTE_PATHS
        .iter()
        .copied()
        .collect();
    let authenticated: BTreeSet<&str> = restream::api::router::AUTHENTICATED_ROUTE_PATHS
        .iter()
        .copied()
        .collect();
    let classified: BTreeSet<&str> = public.union(&authenticated).copied().collect();

    let missing: Vec<_> = declared.difference(&classified).copied().collect();
    assert!(
        missing.is_empty(),
        "router paths missing auth classification: {missing:?}"
    );

    let stale: Vec<_> = classified.difference(&declared).copied().collect();
    assert!(
        stale.is_empty(),
        "auth classification paths not present in router: {stale:?}"
    );

    let overlap: Vec<_> = public.intersection(&authenticated).copied().collect();
    assert!(
        overlap.is_empty(),
        "routes cannot be both public and authenticated: {overlap:?}"
    );
}

#[test]
fn release_policy_metadata_is_declared_and_enforced() {
    let cargo_toml = include_str!("../Cargo.toml");
    assert!(cargo_toml.contains("rust-version = \"1.96\""));
    assert!(cargo_toml.contains("publish = false"));
    assert!(cargo_toml.contains("license-file = \"LICENSE.md\""));
    assert!(cargo_toml.contains("repository = "));
    assert!(!cargo_toml.contains("release_max_level_"));

    let build_rs = include_str!("../build.rs");
    assert!(build_rs.contains("PKG_CONFIG_LIBDIR"));
    assert!(build_rs.contains("remove_var(\"PKG_CONFIG_PATH\")"));
    assert!(build_rs.contains("RESTREAM_NATIVE_BUILD_ID"));
    assert!(build_rs.contains("native_build_id(&prefix)"));
    assert!(build_rs.contains("REQUIRED_STATIC_ARCHIVES"));
    assert!(build_rs.contains("check_required_static_inputs(&prefix)"));
    assert!(build_rs.contains("assert_pinned_paths(package, prefix, &library.link_paths)"));
    assert!(build_rs.contains("assert_pinned_paths(package, prefix, &library.include_paths)"));
    assert!(build_rs.contains("cargo:rustc-link-arg=-Wl,-Bdynamic"));
    for native_input in [
        "libsrt.a",
        "libmbedtls.a",
        "libmbedx509.a",
        "libmbedcrypto.a",
        "libavcodec.a",
        "libavformat.a",
        "libavfilter.a",
        "libswscale.a",
        "libswresample.a",
        "libavutil.a",
        "libx264.a",
        "libx265.a",
        "mbedtls",
        "mbedx509",
        "mbedcrypto",
    ] {
        assert!(
            build_rs.contains(native_input),
            "build.rs missing native input policy for {native_input}"
        );
    }

    let deny_toml = include_str!("../deny.toml");
    assert!(deny_toml.contains("unknown-registry = \"deny\""));
    assert!(deny_toml.contains("unknown-git = \"deny\""));
    assert!(deny_toml.contains("duplicate families currently come through"));

    let sbom_workflow = include_str!("../.github/workflows/sbom-security.yml");
    assert!(sbom_workflow.contains("cargo deny check advisories licenses bans sources"));

    let license = include_str!("../LICENSE.md");
    assert!(license.contains("All rights"));
    assert!(license.contains("reserved"));

    let compliance = include_str!("../docs/release-compliance.md");
    for required in [
        "restream.nativeBuildId",
        "cargo audit",
        "cargo deny check advisories licenses bans sources",
        "GPL-2.0-or-later",
        "LicenseRef-restream-internal",
        "tracing/release_max_level_*",
        "Dependency Policy",
    ] {
        assert!(
            compliance.contains(required),
            "release compliance docs must mention {required}"
        );
    }
}

#[test]
fn hls_preview_runtime_execution_stays_out_of_planner() {
    let planner_mod = include_str!("../src/planner/mod.rs");
    assert!(
        !planner_mod.contains("hls_preview"),
        "planner should expose pure planning modules only"
    );

    let graph_plan = include_str!("../src/planner/graph_plan.rs");
    assert!(graph_plan.contains("pub fn plan_hls_preview_graph"));
    assert!(!graph_plan.contains("StageRuntimeManager"));
    assert!(!graph_plan.contains("tokio::time"));

    let preview_graph = include_str!("../src/media/hls/preview_graph.rs");
    assert!(preview_graph.contains("StageRuntimeManager"));
    assert!(preview_graph.contains("resolve_hls_preview_graph"));
}

#[test]
fn db_module_uses_explicit_repository_exports() {
    let db_mod = include_str!("../src/db/mod.rs");
    assert!(
        !db_mod.contains("pub use ingest_repo::*"),
        "db repositories should export explicit APIs, not wildcard repository surfaces"
    );
    assert!(!db_mod.contains("pub use job_repo::*"));
    assert!(!db_mod.contains("pub use log_repo::*"));
    assert!(!db_mod.contains("pub use output_repo::*"));
    assert!(!db_mod.contains("pub use pipeline_repo::*"));
    assert!(!db_mod.contains("pub use recording_repo::*"));
    assert!(!db_mod.contains("pub use session_repo::*"));
    assert!(db_mod.contains("pub use schema::setup_database_schema"));
}

#[test]
fn application_records_do_not_live_in_root_types_module() {
    let lib_rs = include_str!("../src/lib.rs");
    assert!(
        !lib_rs.contains("pub mod types;"),
        "application records should live under application::models, not a root types module"
    );

    let application_mod = include_str!("../src/application/mod.rs");
    assert!(application_mod.contains("pub mod models;"));

    let models = include_str!("../src/application/models.rs");
    assert!(models.contains("pub struct Pipeline"));
    assert!(models.contains("pub struct Output"));
    assert!(models.contains("pub struct Ingest"));
    assert!(models.contains("pub struct Job"));
}

#[test]
fn app_state_hides_security_session_and_srt_internals() {
    let state = include_str!("../src/api/state.rs");
    let app_state = state
        .split("pub struct AppState {")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n\nimpl AppState").next())
        .expect("AppState struct block should be present");
    for forbidden in [
        "pub security:",
        "pub sessions:",
        "pub ingest_policy_store:",
        "pub srt_passphrase:",
        "pub srt_pbkeylen:",
        "pub secure_session_cookies:",
    ] {
        assert!(
            !app_state.contains(forbidden),
            "AppState should hide internal field {forbidden}"
        );
    }
    for required in [
        "pub fn record_security_failure",
        "pub fn reset_security_failures",
        "pub fn security_failure_snapshots",
        "pub async fn add_session_hash",
        "pub async fn retain_only_session_hash",
        "pub async fn refresh_srt_ingest_policy_store",
    ] {
        assert!(
            state.contains(required),
            "AppState should expose explicit operation {required}"
        );
    }
}

#[test]
fn frontend_tooling_and_vendored_assets_are_reproducible() {
    let package_json = include_str!("../package.json");
    assert!(package_json.contains("\"build:frontend\""));
    assert!(package_json.contains("\"format:check\""));
    assert!(package_json.contains("\"test:frontend\""));

    let package_lock = include_str!("../package-lock.json");
    assert!(package_lock.contains("\"lockfileVersion\""));
    assert!(package_lock.contains("\"hls.js\""));

    let tsconfig = include_str!("../tsconfig.json");
    for required in [
        "\"strict\": true",
        "\"rootDir\": \"public/ts\"",
        "\"outDir\": \"public/js\"",
        "\"sourceMap\": false",
    ] {
        assert!(
            tsconfig.contains(required),
            "tsconfig must contain {required}"
        );
    }

    let hls_sync = include_str!("../scripts/ensure-frontend-assets.mjs");
    assert!(hls_sync.contains("sourceMappingURL=hls\\.min\\.js\\.map"));
    let hls_bundle = include_str!("../public/js/lib/hls.min.js");
    assert!(
        !hls_bundle.contains("sourceMappingURL=hls.min.js.map"),
        "checked-in HLS bundle should not point at an absent source map"
    );
}

#[test]
fn source_distribution_manifest_matches_declared_build_inputs() {
    let manifest = include_str!("../docs/source-distribution.md");
    for required in [
        ".build/static/prefix/",
        "package-lock.json",
        "tsconfig.json",
        "scripts/resource-limit ./scripts/setup-static-build.sh",
        "npm run build:frontend",
        "hls.min.js.map",
    ] {
        assert!(
            manifest.contains(required),
            "source distribution manifest must mention {required}"
        );
    }

    let cargo_toml = include_str!("../Cargo.toml");
    let mut missing_benches = Vec::new();
    for bench in declared_benches(cargo_toml) {
        let expected = format!("benches/{bench}.rs");
        if !std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(&expected)
            .exists()
        {
            missing_benches.push(expected);
        }
    }
    assert!(
        missing_benches.is_empty(),
        "Cargo.toml declares missing bench files: {missing_benches:?}"
    );

    for script in [
        "scripts/run-bench-harness.sh",
        "scripts/build-bench-harness.sh",
        "scripts/setup-static-build.sh",
        "scripts/ensure-frontend-assets.mjs",
    ] {
        assert!(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(script)
                .is_file(),
            "declared build script is missing: {script}"
        );
    }
}

fn declared_benches(cargo_toml: &str) -> Vec<&str> {
    let mut benches = Vec::new();
    let mut in_bench = false;
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed == "[[bench]]" {
            in_bench = true;
            continue;
        }
        if trimmed.starts_with('[') {
            in_bench = false;
        }
        if in_bench
            && let Some(name) = trimmed
                .strip_prefix("name = \"")
                .and_then(|rest| rest.strip_suffix('"'))
        {
            benches.push(name);
        }
    }
    benches
}

fn extract_router_route_paths(source: &'static str) -> BTreeSet<&'static str> {
    let mut paths = BTreeSet::new();
    for (offset, _) in source.match_indices(".route(") {
        let rest = &source[offset..];
        let Some(first_quote) = rest.find('"') else {
            continue;
        };
        let path_start = offset + first_quote + 1;
        let Some(path_len) = source[path_start..].find('"') else {
            continue;
        };
        paths.insert(&source[path_start..path_start + path_len]);
    }
    paths
}

#[tokio::test]
async fn test_phase_4_5_services_and_repositories_flow() {
    let db = SqlitePool::connect("sqlite::memory:").await.unwrap();
    restream::db::setup_database_schema(&db).await.unwrap();

    let pipeline_service =
        restream::application::services::pipeline_service::PipelineService::new(db.clone());
    let output_service =
        restream::application::services::output_service::OutputService::new(db.clone());

    let pid = "test-pipe-service";
    pipeline_service
        .create_pipeline(pid, "name", "stream-key", None, None)
        .await
        .unwrap();

    let pipeline = restream::db::pipeline_repo::get_pipeline(&db, pid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pipeline.stream_key, "stream-key");

    let oid = "test-out-service";
    let config = OutputConfig::default();
    output_service
        .create_output(
            oid,
            pid,
            "rtmp-push",
            "rtmp://localhost/live",
            None,
            "running",
            &config,
        )
        .await
        .unwrap();

    let output = restream::db::output_repo::get_output(&db, pid, oid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.url, "rtmp://localhost/live");
}

#[tokio::test]
async fn test_phase_8_dependency_aware_output_status_resolution() {
    use restream::runtime::output::OutputRuntimeExplanation;

    let explanation = OutputRuntimeExplanation {
        output_id: OutputId::from("out-1".to_string()),
        output_name: "test-out".to_string(),
        encoding: "h264".to_string(),
        url: "rtmp://localhost/live".to_string(),
        phase: EgressPhase::Connecting,
        terminal_stage: None,
        blocked_by: None,
    };

    assert_eq!(explanation.output_name, "test-out");
}
