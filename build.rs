use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=RESTREAM_BUILD_GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=RESTREAM_BUILD_TIMESTAMP");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rustc-check-cfg=cfg(restream_ffmpeg_needs_avcodec_close_shim)");

    let build_identity = build_identity();
    println!("cargo:rustc-env=GIT_COMMIT_HASH={}", build_identity.commit);
    println!(
        "cargo:rustc-env=RESTREAM_BUILD_TIMESTAMP={}",
        build_identity.timestamp
    );
    embed_toolchain_versions();
    embed_rust_dependency_inventory();

    // SRT, FFmpeg, Mbed TLS, x264, and x265 are always linked statically from
    // the repo-managed static prefix built by setup-static-build.sh. The C++
    // runtime is resolved from the active compiler toolchain and linked
    // explicitly below.
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing"));
    let prefix = manifest_dir.join(".build/static/prefix");
    let lib_dir = prefix.join("lib");
    let pkgconfig_dir = lib_dir.join("pkgconfig");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu")
    {
        panic!("restream static native build is currently supported only for linux-gnu targets");
    }

    // Keep pkg-config inside the generated static prefix. A host fallback would
    // make release binaries depend on whichever native packages happen to be
    // installed on the build machine.
    // SAFETY: build scripts are single-threaded at the point this runs.
    unsafe {
        std::env::set_var("PKG_CONFIG_LIBDIR", pkgconfig_dir.display().to_string());
        std::env::remove_var("PKG_CONFIG_PATH");
        std::env::remove_var("PKG_CONFIG_SYSROOT_DIR");
    }

    check_required_static_inputs(&prefix);
    embed_native_input_inventory(&prefix);

    let pc_path = pkgconfig_dir.join("srt.pc");

    println!(
        "cargo:rustc-env=RESTREAM_NATIVE_BUILD_ID={}",
        native_build_id(&prefix)
    );

    let srt_version = std::fs::read_to_string(&pc_path)
        .ok()
        .and_then(|pc| {
            pc.lines()
                .find_map(|line| line.strip_prefix("Version: ").map(str::to_owned))
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=RESTREAM_BUILD_SRT_VERSION={srt_version}");

    // SRT is C++; place libstdc++ after all Rust/native objects so GNU ld
    // resolves C++ symbols from SRT before closing the static archive group.
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    let output = std::process::Command::new("c++")
        .arg("-print-file-name=libstdc++.a")
        .output()
        .expect("failed to ask C++ compiler for libstdc++.a");
    let archive_path = String::from_utf8(output.stdout)
        .expect("C++ compiler returned a non-UTF-8 libstdc++.a path");
    let archive_path = Path::new(archive_path.trim());
    let stdcxx_dir = archive_path
        .parent()
        .filter(|p| archive_path.is_absolute() && p.exists())
        .expect("C++ compiler did not return an absolute libstdc++.a path");
    println!("cargo:rustc-link-search=native={}", stdcxx_dir.display());

    let mbedtls = probe_pinned_package("mbedcrypto", &prefix, false);
    for path in &mbedtls.link_paths {
        println!("cargo:rustc-link-search=native={}", path.display());
    }

    println!("cargo:rustc-link-lib=static=srt");
    // SRT references symbols from all three Mbed TLS archives; mbedtls depends
    // on mbedx509 which depends on mbedcrypto, so list them in that order.
    println!("cargo:rustc-link-lib=static=mbedtls");
    println!("cargo:rustc-link-lib=static=mbedx509");
    println!("cargo:rustc-link-lib=static=mbedcrypto");
    println!("cargo:rustc-link-arg=-Wl,-Bstatic");
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    println!("cargo:rustc-link-arg=-lstdc++");
    println!("cargo:rustc-link-arg=-lm");
    println!("cargo:rustc-link-arg=-lpthread");
    println!("cargo:rustc-link-arg=-ldl");
    println!("cargo:rustc-link-arg=-lc");
    println!("cargo:rustc-link-arg=-lgcc_eh");
    println!("cargo:rustc-link-arg=-lgcc");
    println!("cargo:rustc-link-arg=-Wl,--end-group");
    println!("cargo:rustc-link-arg=-Wl,-Bdynamic");

    let avcodec = probe_pinned_package("libavcodec", &prefix, true);
    if avcodec
        .version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= 60)
    {
        println!("cargo:rustc-cfg=restream_ffmpeg_needs_avcodec_close_shim");
    }
    for package in [
        "libavformat",
        "libavfilter",
        "libswscale",
        "libswresample",
        "libavutil",
    ] {
        probe_pinned_package(package, &prefix, true);
    }

    embed_pkg_version("RESTREAM_BUILD_X264_VERSION", "x264");
    embed_pkg_version("RESTREAM_BUILD_X265_VERSION", "x265");
    embed_pkg_version("RESTREAM_BUILD_MBEDTLS_VERSION", "mbedcrypto");
}

fn embed_pkg_version(env_name: &str, package: &str) {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing"));
    let prefix = manifest_dir.join(".build/static/prefix");
    let cargo_metadata = matches!(package, "x264" | "x265");
    let version = probe_pinned_package(package, &prefix, cargo_metadata).version;
    println!("cargo:rustc-env={env_name}={version}");
}

const REQUIRED_PKG_CONFIG_PACKAGES: &[&str] = &[
    "srt",
    "mbedtls",
    "mbedx509",
    "mbedcrypto",
    "libavcodec",
    "libavformat",
    "libavfilter",
    "libswscale",
    "libswresample",
    "libavutil",
    "x264",
    "x265",
];

const REQUIRED_STATIC_ARCHIVES: &[&str] = &[
    "libsrt.a",
    "libmbedtls.a",
    "libmbedx509.a",
    "libmbedcrypto.a",
    "libp256m.a",
    "libeverest.a",
    "libavcodec.a",
    "libavformat.a",
    "libavfilter.a",
    "libswscale.a",
    "libswresample.a",
    "libavutil.a",
    "libx264.a",
    "libx265.a",
];

struct BuildIdentity {
    commit: String,
    timestamp: String,
}

fn build_identity() -> BuildIdentity {
    let commit = std::env::var("RESTREAM_BUILD_GIT_COMMIT")
        .or_else(|_| std::env::var("GITHUB_SHA"))
        .unwrap_or_else(|_| git_output(["rev-parse", "--verify", "HEAD"]));
    let commit = commit.trim().to_string();
    if commit.is_empty() {
        panic!("build provenance missing: set RESTREAM_BUILD_GIT_COMMIT when building without git");
    }

    if let Ok(path) = git_output_checked(["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={}", path.trim());
    }
    if let Ok(path) = git_output_checked(["rev-parse", "--git-path", "index"]) {
        println!("cargo:rerun-if-changed={}", path.trim());
    }

    let timestamp = std::env::var("RESTREAM_BUILD_TIMESTAMP")
        .or_else(|_| std::env::var("SOURCE_DATE_EPOCH"))
        .unwrap_or_else(|_| git_output(["show", "-s", "--format=%cI", "HEAD"]));
    let timestamp = timestamp.trim().to_string();
    if timestamp.is_empty() {
        panic!(
            "build provenance missing: set RESTREAM_BUILD_TIMESTAMP or SOURCE_DATE_EPOCH when building without git"
        );
    }

    BuildIdentity { commit, timestamp }
}

fn git_output<const N: usize>(args: [&str; N]) -> String {
    git_output_checked(args).unwrap_or_else(|error| panic!("{error}"))
}

fn git_output_checked<const N: usize>(args: [&str; N]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(args)
        .output()
        .map_err(|error| format!("failed to run git for build provenance: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git provenance command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("git output was not UTF-8: {error}"))
}

fn probe_pinned_package(package: &str, prefix: &Path, cargo_metadata: bool) -> pkg_config::Library {
    let library = pkg_config::Config::new()
        .statik(true)
        .cargo_metadata(cargo_metadata)
        .probe(package)
        .unwrap_or_else(|error| {
            panic!(
                "{package} not found in repo static prefix {}: {error}. Run `scripts/resource-limit ./scripts/setup-static-build.sh` first.",
                prefix.display()
            )
        });
    assert_pinned_paths(package, prefix, &library.link_paths);
    assert_pinned_paths(package, prefix, &library.include_paths);
    library
}

fn check_required_static_inputs(prefix: &Path) {
    let lib_dir = prefix.join("lib");
    let pkgconfig_dir = lib_dir.join("pkgconfig");

    for archive in REQUIRED_STATIC_ARCHIVES {
        let path = lib_dir.join(archive);
        println!("cargo:rerun-if-changed={}", path.display());
        assert_required_file(
            &path,
            &format!(
                "repo static archive is missing: {}. Run `scripts/resource-limit ./scripts/setup-static-build.sh` first.",
                path.display()
            ),
        );
    }

    for package in REQUIRED_PKG_CONFIG_PACKAGES {
        let path = pkgconfig_dir.join(format!("{package}.pc"));
        println!("cargo:rerun-if-changed={}", path.display());
        assert_required_file(
            &path,
            &format!(
                "repo static pkg-config file is missing: {}. Run `scripts/resource-limit ./scripts/setup-static-build.sh` first.",
                path.display()
            ),
        );
    }
}

fn assert_required_file(path: &Path, message: &str) {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        _ => panic!("{message}"),
    }
}

fn embed_native_input_inventory(prefix: &Path) {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR missing"));
    let path = out_dir.join("native-build-inputs.json");
    let mut inputs = Vec::new();

    for archive in REQUIRED_STATIC_ARCHIVES {
        inputs.push(native_input(
            prefix,
            &format!("lib/{archive}"),
            "static-archive",
        ));
    }
    for package in REQUIRED_PKG_CONFIG_PACKAGES {
        inputs.push(native_input(
            prefix,
            &format!("lib/pkgconfig/{package}.pc"),
            "pkg-config",
        ));
    }
    inputs.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));

    let bytes =
        serde_json::to_vec_pretty(&inputs).expect("native build input inventory should serialize");
    std::fs::write(&path, bytes).unwrap_or_else(|error| {
        panic!(
            "failed to write native build input inventory {}: {error}",
            path.display()
        )
    });
}

fn native_input(prefix: &Path, relative_path: &str, kind: &str) -> serde_json::Value {
    let path = prefix.join(relative_path);
    json!({
        "kind": kind,
        "path": relative_path,
        "sha256": file_sha256(&path),
    })
}

fn file_sha256(path: &Path) -> String {
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|error| panic!("failed to open native input {}: {error}", path.display()));
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = file.read(&mut buffer).unwrap_or_else(|error| {
            panic!("failed to hash native input {}: {error}", path.display())
        });
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    to_hex(&hasher.finalize())
}

fn assert_pinned_paths(package: &str, prefix: &Path, paths: &[PathBuf]) {
    let prefix = prefix
        .canonicalize()
        .unwrap_or_else(|error| panic!("failed to canonicalize {}: {error}", prefix.display()));
    for path in paths {
        let canonical = path.canonicalize().unwrap_or_else(|error| {
            panic!(
                "pkg-config package {package} resolved missing path {}: {error}",
                path.display()
            )
        });
        if !canonical.starts_with(&prefix) {
            panic!(
                "pkg-config package {package} resolved host path {}; expected all native inputs under {}",
                canonical.display(),
                prefix.display()
            );
        }
    }
}

fn native_build_id(prefix: &Path) -> String {
    let mut files = Vec::new();
    collect_native_inputs(&prefix.join("lib"), &mut files);
    collect_native_inputs(&prefix.join("include"), &mut files);
    files.sort();
    if files.is_empty() {
        panic!("native build provenance missing: no static prefix inputs found");
    }

    let mut hasher = Sha256::new();
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        let relative = path.strip_prefix(prefix).unwrap_or(&path);
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        let mut file = std::fs::File::open(&path).unwrap_or_else(|error| {
            panic!("failed to open native input {}: {error}", path.display())
        });
        let mut buffer = [0u8; 8192];
        loop {
            let read = file.read(&mut buffer).unwrap_or_else(|error| {
                panic!("failed to hash native input {}: {error}", path.display())
            });
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    to_hex(&hasher.finalize())
}

fn collect_native_inputs(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let entry =
            entry.unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()));
        let path = entry.path();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|error| panic!("failed to inspect {}: {error}", path.display()));
        if file_type.is_dir() {
            collect_native_inputs(&path, files);
        } else if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("a" | "pc" | "h" | "hpp")
        ) {
            files.push(path);
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn embed_toolchain_versions() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = std::process::Command::new(rustc)
        .args(["--version", "--verbose"])
        .output()
        .expect("failed to query rustc version");
    let text = String::from_utf8(output.stdout).expect("rustc version was not UTF-8");

    for (key, env_name) in [
        ("release", "RESTREAM_RUSTC_VERSION"),
        ("host", "RESTREAM_RUSTC_HOST"),
        ("LLVM version", "RESTREAM_LLVM_VERSION"),
    ] {
        let value = text
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}: ")))
            .unwrap_or("unknown");
        println!("cargo:rustc-env={env_name}={value}");
    }

    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_string());
    let output = std::process::Command::new(cxx)
        .args(["-dumpfullversion", "-dumpversion"])
        .output()
        .expect("failed to query C++ compiler version");
    let version = String::from_utf8(output.stdout).expect("C++ compiler version was not UTF-8");
    println!(
        "cargo:rustc-env=RESTREAM_GCC_RUNTIME_VERSION={}",
        version.trim()
    );
}

fn embed_rust_dependency_inventory() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let target = std::env::var("TARGET").expect("TARGET missing");
    let output = std::process::Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            &target,
        ])
        .output()
        .expect("failed to run cargo metadata");
    if !output.status.success() {
        panic!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("invalid cargo metadata JSON");
    let packages = metadata["packages"]
        .as_array()
        .expect("cargo metadata packages missing");
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .expect("cargo metadata resolve graph missing");
    let root_id = metadata["resolve"]["root"]
        .as_str()
        .expect("cargo metadata root missing");

    let mut package_by_id = std::collections::HashMap::new();
    for package in packages {
        if let Some(id) = package["id"].as_str() {
            package_by_id.insert(id, package);
        }
    }

    let mut node_by_id = std::collections::HashMap::new();
    for node in nodes {
        if let Some(id) = node["id"].as_str() {
            node_by_id.insert(id, node);
        }
    }
    let package_checksums = package_checksums_from_lock();

    let mut pending = vec![root_id.to_string()];
    let mut visited = std::collections::HashSet::new();
    let mut dependencies = Vec::new();

    while let Some(id) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let Some(node) = node_by_id.get(id.as_str()) else {
            continue;
        };
        if id != root_id {
            let package = package_by_id
                .get(id.as_str())
                .expect("resolved package missing from metadata");
            let is_runtime_library = package["targets"]
                .as_array()
                .map(|targets| {
                    targets.iter().any(|target| {
                        target["kind"].as_array().is_some_and(|kinds| {
                            kinds.iter().any(|kind| {
                                matches!(
                                    kind.as_str(),
                                    Some("lib" | "rlib" | "dylib" | "staticlib" | "cdylib")
                                )
                            })
                        })
                    })
                })
                .unwrap_or(false);
            if !is_runtime_library {
                // Procedural macros and build tools execute during compilation
                // but are not linked into the shipped runtime artifact.
                continue;
            }
            let name = package["name"].as_str().unwrap_or("unknown");
            let version = package["version"].as_str().unwrap_or("unknown");
            let bom_ref = format!("pkg:cargo/{name}@{version}");
            let features: Vec<&str> = node["features"]
                .as_array()
                .map(|features| {
                    features
                        .iter()
                        .filter_map(|feature| feature.as_str())
                        .collect()
                })
                .unwrap_or_default();
            let dependency_refs: Vec<String> = node["deps"]
                .as_array()
                .map(|deps| {
                    deps.iter()
                        .filter(|dep| {
                            dep["dep_kinds"]
                                .as_array()
                                .map(|kinds| {
                                    kinds.iter().any(|kind| {
                                        kind["kind"].is_null() || kind["kind"] == "normal"
                                    })
                                })
                                .unwrap_or(true)
                        })
                        .filter_map(|dep| dep["pkg"].as_str())
                        .filter_map(|pkg_id| package_by_id.get(pkg_id).copied())
                        .filter_map(|package| {
                            package["name"]
                                .as_str()
                                .zip(package["version"].as_str())
                                .map(|(name, version)| format!("pkg:cargo/{name}@{version}"))
                        })
                        .collect()
                })
                .unwrap_or_default();
            dependencies.push(serde_json::json!({
                "bomRef": bom_ref,
                "name": name,
                "version": version,
                "source": package["source"],
                "checksum": package_checksums.get(&(name.to_string(), version.to_string()))
                    .map(String::as_str),
                "license": package["license"],
                "features": features,
                "dependencyRefs": dependency_refs,
            }));
        }

        let Some(deps) = node["deps"].as_array() else {
            continue;
        };
        for dep in deps {
            let is_runtime = dep["dep_kinds"]
                .as_array()
                .map(|kinds| {
                    kinds
                        .iter()
                        .any(|kind| kind["kind"].is_null() || kind["kind"] == "normal")
                })
                .unwrap_or(true);
            if is_runtime && let Some(package_id) = dep["pkg"].as_str() {
                pending.push(package_id.to_string());
            }
        }
    }

    dependencies.sort_by(|left, right| {
        left["name"]
            .as_str()
            .cmp(&right["name"].as_str())
            .then_with(|| left["version"].as_str().cmp(&right["version"].as_str()))
    });

    let output_path =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR missing"))
            .join("rust-runtime-dependencies.json");
    std::fs::write(
        output_path,
        serde_json::to_vec(&dependencies).expect("failed to serialize dependency inventory"),
    )
    .expect("failed to write dependency inventory");
}

fn package_checksums_from_lock() -> std::collections::HashMap<(String, String), String> {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing"));
    let lock_path = manifest_dir.join("Cargo.lock");
    let lock = std::fs::read_to_string(&lock_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", lock_path.display()));
    let mut checksums = std::collections::HashMap::new();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut checksum: Option<String> = None;

    let mut flush =
        |name: &mut Option<String>, version: &mut Option<String>, checksum: &mut Option<String>| {
            if let (Some(name), Some(version), Some(checksum)) =
                (name.take(), version.take(), checksum.take())
            {
                checksums.insert((name, version), checksum);
            } else {
                *name = None;
                *version = None;
                *checksum = None;
            }
        };

    for line in lock.lines() {
        if line == "[[package]]" {
            flush(&mut name, &mut version, &mut checksum);
        } else if let Some(value) = line.strip_prefix("name = ") {
            name = Some(value.trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("version = ") {
            version = Some(value.trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("checksum = ") {
            checksum = Some(value.trim_matches('"').to_string());
        }
    }
    flush(&mut name, &mut version, &mut checksum);
    checksums
}
