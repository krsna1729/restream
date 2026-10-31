//! Build provenance shared by the `restream` and `restream-mcp` build scripts:
//! the MCP sidecar refuses a target whose `/api/v1/engine` identity differs
//! from its own, so both must stamp the same commit.

/// Stamps `GIT_COMMIT_HASH` and `RESTREAM_BUILD_TIMESTAMP` into the crate.
pub fn emit_build_identity() {
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
