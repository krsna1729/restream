use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[test]
fn production_domain_dependencies_are_contract_only() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let domain_dir = manifest_dir.join("crates/restream-domain/src/domain");
    let dependencies =
        declared_dependency_roots(include_str!("../../crates/restream-domain/Cargo.toml"));
    let mut sources = Vec::new();
    collect_rust_sources(&domain_dir, &mut sources);
    let mut offenders = Vec::new();

    for path in sources {
        if is_test_only_source(&path) {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("domain source should be UTF-8");
        let production = source.split("#[cfg(test)]").next().unwrap_or(&source);
        // This neutral value descriptor is shared by domain output selection
        // and the wire codecs. Only its re-export is a contract dependency;
        // other core parser/runtime imports remain forbidden below.
        let dependencies_source = production.replace("pub use restream_core::VideoCodecKind;", "");
        let relative = path
            .strip_prefix(manifest_dir)
            .expect("domain source should be inside the crate");

        for namespace in ["crate::", "restream::", "restream_domain::"] {
            for (offset, _) in production.match_indices(namespace) {
                let reference = &production[offset..];
                let allowed = reference.starts_with("crate::domain::")
                    || reference.starts_with("restream_domain::domain::");
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
            // `ts_rs` only backs the `ts`-feature TypeScript export derives.
            if matches!(dependency.as_str(), "serde" | "url" | "ts_rs") {
                continue;
            }
            if uses_dependency(&dependencies_source, dependency) {
                offenders.push(format!(
                    "{} uses non-contract dependency {dependency}",
                    relative.display()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "production domain code may depend only on domain-owned contracts, the shared VideoCodecKind descriptor, std, serde, and URL parsing: {offenders:#?}"
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
    name == "tests.rs"
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
    // A path use starts at a word boundary: `String::` is not `ring::`.
    let path = format!("{dependency}::");
    if source.match_indices(&path).any(|(offset, _)| {
        source[..offset]
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_ascii_alphanumeric() && before != '_')
    }) {
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

#[test]
fn a_dependency_use_starts_at_a_word_boundary() {
    assert!(uses_dependency("let x = ring::digest();", "ring"));
    assert!(uses_dependency("use ring::hmac;", "ring"));
    assert!(!uses_dependency("let s = String::new();", "ring"));
}
