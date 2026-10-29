use super::{artifact, support};
use crate::workflow::{Error, WorkflowResult, checked};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const USAGE: &str = "usage: cargo xtask qualify release-evidence [runtime-image-archive] [sbom (dist/restream-runtime.cdx.json)] [binary-bundle]
Runs cargo-audit, cargo-deny, grype and trivy, certifies the bundle and seccomp profile, then smokes the image.";

pub(super) fn run(args: &[&str]) -> WorkflowResult {
    if matches!(args.first(), Some(&"-h" | &"--help")) {
        println!("{USAGE}");
        return Ok(());
    }
    let archive = args.first().copied().unwrap_or("");
    // `${2:-dist/restream-runtime.cdx.json}`: an empty argument takes the default.
    let sbom = Path::new(
        args.get(1)
            .copied()
            .filter(|s| !s.is_empty())
            .unwrap_or("dist/restream-runtime.cdx.json"),
    );
    let bundle = args.get(2).copied().unwrap_or("");
    for tool in ["cargo-audit", "cargo-deny", "grype", "trivy"] {
        support::require_command(tool, "release-evidence")?;
    }
    checked(Command::new("cargo").arg("audit"))?;
    checked(Command::new("cargo").args([
        "deny",
        "check",
        "advisories",
        "licenses",
        "bans",
        "sources",
    ]))?;
    if !bundle.is_empty() {
        for tool in ["ldd", "tar"] {
            support::require_command(tool, "release-evidence")?;
        }
        if !crate::workflow::nonempty(Path::new(bundle)) {
            return Err(format!("release-evidence: binary bundle was not found: {bundle}").into());
        }
        let extracted = crate::workflow::TempDir::new("restream-qualify")?;
        let bundle_root = extracted.path().join("bundle");
        fs::create_dir(&bundle_root)?;
        checked(
            Command::new("tar")
                .args(["-xzf", bundle, "-C"])
                .arg(&bundle_root),
        )?;
        let binary = certified_binary(&bundle_root)?;
        // A statically linked executable makes ldd fail; the original gate only
        // rejects an actual libsrt dependency, not ldd's static-binary exit code.
        let link_log = extracted.path().join("ldd.log");
        let log = fs::File::create(&link_log)?;
        let mut child = crate::workflow::spawn(
            Command::new("ldd")
                .arg(&binary)
                .stdout(log.try_clone()?)
                .stderr(log),
        )?;
        let _status = crate::workflow::wait(&mut child)?;
        let links = fs::read(link_log)?;
        if links.windows(6).any(|w| w == b"libsrt") {
            return Err(format!(
                "release-evidence: packaged binary still links libsrt dynamically: {}",
                binary.display()
            )
            .into());
        }
        let sboms = support::sboms(&bundle_root, true)?;
        let [source] = sboms.as_slice() else {
            return Err(
                format!("release-evidence: expected exactly one SBOM inside {bundle}").into(),
            );
        };
        if let Some(parent) = sbom.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, sbom)?;
    } else {
        checked(
            support::xtask()?
                .args(["release", "app-native"])
                .env("RESTREAM_SBOM_PATH", sbom)
                .env("RESTREAM_BUILD_PROFILE", "release"),
        )?;
    }
    if !crate::workflow::nonempty(sbom) {
        return Err(format!("release-evidence: SBOM was not written: {}", sbom.display()).into());
    }
    checked(
        Command::new("grype")
            .arg(format!("sbom:{}", sbom.display()))
            .args(["--fail-on", "high"]),
    )?;
    checked(
        Command::new("trivy")
            .args(["sbom", "--exit-code", "1", "--severity", "HIGH,CRITICAL"])
            .arg(sbom),
    )?;
    if archive.is_empty() {
        crate::container::run(&["smoke", "--image", "restream:release"])
    } else {
        if !bundle.is_empty() {
            artifact::run(&[bundle])?;
        }
        let profile = seccomp_path(archive);
        certify_seccomp(
            &profile,
            Path::new("distribution/docker/restream-seccomp.json"),
        )?;
        crate::container::run(&[
            "smoke",
            "--image",
            "restream:release",
            "--load-archive",
            archive,
            "--seccomp-profile",
            crate::path_str(&profile)?,
        ])
    }
}

fn certified_binary(root: &Path) -> Result<PathBuf, Error> {
    let binaries = support::files(root, true)?
        .into_iter()
        .filter(|path| {
            fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 == 0o111)
        })
        .collect::<Vec<_>>();
    match binaries.as_slice() {
        [binary] if binary.file_name().is_some_and(|name| name == "restream") => Ok(binary.clone()),
        _ => Err(
            "release-evidence: expected exactly one executable named restream in binary bundle"
                .into(),
        ),
    }
}

fn seccomp_path(archive: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}-seccomp.json",
        archive.strip_suffix("-oci.tar.gz").unwrap_or(archive)
    ))
}

fn certify_seccomp(profile: &Path, canonical: &Path) -> WorkflowResult {
    if !crate::workflow::nonempty(profile) {
        return Err(format!("release-evidence: the runtime image archive is missing its seccomp profile beside it: {}", profile.display()).into());
    }
    if fs::read(profile)? != fs::read(canonical)? {
        return Err(format!(
            "release-evidence: {} differs from {}",
            profile.display(),
            canonical.display()
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certifies_the_exact_executable_set_not_just_a_named_binary() {
        let temp = crate::workflow::TempDir::new("restream-qualify").unwrap();
        let binary = temp.path().join("restream");
        fs::write(&binary, b"binary").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(certified_binary(temp.path()).unwrap(), binary);
        let extra = temp.path().join("helper");
        fs::write(&extra, b"helper").unwrap();
        fs::set_permissions(&extra, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(certified_binary(temp.path()).is_err());
        fs::remove_file(&extra).unwrap();
        fs::rename(&binary, &extra).unwrap();
        assert!(certified_binary(temp.path()).is_err());
    }

    #[test]
    fn seccomp_requires_nonempty_byte_identical_release_asset() {
        let temp = crate::workflow::TempDir::new("restream-qualify").unwrap();
        let canonical = temp.path().join("canonical.json");
        let release = temp.path().join("release.json");
        fs::write(&canonical, b"{}\n").unwrap();
        assert!(certify_seccomp(&release, &canonical).is_err());
        fs::write(&release, b"{}").unwrap();
        assert!(certify_seccomp(&release, &canonical).is_err());
        fs::write(&release, b"{}\n").unwrap();
        assert!(certify_seccomp(&release, &canonical).is_ok());
        assert_eq!(
            seccomp_path("dir with spaces/restream-v1-oci.tar.gz"),
            Path::new("dir with spaces/restream-v1-seccomp.json")
        );
        assert_eq!(
            seccomp_path("custom.tar.gz"),
            Path::new("custom.tar.gz-seccomp.json")
        );
    }
}
