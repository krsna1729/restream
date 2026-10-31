//! Runtime dependency and build-version introspection exposed to status and
//! diagnostics surfaces.

use std::ffi::{CStr, c_char};
use std::path::Path;

use serde_json::{Value, json};

unsafe extern "C" {
    fn sqlite3_libversion() -> *const c_char;
    fn sqlite3_sourceid() -> *const c_char;
}

const RUST_DEPENDENCIES_JSON: &str =
    include_str!(concat!(env!("OUT_DIR"), "/rust-runtime-dependencies.json"));

fn c_string(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return "unknown".to_string();
    }
    // SAFETY: Caller guarantees pointer is either null or a valid
    // NUL-terminated C string obtained from a C library (SQLite or glibc). The null case is checked above.
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

fn license(expression: &str) -> Value {
    json!([{ "expression": expression }])
}

fn application_license() -> Value {
    if !env!("CARGO_PKG_LICENSE").is_empty() {
        license(env!("CARGO_PKG_LICENSE"))
    } else if !env!("CARGO_PKG_LICENSE_FILE").is_empty() {
        json!([{ "license": { "name": "LicenseRef-restream-internal" } }])
    } else {
        json!([{ "license": { "name": "NOASSERTION" } }])
    }
}

fn native_component(
    name: &str,
    version: String,
    license_expression: &str,
    version_source: &str,
    properties: Vec<Value>,
) -> Value {
    let linkage = "statically linked component";
    let bom_version = version
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || ".-_+".contains(character) {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let mut component = json!({
        "type": "library",
        "bom-ref": format!("native:{name}@{bom_version}"),
        "name": name,
        "version": version,
        "licenses": license(license_expression),
        "properties": [
            { "name": "restream:ecosystem", "value": "native" },
            { "name": "restream:versionSource", "value": version_source },
            { "name": "restream:linkage", "value": linkage }
        ]
    });
    if let Some(list) = component["properties"].as_array_mut() {
        list.extend(properties);
    }
    component
}

fn sbom_dependencies(
    application_ref: &str,
    components: &[Value],
    rust_dependencies: &[Value],
) -> Value {
    let component_refs: Vec<String> = components
        .iter()
        .filter_map(|component| component["bom-ref"].as_str().map(str::to_owned))
        .collect();
    let mut dependencies = vec![json!({
        "ref": application_ref,
        "dependsOn": component_refs
    })];

    for dependency in rust_dependencies {
        let Some(reference) = dependency["bomRef"].as_str() else {
            continue;
        };
        let depends_on: Vec<String> = dependency["dependencyRefs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .filter(|reference| component_refs.contains(reference))
            .collect();
        dependencies.push(json!({
            "ref": reference,
            "dependsOn": depends_on
        }));
    }

    json!(dependencies)
}

fn rust_dependency_inventory() -> Vec<Value> {
    serde_json::from_str(RUST_DEPENDENCIES_JSON).expect("embedded dependency inventory")
}

fn rust_components(dependencies: &[Value]) -> Vec<Value> {
    dependencies
        .iter()
        .map(|dependency| {
            let name = dependency["name"].as_str().unwrap_or("unknown");
            let version = dependency["version"].as_str().unwrap_or("unknown");
            let purl = dependency["bomRef"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("pkg:cargo/{name}@{version}"));
            let licenses = dependency["license"]
                .as_str()
                .map(license)
                .unwrap_or_else(|| json!([{ "license": { "name": "NOASSERTION" } }]));
            let enabled_features = dependency["features"]
                .as_array()
                .map(|features| {
                    features
                        .iter()
                        .filter_map(|feature| feature.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            json!({
                "type": "library",
                "bom-ref": purl,
                "name": name,
                "version": version,
                "purl": purl,
                "licenses": licenses,
                "hashes": dependency["checksum"].as_str().map(|checksum| {
                    json!([{ "alg": "SHA-256", "content": checksum }])
                }).unwrap_or_else(|| json!([])),
                "properties": [
                    { "name": "restream:ecosystem", "value": "cargo" },
                    { "name": "restream:versionSource", "value": "Cargo.lock" },
                    { "name": "restream:enabledFeatures", "value": enabled_features },
                    {
                        "name": "restream:source",
                        "value": dependency["source"].as_str().unwrap_or("path")
                    }
                ]
            })
        })
        .collect()
}

fn sqlite_runtime_info() -> (String, String) {
    (
        // SAFETY: sqlite3_libversion returns a valid static string owned by the
        // linked SQLite library for the process lifetime.
        c_string(unsafe { sqlite3_libversion() }),
        // SAFETY: sqlite3_sourceid returns a valid static string owned by the
        // linked SQLite library for the process lifetime.
        c_string(unsafe { sqlite3_sourceid() }),
    )
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn libc_component() -> Option<Value> {
    // SAFETY: gnu_get_libc_version is a glibc extension returning a
    // NUL-terminated static string. The returned pointer is valid for
    // the process lifetime. Only compiled on linux+gnu targets.
    unsafe extern "C" {
        fn gnu_get_libc_version() -> *const c_char;
    }
    Some(native_component(
        "glibc",
        // SAFETY: gnu_get_libc_version returns a valid static string pointer.
        c_string(unsafe { gnu_get_libc_version() }),
        "LGPL-2.1-or-later",
        "runtime API",
        Vec::new(),
    ))
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn libc_component() -> Option<Value> {
    None
}

pub fn status_and_sbom(bonding_available: bool) -> (Value, Value) {
    let (sqlite_version, sqlite_source_id) = sqlite_runtime_info();
    let srt_version = restream_engine::srt::linked_srt_version();
    let mut native_components = vec![
        native_component(
            "srt-rs",
            srt_version.clone(),
            "Apache-2.0",
            "runtime API",
            vec![
                json!({
                    "name": "restream:bondingAvailable",
                    "value": bonding_available.to_string()
                }),
                json!({
                    "name": "restream:buildResolvedVersion",
                    "value": env!("RESTREAM_BUILD_SRT_VERSION")
                }),
            ],
        ),
        native_component(
            "SQLite",
            sqlite_version.clone(),
            "blessing",
            "runtime SQL function",
            vec![json!({
                "name": "restream:sqliteSourceId",
                "value": sqlite_source_id
            })],
        ),
        native_component(
            "libgcc",
            env!("RESTREAM_GCC_RUNTIME_VERSION").to_string(),
            "GPL-3.0-or-later WITH GCC-exception-3.1",
            "C compiler used for linking",
            Vec::new(),
        ),
        native_component(
            "Rust standard library",
            env!("RESTREAM_RUSTC_VERSION").to_string(),
            "MIT OR Apache-2.0",
            "rustc toolchain used for linking",
            Vec::new(),
        ),
    ];
    if let Some(component) = libc_component() {
        native_components.push(component);
    }
    let native_component_names: Vec<String> = native_components
        .iter()
        .filter_map(|component| component["name"].as_str().map(str::to_string))
        .collect();

    let rust_dependencies = rust_dependency_inventory();
    let mut components = rust_components(&rust_dependencies);
    let rust_component_count = components.len();
    let native_component_count = native_components.len();
    components.extend(native_components);
    components.sort_by(|left, right| {
        left["name"]
            .as_str()
            .cmp(&right["name"].as_str())
            .then_with(|| left["version"].as_str().cmp(&right["version"].as_str()))
    });

    let application = json!({
        "type": "application",
        "bom-ref": format!("pkg:cargo/restream@{}", env!("CARGO_PKG_VERSION")),
        "name": "restream",
        "version": env!("CARGO_PKG_VERSION"),
        "purl": format!("pkg:cargo/restream@{}", env!("CARGO_PKG_VERSION")),
        "licenses": application_license(),
        "properties": [
            { "name": "restream:gitCommit", "value": env!("GIT_COMMIT_HASH") },
            {
                "name": "restream:licenseFile",
                "value": env!("CARGO_PKG_LICENSE_FILE")
            }
        ]
    });
    let application_ref = application["bom-ref"]
        .as_str()
        .expect("application component has bom-ref");
    let dependencies = sbom_dependencies(application_ref, &components, &rust_dependencies);

    let sbom = json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "timestamp": env!("RESTREAM_BUILD_TIMESTAMP"),
            "component": application,
            "tools": {
                "components": [{
                    "type": "application",
                    "name": "restream-runtime-sbom",
                    "version": env!("CARGO_PKG_VERSION")
                }]
            },
            "properties": [
                { "name": "restream:generatedBy", "value": "running process" },
                { "name": "restream:rustDependencySource", "value": "resolved normal Cargo dependency closure" },
                { "name": "restream:gitCommit", "value": env!("GIT_COMMIT_HASH") }
            ]
        },
        "components": components,
        "dependencies": dependencies
    });

    let status = json!({
        "restream": {
            "version": env!("CARGO_PKG_VERSION"),
            "commit": env!("GIT_COMMIT_HASH"),
            "buildTimestamp": env!("RESTREAM_BUILD_TIMESTAMP"),
        },
        "toolchain": {
            "rustc": env!("RESTREAM_RUSTC_VERSION"),
            "target": env!("RESTREAM_RUSTC_HOST"),
            "llvm": env!("RESTREAM_LLVM_VERSION"),
            "gccRuntime": env!("RESTREAM_GCC_RUNTIME_VERSION"),
        },
        "nativeLibraries": {
            "srt": {
                "version": srt_version,
                "buildVersion": env!("RESTREAM_BUILD_SRT_VERSION"),
                "license": "Apache-2.0",
                "bondingAvailable": bonding_available,
            },
            "sqlite": {
                "version": sqlite_version,
                "sourceId": sqlite_source_id,
                "license": "blessing",
            }
        },
        // The FFmpeg executables and what each FFmpeg-backed feature can use;
        // `null` until the startup check finishes.
        "externalTools": restream_engine::ffmpeg_capabilities::finished(),
        "sbom": {
            "format": "CycloneDX",
            "specVersion": "1.5",
            "endpoint": "/api/v1/engine/sbom",
            "componentCount": rust_component_count + native_component_count,
            "rustComponentCount": rust_component_count,
            "nativeComponentCount": native_component_count,
            "nativeComponents": native_component_names,
            "licensesIncluded": true,
        }
    });

    (status, sbom)
}

pub fn emit_sbom(path: &Path) -> Result<(), String> {
    let (_, sbom) = status_and_sbom(false);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create SBOM directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let bytes = serde_json::to_vec_pretty(&sbom)
        .map_err(|error| format!("failed to serialize SBOM JSON: {error}"))?;
    std::fs::write(
        path,
        format!("{}\n", String::from_utf8_lossy(&bytes)).as_bytes(),
    )
    .map_err(|error| format!("failed to write SBOM file {}: {error}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_sbom_keeps_full_provenance() {
        let (_, sbom) = status_and_sbom(false);

        assert!(
            sbom.pointer("/metadata/timestamp").is_some(),
            "runtime SBOM should expose build timestamp provenance"
        );
        assert!(
            sbom.pointer("/metadata/component/properties")
                .and_then(|value| value.as_array())
                .is_some_and(|properties| properties.iter().any(|property| property
                    .get("name")
                    .and_then(|name| name.as_str())
                    == Some("restream:gitCommit"))),
            "runtime SBOM should expose git commit provenance"
        );
        assert!(
            sbom.pointer("/components")
                .and_then(|value| value.as_array())
                .is_some_and(|components| components.iter().any(|component| component
                    .get("properties")
                    .and_then(|value| value.as_array())
                    .is_some_and(|properties| properties.iter().any(|property| property
                        .get("name")
                        .and_then(|name| name.as_str())
                        == Some("restream:sqliteSourceId"))))),
            "runtime SBOM should expose the linked SQLite source id"
        );
    }
}
