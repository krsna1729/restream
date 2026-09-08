use std::path::Path;

fn collect_rust_sources(directory: &Path, inspect: &mut impl FnMut(&Path, &str)) {
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

#[test]
fn infrastructure_owns_sqlite_service_composition() {
    let source = include_str!("../src/infrastructure/service_wiring.rs");

    for application_service in [
        "PipelineService",
        "PipelineInputService",
        "AuthService",
        "SettingsService",
        "FileIngestService",
        "MediaLibraryService",
        "AgentService",
    ] {
        assert!(
            !source.contains(&format!("impl {application_service}")),
            "infrastructure must not add inherent methods to application-owned {application_service}"
        );
    }
    assert!(source.contains("pub struct SqliteServiceFactory"));
    assert!(source.contains("use crate::api::AppServices;"));
    assert!(!source.contains("pub struct AppServices"));
    assert!(
        !source.contains("LogService"),
        "LogService was collapsed to application::logs + AppState.db (Wave 1 A-lite)"
    );
    assert!(
        !source.contains("OutputService"),
        "OutputService was collapsed to application::outputs + AppState.db (Wave 1 B)"
    );
    assert!(
        !source.contains("IngestService"),
        "IngestService was collapsed to application::ingests + AppState.db (Wave 1 C)"
    );

    let services_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/application/services");
    let mut storage_coupled_services = Vec::new();
    collect_rust_sources(&services_dir, &mut |path, source| {
        if path.file_name().and_then(|name| name.to_str()) == Some("tests.rs") {
            return;
        }
        let production_source = source.split("#[cfg(test)]").next().unwrap_or(source);
        // Collapsed #149 slices may hold SqlitePool; still forbid infrastructure adapters.
        for forbidden in [
            "crate::infrastructure::sqlite_ports",
            "SqlitePipelineStore",
            "SqliteOutputStore",
            "SqliteIngestLookup",
            "SqliteMetaStore",
            "SqliteSessionStore",
            "SqliteJobStore",
            "SqliteRecordingStore",
        ] {
            if production_source.contains(forbidden) {
                storage_coupled_services.push(format!("{} uses {forbidden}", path.display()));
            }
        }
    });
    assert!(
        storage_coupled_services.is_empty(),
        "application services must not import SQLite adapters: {storage_coupled_services:?}"
    );
}

#[test]
fn api_modules_keep_sqlite_at_the_app_state_root() {
    let api_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
    let mut offenders = Vec::new();
    collect_rust_sources(&api_dir, &mut |path, source| {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let production_source = source.split("#[cfg(test)]").next().unwrap_or(source);
        // Concrete App root may hold SqlitePool; handlers must not reach into
        // infrastructure adapters or the service factory.
        for forbidden in ["crate::infrastructure::", "SqliteServiceFactory"] {
            if production_source.contains(forbidden) {
                offenders.push(format!("{} depends on {forbidden}", path.display()));
            }
        }
        if file_name != "state.rs"
            && (production_source.contains("sqlx::") || production_source.contains("SqlitePool"))
        {
            offenders.push(format!(
                "{} must not import sqlx; use AppState.db / application helpers",
                path.display()
            ));
        }
    });
    assert!(
        offenders.is_empty(),
        "API modules must keep SQLite at the AppState root: {offenders:?}"
    );
}
