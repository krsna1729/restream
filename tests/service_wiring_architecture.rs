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
        "OutputService",
        "IngestService",
        "AuthService",
        "SettingsService",
        "HealthService",
        "FileIngestService",
        "MediaLibraryService",
        "LogService",
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

    let services_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/application/services");
    let mut storage_coupled_services = Vec::new();
    collect_rust_sources(&services_dir, &mut |path, source| {
        if path.file_name().and_then(|name| name.to_str()) == Some("tests.rs") {
            return;
        }
        let production_source = source.split("#[cfg(test)]").next().unwrap_or(source);
        if production_source.contains("use sqlx::")
            || production_source.contains("sqlx::SqlitePool")
        {
            storage_coupled_services.push(path.display().to_string());
        }
    });
    assert!(
        storage_coupled_services.is_empty(),
        "application services must accept storage-neutral ports: {storage_coupled_services:?}"
    );
}

#[test]
fn api_state_and_auth_do_not_depend_on_sqlite_infrastructure() {
    let api_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
    let mut offenders = Vec::new();
    collect_rust_sources(&api_dir, &mut |path, source| {
        let production_source = source.split("#[cfg(test)]").next().unwrap_or(source);
        for forbidden in ["sqlx::", "crate::infrastructure::", "SqliteServiceFactory"] {
            if production_source.contains(forbidden) {
                offenders.push(format!("{} depends on {forbidden}", path.display()));
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "API modules must receive storage-neutral services: {offenders:?}"
    );
}
