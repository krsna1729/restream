use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::workflow::{Error, WorkflowResult, capture, checked, interrupted};

pub(super) struct NativeBuild {
    pub(super) profile: String,
    pub(super) bins: Vec<String>,
    pub(super) explicit_bins: bool,
    pub(super) features: String,
    pub(super) sbom: PathBuf,
}

impl NativeBuild {
    fn from_env() -> Self {
        let root = crate::repo_root();
        let bins = env::var("RESTREAM_BUILD_BINS").unwrap_or_default();
        Self {
            profile: crate::workflow::env_or("RESTREAM_BUILD_PROFILE", "debug"),
            bins: requested_bins(&bins),
            explicit_bins: !bins.is_empty(),
            features: env::var("RESTREAM_BUILD_FEATURES").unwrap_or_default(),
            sbom: env::var_os("RESTREAM_SBOM_PATH")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("dist/restream-runtime.cdx.json")),
        }
    }
}

// Bash `read -r -a` reads the first line and splits only IFS whitespace.
fn requested_bins(value: &str) -> Vec<String> {
    value
        .split('\n')
        .next()
        .unwrap_or_default()
        .split([' ', '\t'])
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

const USAGE: &str = "usage: cargo xtask release app-native
Builds RESTREAM_BUILD_BINS (restream) with RESTREAM_BUILD_PROFILE=debug|release and
RESTREAM_BUILD_FEATURES, verifies no native SRT/media linkage, then writes the SBOM to
RESTREAM_SBOM_PATH (dist/restream-runtime.cdx.json) unless RESTREAM_SKIP_SBOM=1.";

pub(super) fn run(args: &[&str]) -> WorkflowResult {
    match args {
        [] => build(&NativeBuild::from_env()),
        ["-h" | "--help"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(Error::usage(USAGE)),
    }
}

pub(crate) fn app_native_without_sbom() -> WorkflowResult {
    build_inner(&NativeBuild::from_env(), true)
}

pub(super) fn build(options: &NativeBuild) -> WorkflowResult {
    build_inner(
        options,
        crate::workflow::env_or("RESTREAM_SKIP_SBOM", "0") == "1",
    )
}

fn build_inner(options: &NativeBuild, skip_sbom: bool) -> WorkflowResult {
    if !matches!(options.profile.as_str(), "debug" | "release") {
        return Err(Error::usage(
            "RESTREAM_BUILD_PROFILE must be debug or release",
        ));
    }
    let mut cargo = Command::new("cargo");
    cargo.arg("build");
    if options.profile == "release" {
        cargo.arg("--release");
    }
    if !options.explicit_bins {
        cargo.args(["--bin", "restream"]);
    } else {
        for binary in &options.bins {
            cargo.args(["--bin", binary]);
        }
    }
    if !options.features.is_empty() {
        cargo.args(["--features", &options.features]);
    }
    checked(&mut cargo)?;

    let target = env::var_os("CARGO_TARGET_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    let binary_dir = target.join(&options.profile);
    let binary = binary_dir.join("restream");
    let linkage = ldd(&binary)?;
    println!("{linkage}");
    verify_linkage(&binary, &linkage)?;
    for name in &options.bins {
        let built = binary_dir.join(name);
        if !crate::workflow::executable(&built) {
            return Err(format!(
                "Native build verification failed: expected executable is missing: {}",
                built.display()
            )
            .into());
        }
        verify_linkage(&built, &ldd(&built)?)?;
    }
    println!(
        "Verified: {} links no native SRT or media library.",
        binary.display()
    );
    emit_sbom(&binary, &options.sbom, skip_sbom)
}

fn ldd(binary: &Path) -> Result<String, Error> {
    match capture(Command::new("ldd").arg(binary)) {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            Ok(text.trim_end_matches('\n').to_owned())
        }
        // The original check tolerates unavailable ldd and static executables,
        // but an interrupted workflow must never become a successful check.
        Err(error) if interrupted().is_none() => Ok(format!("ldd: {error}")),
        Err(error) => Err(error),
    }
}

fn verify_linkage(binary: &Path, linkage: &str) -> WorkflowResult {
    if ["libsrt", "libav", "libsw", "libx26"]
        .iter()
        .any(|name| linkage.contains(name))
    {
        return Err(format!(
            "Native linkage verification failed: {} links a native SRT or media library.",
            binary.display()
        )
        .into());
    }
    Ok(())
}

pub(super) fn emit_sbom_args(args: &[&str]) -> WorkflowResult {
    if args.len() != 2 {
        return Err(Error::usage(
            "usage: cargo xtask release emit-sbom <restream-binary> <sbom-path>",
        ));
    }
    emit_sbom(
        Path::new(args[0]),
        Path::new(args[1]),
        crate::workflow::env_or("RESTREAM_SKIP_SBOM", "0") == "1",
    )
}

fn emit_sbom(binary: &Path, sbom: &Path, skip: bool) -> WorkflowResult {
    if skip {
        println!("Skipping SBOM emission (RESTREAM_SKIP_SBOM=1).");
        return Ok(());
    }
    if let Some(parent) = sbom.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    checked(Command::new(binary).arg("--emit-sbom").arg(sbom))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_bin_environment_preserves_read_array_boundaries() {
        assert_eq!(
            requested_bins(" restream\t restream-mcp  test_harness\nignored"),
            ["restream", "restream-mcp", "test_harness"]
        );
        assert!(requested_bins("\t  ").is_empty());
        assert_eq!(requested_bins("binary\rname"), ["binary\rname"]);
        assert_eq!(requested_bins("literal\\name"), ["literal\\name"]);
    }

    #[test]
    fn rejects_every_forbidden_native_media_family() {
        for library in [
            "libsrt.so.1",
            "libavcodec.so",
            "libswresample.so",
            "libx265.so",
        ] {
            assert!(verify_linkage(Path::new("app"), library).is_err());
        }
        assert!(verify_linkage(Path::new("app"), "libc.so.6 => /lib/libc.so.6").is_ok());
        assert!(verify_linkage(Path::new("app"), "not a dynamic executable").is_ok());
    }
}
