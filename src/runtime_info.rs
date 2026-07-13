//! Runtime dependency and build-version introspection exposed to status and
//! diagnostics surfaces.

use std::ffi::{CStr, c_char};
use std::path::Path;

use serde_json::{Value, json};

// SAFETY: mbedtls_version_get_string_full writes a NUL-terminated version
// string (e.g. "Mbed TLS 3.6.6") into the caller-provided buffer. The Mbed TLS
// docs guarantee the output never exceeds 18 bytes including the NUL, so the
// 32-byte buffer at the call site is always large enough.
unsafe extern "C" {
    fn mbedtls_version_get_string_full(string: *mut c_char);
    fn sqlite3_libversion() -> *const c_char;
    fn sqlite3_sourceid() -> *const c_char;
}

const RUST_DEPENDENCIES_JSON: &str =
    include_str!(concat!(env!("OUT_DIR"), "/rust-runtime-dependencies.json"));
const NATIVE_BUILD_INPUTS_JSON: &str =
    include_str!(concat!(env!("OUT_DIR"), "/native-build-inputs.json"));

fn c_string(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return "unknown".to_string();
    }
    // SAFETY: Caller guarantees pointer is either null or a valid
    // NUL-terminated C string obtained from a C library (Mbed TLS, FFmpeg,
    // or glibc). The null case is checked above.
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

fn av_version(version: u32) -> String {
    format!(
        "{}.{}.{}",
        (version >> 16) & 0xff,
        (version >> 8) & 0xff,
        version & 0xff
    )
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

fn native_component_with_inputs(
    name: &str,
    version: String,
    license_expression: &str,
    version_source: &str,
    properties: Vec<Value>,
    input_paths: &[&str],
    native_inputs: &[Value],
) -> Value {
    let mut component = native_component(
        name,
        version,
        license_expression,
        version_source,
        properties,
    );
    let entries = native_input_entries(input_paths, native_inputs);

    let archive_hashes: Vec<_> = entries
        .iter()
        .filter(|entry| entry["kind"] == "static-archive")
        .filter_map(|entry| {
            entry["sha256"]
                .as_str()
                .map(|hash| json!({ "alg": "SHA-256", "content": hash }))
        })
        .collect();
    if !archive_hashes.is_empty() {
        component["hashes"] = json!(archive_hashes);
    }

    if let Some(list) = component["properties"].as_array_mut() {
        for entry in entries {
            if let (Some(path), Some(hash)) = (entry["path"].as_str(), entry["sha256"].as_str()) {
                list.push(json!({ "name": "restream:nativeInput", "value": path }));
                list.push(json!({
                    "name": "restream:nativeInputSha256",
                    "value": format!("{path}={hash}")
                }));
            }
        }
    }
    component
}

fn native_input_entries<'a>(input_paths: &[&str], native_inputs: &'a [Value]) -> Vec<&'a Value> {
    input_paths
        .iter()
        .filter_map(|path| {
            native_inputs
                .iter()
                .find(|entry| entry["path"].as_str() == Some(*path))
        })
        .collect()
}

fn component_ref(components: &[Value], name: &str) -> Option<String> {
    components
        .iter()
        .find(|component| component["name"] == name)
        .and_then(|component| component["bom-ref"].as_str())
        .map(str::to_owned)
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

    for (name, depends_on) in [
        (
            "libsrt",
            &["libmbedtls", "libmbedx509", "libmbedcrypto"][..],
        ),
        ("libmbedtls", &["libmbedx509", "libmbedcrypto"][..]),
        ("libmbedx509", &["libmbedcrypto"][..]),
        ("libavformat", &["libavcodec", "libavutil"][..]),
        (
            "libavfilter",
            &["libavcodec", "libavformat", "libavutil"][..],
        ),
        ("libavcodec", &["libavutil", "x264", "x265"][..]),
        ("libswscale", &["libavutil"][..]),
        ("libswresample", &["libavutil"][..]),
    ] {
        if let Some(reference) = component_ref(components, name) {
            let refs: Vec<String> = depends_on
                .iter()
                .filter_map(|dependency| component_ref(components, dependency))
                .collect();
            dependencies.push(json!({
                "ref": reference,
                "dependsOn": refs
            }));
        }
    }

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

fn native_build_inputs() -> Vec<Value> {
    serde_json::from_str(NATIVE_BUILD_INPUTS_JSON).expect("embedded native input inventory")
}

fn ffmpeg_components(native_inputs: &[Value]) -> (Vec<Value>, String, String) {
    // SAFETY: avcodec_configuration and avcodec_license are FFmpeg C API
    // functions that return NUL-terminated static strings valid for the
    // process lifetime. No ownership transfer; caller must not free.
    let configuration = c_string(unsafe { ffmpeg_next::ffi::avcodec_configuration() });
    let license_text = c_string(unsafe { ffmpeg_next::ffi::avcodec_license() });
    let license_expression = if license_text.to_ascii_lowercase().contains("gpl") {
        "GPL-2.0-or-later"
    } else {
        "LGPL-2.1-or-later"
    };
    let common_properties = || {
        vec![
            json!({ "name": "restream:ffmpegConfiguration", "value": configuration }),
            json!({ "name": "restream:runtimeLicenseText", "value": license_text }),
        ]
    };

    // SAFETY: All ffmpeg_next::ffi::*_version() functions are FFmpeg C API
    // calls that return a u32 version integer. No pointer arguments, no
    // memory allocation; they are pure functions callable from any context.
    let components = unsafe {
        vec![
            native_component_with_inputs(
                "libavcodec",
                av_version(ffmpeg_next::ffi::avcodec_version()),
                license_expression,
                "runtime API",
                common_properties(),
                &["lib/libavcodec.a", "lib/pkgconfig/libavcodec.pc"],
                native_inputs,
            ),
            native_component_with_inputs(
                "libavformat",
                av_version(ffmpeg_next::ffi::avformat_version()),
                license_expression,
                "runtime API",
                common_properties(),
                &["lib/libavformat.a", "lib/pkgconfig/libavformat.pc"],
                native_inputs,
            ),
            native_component_with_inputs(
                "libavfilter",
                av_version(ffmpeg_next::ffi::avfilter_version()),
                license_expression,
                "runtime API",
                common_properties(),
                &["lib/libavfilter.a", "lib/pkgconfig/libavfilter.pc"],
                native_inputs,
            ),
            native_component_with_inputs(
                "libswscale",
                av_version(ffmpeg_next::ffi::swscale_version()),
                license_expression,
                "runtime API",
                common_properties(),
                &["lib/libswscale.a", "lib/pkgconfig/libswscale.pc"],
                native_inputs,
            ),
            native_component_with_inputs(
                "libswresample",
                av_version(ffmpeg_next::ffi::swresample_version()),
                license_expression,
                "runtime API",
                common_properties(),
                &["lib/libswresample.a", "lib/pkgconfig/libswresample.pc"],
                native_inputs,
            ),
            native_component_with_inputs(
                "libavutil",
                av_version(ffmpeg_next::ffi::avutil_version()),
                license_expression,
                "runtime API",
                common_properties(),
                &["lib/libavutil.a", "lib/pkgconfig/libavutil.pc"],
                native_inputs,
            ),
        ]
    };

    (components, configuration, license_text)
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
    let native_inputs = native_build_inputs();
    let (sqlite_version, sqlite_source_id) = sqlite_runtime_info();
    // SAFETY: mbedtls_version_get_string_full writes at most 18 bytes
    // (including the NUL) into the 32-byte buffer, then c_string reads it back
    // as a NUL-terminated C string. The buffer outlives the read.
    let mbedtls_version = {
        let mut buffer = [0 as c_char; 32];
        unsafe { mbedtls_version_get_string_full(buffer.as_mut_ptr()) };
        c_string(buffer.as_ptr())
    };
    let srt_version = crate::media::srt::linked_srt_version();
    let x264_version = env!("RESTREAM_BUILD_X264_VERSION").to_string();
    let x265_version = env!("RESTREAM_BUILD_X265_VERSION").to_string();
    let (mut native_components, ffmpeg_configuration, ffmpeg_license) =
        ffmpeg_components(&native_inputs);

    native_components.extend([
        native_component_with_inputs(
            "libsrt",
            srt_version.clone(),
            "MPL-2.0",
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
            &["lib/libsrt.a", "lib/pkgconfig/srt.pc"],
            &native_inputs,
        ),
        native_component_with_inputs(
            "libmbedtls",
            mbedtls_version.clone(),
            "Apache-2.0",
            "runtime API",
            vec![json!({
                "name": "restream:buildResolvedVersion",
                "value": env!("RESTREAM_BUILD_MBEDTLS_VERSION")
            })],
            &["lib/libmbedtls.a", "lib/pkgconfig/mbedtls.pc"],
            &native_inputs,
        ),
        native_component_with_inputs(
            "libmbedx509",
            mbedtls_version.clone(),
            "Apache-2.0",
            "runtime API",
            vec![json!({
                "name": "restream:buildResolvedVersion",
                "value": env!("RESTREAM_BUILD_MBEDTLS_VERSION")
            })],
            &["lib/libmbedx509.a", "lib/pkgconfig/mbedx509.pc"],
            &native_inputs,
        ),
        native_component_with_inputs(
            "libmbedcrypto",
            mbedtls_version.clone(),
            "Apache-2.0",
            "runtime API",
            vec![json!({
                "name": "restream:buildResolvedVersion",
                "value": env!("RESTREAM_BUILD_MBEDTLS_VERSION")
            })],
            &["lib/libmbedcrypto.a", "lib/pkgconfig/mbedcrypto.pc"],
            &native_inputs,
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
        native_component_with_inputs(
            "x264",
            x264_version.clone(),
            "GPL-2.0-or-later",
            "linked pkg-config metadata at build time",
            vec![json!({
                "name": "restream:runtimeDispatch",
                "value": "x86 assembly enabled"
            })],
            &["lib/libx264.a", "lib/pkgconfig/x264.pc"],
            &native_inputs,
        ),
        native_component_with_inputs(
            "x265",
            x265_version.clone(),
            "GPL-2.0-or-later",
            "linked pkg-config metadata at build time",
            vec![json!({
                "name": "restream:runtimeDispatch",
                "value": "x86 assembly enabled"
            })],
            &["lib/libx265.a", "lib/pkgconfig/x265.pc"],
            &native_inputs,
        ),
        native_component(
            "libstdc++",
            env!("RESTREAM_GCC_RUNTIME_VERSION").to_string(),
            "GPL-3.0-or-later WITH GCC-exception-3.1",
            "C++ compiler used for linking",
            Vec::new(),
        ),
        native_component(
            "libgcc",
            env!("RESTREAM_GCC_RUNTIME_VERSION").to_string(),
            "GPL-3.0-or-later WITH GCC-exception-3.1",
            "C/C++ compiler used for linking",
            Vec::new(),
        ),
        native_component(
            "Rust standard library",
            env!("RESTREAM_RUSTC_VERSION").to_string(),
            "MIT OR Apache-2.0",
            "rustc toolchain used for linking",
            Vec::new(),
        ),
    ]);
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
                "name": "restream:nativeBuildId",
                "value": env!("RESTREAM_NATIVE_BUILD_ID")
            },
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
                { "name": "restream:gitCommit", "value": env!("GIT_COMMIT_HASH") },
                { "name": "restream:nativeBuildId", "value": env!("RESTREAM_NATIVE_BUILD_ID") }
            ]
        },
        "components": components,
        "dependencies": dependencies
    });

    // SAFETY: av_version_info returns a NUL-terminated static string
    // owned by FFmpeg, valid for the process lifetime.
    let ffmpeg_version = c_string(unsafe { ffmpeg_next::ffi::av_version_info() });
    let status = json!({
        "restream": {
            "version": env!("CARGO_PKG_VERSION"),
            "commit": env!("GIT_COMMIT_HASH"),
            "nativeBuildId": env!("RESTREAM_NATIVE_BUILD_ID"),
            "buildTimestamp": env!("RESTREAM_BUILD_TIMESTAMP"),
        },
        "toolchain": {
            "rustc": env!("RESTREAM_RUSTC_VERSION"),
            "target": env!("RESTREAM_RUSTC_HOST"),
            "llvm": env!("RESTREAM_LLVM_VERSION"),
            "gccRuntime": env!("RESTREAM_GCC_RUNTIME_VERSION"),
        },
        "nativeLibraries": {
            "ffmpeg": {
                "version": ffmpeg_version,
                "configuration": ffmpeg_configuration,
                "license": ffmpeg_license,
                "x86Assembly": ffmpeg_configuration.contains("--enable-x86asm"),
            },
            "srt": {
                "version": srt_version,
                "buildVersion": env!("RESTREAM_BUILD_SRT_VERSION"),
                "license": "MPL-2.0",
                "bondingAvailable": bonding_available,
            },
            "mbedtls": {
                "version": mbedtls_version,
                "buildVersion": env!("RESTREAM_BUILD_MBEDTLS_VERSION"),
                "license": "Apache-2.0",
            },
            "sqlite": {
                "version": sqlite_version,
                "sourceId": sqlite_source_id,
                "license": "blessing",
            },
            "x264": {
                "version": x264_version,
                "license": "GPL-2.0-or-later",
                "versionSource": "linked pkg-config metadata at build time",
            },
            "x265": {
                "version": x265_version,
                "license": "GPL-2.0-or-later",
                "versionSource": "linked pkg-config metadata at build time",
            }
        },
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

fn normalize_sbom_for_repo_compare(sbom: &mut serde_json::Value) {
    if let Some(metadata) = sbom
        .get_mut("metadata")
        .and_then(|value| value.as_object_mut())
    {
        metadata.remove("timestamp");
        remove_sbom_properties_named(metadata.get_mut("component"), SBOM_REPO_VOLATILE_PROPERTIES);
        remove_sbom_properties_named(
            metadata.get_mut("properties"),
            SBOM_REPO_VOLATILE_PROPERTIES,
        );
    }
}

const SBOM_REPO_VOLATILE_PROPERTIES: &[&str] = &["restream:gitCommit", "restream:nativeBuildId"];

fn remove_sbom_properties_named(value: Option<&mut serde_json::Value>, names: &[&str]) {
    let Some(value) = value else {
        return;
    };
    let properties = if let Some(object) = value.as_object_mut() {
        object.get_mut("properties")
    } else {
        Some(value)
    };
    let Some(properties) = properties.and_then(|value| value.as_array_mut()) else {
        return;
    };
    properties.retain(|property| {
        property
            .get("name")
            .and_then(|name| name.as_str())
            .is_none_or(|name| !names.contains(&name))
    });
}

pub fn emit_sbom(path: &Path, deterministic: bool) -> Result<(), String> {
    let (_, sbom) = status_and_sbom(false);

    let mut output_sbom = sbom.clone();
    if deterministic {
        normalize_sbom_for_repo_compare(&mut output_sbom);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create SBOM directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let bytes = serde_json::to_vec_pretty(&output_sbom)
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
    fn repo_sbom_compare_ignores_volatile_build_provenance() {
        let mut left = serde_json::json!({
            "metadata": {
                "timestamp": "2026-06-28T01:00:00Z",
                "component": {
                    "properties": [
                        { "name": "restream:gitCommit", "value": "old" },
                        { "name": "restream:nativeBuildId", "value": "old-native" },
                        { "name": "restream:licenseFile", "value": "LICENSE" }
                    ]
                },
                "properties": [
                    { "name": "restream:gitCommit", "value": "old" },
                    { "name": "restream:nativeBuildId", "value": "old-native" },
                    { "name": "restream:generatedBy", "value": "running process" }
                ]
            },
            "components": [{ "name": "restream" }]
        });
        let mut right = serde_json::json!({
            "metadata": {
                "timestamp": "2026-06-29T01:00:00Z",
                "component": {
                    "properties": [
                        { "name": "restream:gitCommit", "value": "new" },
                        { "name": "restream:nativeBuildId", "value": "new-native" },
                        { "name": "restream:licenseFile", "value": "LICENSE" }
                    ]
                },
                "properties": [
                    { "name": "restream:gitCommit", "value": "new" },
                    { "name": "restream:nativeBuildId", "value": "new-native" },
                    { "name": "restream:generatedBy", "value": "running process" }
                ]
            },
            "components": [{ "name": "restream" }]
        });

        normalize_sbom_for_repo_compare(&mut left);
        normalize_sbom_for_repo_compare(&mut right);

        assert_eq!(left, right);
    }

    #[test]
    fn runtime_sbom_keeps_full_provenance_before_cli_normalization() {
        let (_, mut sbom) = status_and_sbom(false);

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

        normalize_sbom_for_repo_compare(&mut sbom);
        assert!(
            sbom.pointer("/metadata/timestamp").is_none(),
            "CLI/repo SBOM normalization should remove volatile timestamp"
        );
    }
}
