use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[test]
fn production_domain_dependencies_are_contract_only() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let domain_dir = manifest_dir.join("src/domain");
    let dependencies = declared_dependency_roots(include_str!("../Cargo.toml"));
    let mut sources = Vec::new();
    collect_rust_sources(&domain_dir, &mut sources);
    let mut offenders = Vec::new();

    for path in sources {
        if is_test_only_source(&path) {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("domain source should be UTF-8");
        let production = source.split("#[cfg(test)]").next().unwrap_or(&source);
        let relative = path
            .strip_prefix(manifest_dir)
            .expect("domain source should be inside the crate");

        for namespace in ["crate::", "restream::"] {
            for (offset, _) in production.match_indices(namespace) {
                let reference = &production[offset..];
                let allowed = reference.starts_with("crate::domain::")
                    || reference.starts_with("restream::domain::");
                if !allowed {
                    offenders.push(format!(
                        "{} uses an outward internal dependency near `{}`",
                        relative.display(),
                        reference.lines().next().unwrap_or(reference)
                    ));
                }
            }
        }

        for dependency in &dependencies {
            if matches!(dependency.as_str(), "serde" | "url") {
                continue;
            }
            if uses_dependency(production, dependency) {
                offenders.push(format!(
                    "{} uses non-contract dependency {dependency}",
                    relative.display()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "production domain code may depend only on domain-owned contracts, std, serde, and URL parsing: {offenders:#?}"
    );
}

fn collect_rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("source directory should be readable") {
        let path = entry.expect("source entry should be readable").path();
        if path.is_dir() {
            collect_rust_sources(&path, sources);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            sources.push(path);
        }
    }
}

fn is_test_only_source(path: &Path) -> bool {
    if path
        .components()
        .any(|component| component.as_os_str() == "tests")
    {
        return true;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(name, "test.rs" | "tests.rs")
        || name.ends_with("_test.rs")
        || name.ends_with("_tests.rs")
}

fn declared_dependency_roots(cargo_toml: &str) -> BTreeSet<String> {
    let mut dependencies = BTreeSet::new();
    let mut in_dependencies = false;
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed == "[dependencies]" {
            in_dependencies = true;
            continue;
        }
        if in_dependencies && trimmed.starts_with('[') {
            break;
        }
        if in_dependencies && let Some((name, _)) = trimmed.split_once('=') {
            dependencies.insert(name.trim().replace('-', "_"));
        }
    }
    dependencies
}

fn uses_dependency(source: &str, dependency: &str) -> bool {
    if source.contains(&format!("{dependency}::")) {
        return true;
    }
    source.lines().any(|line| {
        let trimmed = line.trim_start();
        let import = trimmed
            .strip_prefix("use ")
            .or_else(|| trimmed.strip_prefix("pub use "))
            .or_else(|| trimmed.strip_prefix("extern crate "));
        import.is_some_and(|import| {
            import
                .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .next()
                == Some(dependency)
        })
    })
}
