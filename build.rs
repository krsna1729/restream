use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=RESTREAM_BUILD_GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=RESTREAM_BUILD_TIMESTAMP");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let build_identity = build_identity();
    println!("cargo:rustc-env=GIT_COMMIT_HASH={}", build_identity.commit);
    println!(
        "cargo:rustc-env=RESTREAM_BUILD_TIMESTAMP={}",
        build_identity.timestamp
    );
    embed_toolchain_versions();
    embed_rust_dependency_inventory();

    // SRT is pure Rust (srt-rs); no native media libraries are linked. FFmpeg
    // runs only as an external process (src/ffmpeg_binary.rs).
    println!("cargo:rustc-env=RESTREAM_BUILD_SRT_VERSION=srt-rs");
}

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

    // The C compiler that builds the bundled SQLite and ring and provides libgcc.
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let output = std::process::Command::new(cc)
        .args(["-dumpfullversion", "-dumpversion"])
        .output()
        .expect("failed to query C compiler version");
    let version = String::from_utf8(output.stdout).expect("C compiler version was not UTF-8");
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
