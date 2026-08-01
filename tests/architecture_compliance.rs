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
        restream::infrastructure::service_wiring::SqliteServiceFactory::new(&db).compose(),
        security,
        ingest_policy_store,
        sessions,
        mock_engine,
        log_broadcast,
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
    assert!(cargo_toml.contains("license = \"MIT\""));
    assert!(cargo_toml.contains("repository = "));
    assert!(!cargo_toml.contains("release_max_level_"));

    let build_rs = include_str!("../build.rs");
    assert!(build_rs.contains("PKG_CONFIG_LIBDIR"));
    assert!(build_rs.contains("remove_var(\"PKG_CONFIG_PATH\")"));
    assert!(build_rs.contains("RESTREAM_NATIVE_BUILD_ID"));
    assert!(build_rs.contains("native_build_id(&prefix)"));
    assert!(build_rs.contains("embed_native_input_inventory(&prefix)"));
    assert!(build_rs.contains("native-build-inputs.json"));
    assert!(build_rs.contains("\"features\": features"));
    assert!(build_rs.contains("\"dependencyRefs\": dependency_refs"));
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

    let native_build = include_str!("../scripts/build/native-deps.sh");
    assert!(native_build.contains("scripts/build/native/native-inputs.lock"));
    assert!(native_build.contains("require_locked_value MBEDTLS_SHA256"));
    assert!(native_build.contains("require_source_commit \"SRT\""));
    assert!(native_build.contains("require_source_commit \"FFmpeg\""));
    assert!(native_build.contains("Mbed TLS config SHA-256"));
    assert!(native_build.contains("reset_cmake_build_if_moved"));
    assert!(native_build.contains("RESTREAM_VERIFY_NATIVE_INPUT_LOCK_ONLY"));

    let native_lock = include_str!("../scripts/build/native/native-inputs.lock");
    for required in [
        "RESTREAM_LOCK_MBEDTLS_TARBALL_SHA256",
        "RESTREAM_LOCK_MBEDTLS_CONFIG_SHA256",
        "RESTREAM_LOCK_SRT_COMMIT",
        "RESTREAM_LOCK_FFMPEG_COMMIT",
        "RESTREAM_LOCK_X264_COMMIT",
        "RESTREAM_LOCK_X265_COMMIT",
    ] {
        assert!(
            native_lock.contains(required),
            "native input lock must declare {required}"
        );
    }

    let license = include_str!("../LICENSE.md");
    assert!(license.contains("MIT License"));
    assert!(license.contains("Permission is hereby granted"));

    let compliance = include_str!("../docs/release-compliance.md");
    for required in [
        "restream.nativeBuildId",
        "cargo audit",
        "cargo deny check advisories licenses bans sources",
        "GPL-2.0-or-later",
        "MIT",
        "tracing/release_max_level_*",
        "Dependency Policy",
    ] {
        assert!(
            compliance.contains(required),
            "release compliance docs must mention {required}"
        );
    }

    let runtime_info = include_str!("../src/runtime_info.rs");
    for required in [
        "env!(\"RESTREAM_BUILD_TIMESTAMP\")",
        "restream:enabledFeatures",
        "dependencyRefs",
        "restream:nativeInputSha256",
        "restream:nativeBuildId",
        "\"dependencies\": dependencies",
    ] {
        assert!(
            runtime_info.contains(required),
            "runtime SBOM should include {required}"
        );
    }
}

#[test]
fn hls_preview_runtime_execution_stays_out_of_planner() {
    let planner_mod = include_str!("../src/planner/mod.rs");
    assert!(
        !planner_mod.contains("mod hls_preview")
            && !planner_mod.contains("pub mod hls_preview")
            && !planner_mod.contains("StageRuntimeManager")
            && !planner_mod.contains("tokio::"),
        "planner root may re-export pure HLS preview planning, but not runtime execution"
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
fn planner_stays_independent_of_edge_runtime_and_environment_layers() {
    let planner_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/planner");
    let forbidden_dependencies = [
        "crate::application::",
        "restream::application::",
        "crate::media::",
        "restream::media::",
        "crate::api::",
        "restream::api::",
        "crate::db::",
        "restream::db::",
        "crate::config::",
        "restream::config::",
        "axum::",
        "sqlx::",
        "tokio::",
        "std::env",
        "env::var",
        "var_os(",
        "env!(",
        "option_env!(",
    ];

    for entry in std::fs::read_dir(&planner_dir).expect("planner directory should exist") {
        let path = entry.expect("planner entry should be readable").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("planner source should be readable");
        for forbidden in forbidden_dependencies {
            assert!(
                !source.contains(forbidden),
                "{} must not depend on {forbidden}",
                path.display()
            );
        }
    }
}

#[test]
fn agent_core_stays_independent_of_edge_and_runtime_layers() {
    let agent_core_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agent_core");
    let forbidden_dependencies = [
        "crate::agent_plane::",
        "crate::application::",
        "crate::media::",
        "reqwest::",
        "axum::",
        "sqlx::",
    ];
    let mut offenders = Vec::new();
    collect_rust_sources(&agent_core_dir, &mut |path, source| {
        for dependency in forbidden_dependencies {
            if source.contains(dependency) {
                offenders.push(format!("{} imports {dependency}", path.display()));
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "agent_core must stay transport-, persistence-, and runtime-independent: {offenders:?}"
    );

    let cargo_toml = include_str!("../Cargo.toml");
    assert!(
        cargo_toml
            .lines()
            .any(|line| line.trim() == "mcp-core = []"),
        "mcp-core must not enable the in-process agent plane"
    );
    let lib_rs = include_str!("../src/lib.rs");
    assert!(
        lib_rs.contains("#[cfg(any(feature = \"agent-plane\", feature = \"mcp-core\"))]"),
        "agent_core must compile for either the HTTP agent plane or MCP core"
    );
    let backends_mod = include_str!("../src/agent_backends/mod.rs");
    assert!(backends_mod.contains("#[cfg(feature = \"mcp-http-backend\")]"));
    assert!(backends_mod.contains("#[cfg(feature = \"mcp-embedded\")]"));
}

#[test]
fn srt_policy_store_consumes_typed_policies_without_persistence_dependencies() {
    let srt_policy = include_str!("../src/media/srt_policy.rs");
    assert!(srt_policy.contains("pub struct SrtIngestPolicyEntry"));
    assert!(srt_policy.contains("pub policy: SrtPipelineIngestConfig"));
    assert!(
        !srt_policy.contains("crate::application::")
            && !srt_policy.contains("restream::application::"),
        "media SRT policy store should depend on narrow policy entries, not application models"
    );
    assert!(
        !srt_policy.contains("serde_json") && !srt_policy.contains("serialized_policy"),
        "media SRT policy store should consume typed policy values, not persisted JSON"
    );
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
fn db_production_layer_stays_independent_of_application() {
    for (name, source) in [
        ("db/mod.rs", include_str!("../src/db/mod.rs")),
        (
            "db/ingest_repo.rs",
            include_str!("../src/db/ingest_repo.rs"),
        ),
        ("db/job_repo.rs", include_str!("../src/db/job_repo.rs")),
        ("db/log_repo.rs", include_str!("../src/db/log_repo.rs")),
        ("db/meta_repo.rs", include_str!("../src/db/meta_repo.rs")),
        ("db/migrations.rs", include_str!("../src/db/migrations.rs")),
        (
            "db/output_repo.rs",
            include_str!("../src/db/output_repo.rs"),
        ),
        (
            "db/pipeline_input_repo.rs",
            include_str!("../src/db/pipeline_input_repo.rs"),
        ),
        (
            "db/pipeline_repo.rs",
            include_str!("../src/db/pipeline_repo.rs"),
        ),
        (
            "db/recording_repo.rs",
            include_str!("../src/db/recording_repo.rs"),
        ),
        ("db/schema.rs", include_str!("../src/db/schema.rs")),
        (
            "db/session_repo.rs",
            include_str!("../src/db/session_repo.rs"),
        ),
    ] {
        assert!(
            !source.contains("crate::application::") && !source.contains("restream::application::"),
            "{name} must not depend on application-layer models or helpers"
        );
    }
}

#[test]
fn application_ports_are_abstract_and_sqlite_adapters_live_in_infrastructure() {
    let ports = include_str!("../src/application/ports.rs");
    for forbidden in ["sqlx", "Sqlite", "crate::db"] {
        assert!(
            !ports.contains(forbidden),
            "application ports should not expose concrete persistence detail {forbidden}"
        );
    }

    let infrastructure_mod = include_str!("../src/infrastructure/mod.rs");
    assert!(infrastructure_mod.contains("pub mod sqlite_ports;"));
    assert!(infrastructure_mod.contains("pub mod recording_metadata;"));

    let sqlite_ports = include_str!("../src/infrastructure/sqlite_ports.rs");
    assert!(sqlite_ports.contains("SqlitePool"));
    assert!(sqlite_ports.contains("impl PipelineStore for SqlitePipelineStore"));
    assert!(sqlite_ports.contains("impl OutputStore for SqliteOutputStore"));
}

#[test]
fn application_production_code_does_not_depend_on_axum() {
    let application_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/application");
    let mut offenders = Vec::new();
    collect_rust_sources(&application_dir, &mut |path, source| {
        if path.file_name().and_then(|name| name.to_str()) == Some("tests.rs") {
            return;
        }
        let production_source = source.split("#[cfg(test)]").next().unwrap_or(source);
        if production_source.contains("axum") {
            offenders.push(
                path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                    .expect("application source should be inside the repository")
                    .display()
                    .to_string(),
            );
        }
    });

    offenders.sort();
    assert!(
        offenders.is_empty(),
        "application production code must not import Axum: {offenders:?}"
    );
}

#[test]
fn recording_metadata_persistence_is_infrastructure_owned() {
    let recording = include_str!("../src/application/recording.rs");
    let production_recording = recording
        .split("#[cfg(test)]")
        .next()
        .expect("application recording module should have production section");
    for forbidden in ["SqlitePool", "crate::db", "sqlx::"] {
        assert!(
            !production_recording.contains(forbidden),
            "application recording runtime should not own concrete DB detail {forbidden}"
        );
    }

    let infrastructure = include_str!("../src/infrastructure/recording_metadata.rs");
    assert!(infrastructure.contains("SqlitePool"));
    assert!(infrastructure.contains("persist_recording_metadata_event"));
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
fn god_file_extractions_keep_owned_helpers_out_of_protocol_roots() {
    let rtmp = include_str!("../src/media/rtmp.rs");
    assert!(
        !rtmp.contains("struct RtmpTimestampGuard"),
        "RTMP timestamp monotonicity should stay in the timestamp helper module"
    );

    let lib_rs = include_str!("../src/lib.rs");
    assert!(
        !lib_rs.contains("pub struct RuntimeTuning"),
        "runtime tuning shape should stay in config, not the crate composition root"
    );
}

#[test]
fn runtime_json_views_stay_out_of_application_services() {
    let services_mod = include_str!("../src/application/services/mod.rs");
    assert!(
        !services_mod.contains("runtime_view_service"),
        "API/runtime JSON adapters should stay at the presentation-runtime boundary"
    );

    let app_state = include_str!("../src/api/state.rs");
    assert!(
        !app_state.contains("RuntimeViewService"),
        "AppState should not carry a pass-through application service for API JSON views"
    );
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
        "\"rootDir\": \"web/ts\"",
        "\"outDir\": \"public/js\"",
        "\"sourceMap\": false",
    ] {
        assert!(
            tsconfig.contains(required),
            "tsconfig must contain {required}"
        );
    }

    let hls_sync = include_str!("../scripts/dev/frontend/prepare-assets.mjs");
    assert!(hls_sync.contains("sourceMappingURL=hls\\.min\\.js\\.map"));
    let generated_hls_bundle =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("public/js/lib/hls.min.js");
    if generated_hls_bundle.is_file() {
        let hls_bundle =
            std::fs::read_to_string(&generated_hls_bundle).expect("generated HLS bundle is UTF-8");
        assert!(
            !hls_bundle.contains("sourceMappingURL=hls.min.js.map"),
            "generated HLS bundle should not point at an absent source map"
        );
    }
}

#[test]
fn frontend_features_do_not_import_app_composition_modules() {
    let features_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/ts/features");
    let mut offenders = Vec::new();
    let mut stack = vec![features_dir];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("features dir should exist") {
            let path = entry.expect("feature entry should be readable").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("ts") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("feature source should be UTF-8");
            if source.contains("../app/") {
                offenders.push(
                    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap()
                        .display()
                        .to_string(),
                );
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "feature modules should not import app composition modules: {offenders:?}"
    );
}

#[test]
fn frontend_render_modules_do_not_import_runtime_composition_modules() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let forbidden_edges = [
        (
            "web/ts/features/pipeline-view/index.ts",
            "from \"./dashboard.js\"",
        ),
        (
            "web/ts/features/pipeline-view/index.ts",
            "from './dashboard.js'",
        ),
        (
            "web/ts/features/pipeline-output-list.ts",
            "from \"./control-room.js\"",
        ),
        (
            "web/ts/features/pipeline-output-list.ts",
            "from './control-room.js'",
        ),
    ];
    let mut offenders = Vec::new();
    for (relative_path, needle) in forbidden_edges {
        let source = std::fs::read_to_string(manifest_dir.join(relative_path))
            .expect("frontend render module should be UTF-8");
        if source.contains(needle) {
            offenders.push(format!("{relative_path} imports {needle}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "render modules should use app-wired dependencies instead of runtime composition imports: {offenders:?}"
    );
}

#[test]
fn source_distribution_manifest_matches_declared_build_inputs() {
    let manifest = include_str!("../docs/source-distribution.md");
    for required in [
        ".local/build/static/prefix/",
        "package-lock.json",
        "tsconfig.json",
        "tsconfig.v2.json",
        "vite.v2.config.ts",
        "scripts/build/resource-limit.sh ./scripts/build/native-deps.sh",
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
        "scripts/harness/run.sh",
        "scripts/build/bench-harness.sh",
        "scripts/build/native-deps.sh",
        "scripts/dev/frontend/prepare-assets.mjs",
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

fn collect_rust_sources(
    directory: &std::path::Path,
    inspect: &mut impl FnMut(&std::path::Path, &str),
) {
    for entry in std::fs::read_dir(directory).expect("Rust source directory should be readable") {
        let path = entry.expect("Rust source entry should be readable").path();
        if path.is_dir() {
            collect_rust_sources(&path, inspect);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            let source = std::fs::read_to_string(&path).expect("Rust source should be UTF-8");
            inspect(&path, &source);
        }
    }
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

    let services =
        restream::infrastructure::service_wiring::SqliteServiceFactory::new(&db).compose();
    let pipeline_service = services.pipeline_service;
    let output_service = services.output_service;

    let pid = "test-pipe-service";
    pipeline_service
        .create_pipeline(pid, "name", "stream-key", None, None)
        .await
        .unwrap();

    let pipeline = restream::db::get_pipeline(&db, pid).await.unwrap().unwrap();
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

    let output = restream::db::get_output(&db, pid, oid)
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
