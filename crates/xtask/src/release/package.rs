use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::common::{git, install, require_commands, require_version};
use super::native::{self, NativeBuild};
use crate::workflow::{Error, OwnedChild, WorkflowResult, checked, output};

struct PackageRole {
    prefix: &'static str,
    binary: &'static str,
    compliance: bool,
}

// Sole role catalog: the server ships compliance material; helper archives are executable-only.
const ROLES: &[PackageRole] = &[
    PackageRole {
        prefix: "restream",
        binary: "restream",
        compliance: true,
    },
    PackageRole {
        prefix: "restream-mcp",
        binary: "restream-mcp",
        compliance: false,
    },
    PackageRole {
        prefix: "test-harness",
        binary: "test_harness",
        compliance: false,
    },
];

pub(crate) fn package_binaries(version: &str) -> WorkflowResult {
    require_version("package-binaries", version)?;
    require_commands(&["cargo", "npm", "tar"])?;
    let arch = match env::var("RESTREAM_RELEASE_ARCH")
        .ok()
        .filter(|s| !s.is_empty())
    {
        Some(arch) => arch,
        None => {
            let machine = output(Command::new("uname").arg("-m"))?;
            format!(
                "linux-{}",
                String::from_utf8_lossy(&machine.stdout).trim_end_matches('\n')
            )
        }
    };
    let out_dir = PathBuf::from(crate::workflow::env_or("RESTREAM_RELEASE_DIR", "dist"));
    let sbom_name = format!("restream-{version}-{arch}.sbom.cdx.json");
    let sbom = out_dir.join(&sbom_name);
    super::prepare_build_tree()?;
    native::build(&NativeBuild {
        profile: "release".to_owned(),
        bins: ROLES.iter().map(|role| role.binary.to_owned()).collect(),
        explicit_bins: true,
        features: "mcp-server,mcp-http-backend".to_owned(),
        sbom: sbom.clone(),
    })?;
    for role in ROLES {
        let binary = Path::new("target/release").join(role.binary);
        if !crate::workflow::executable(&binary) {
            return Err(format!(
                "package-binaries: expected built executable is missing: {}",
                binary.display()
            )
            .into());
        }
    }
    if !crate::workflow::nonempty(&sbom) {
        return Err(format!(
            "package-binaries: expected release SBOM is missing: {}",
            sbom.display()
        )
        .into());
    }
    let temp = crate::workflow::TempDir::new("restream-xtask")?;
    for role in ROLES {
        let name = package_name(role, version, &arch);
        let stage = temp.path().join(name);
        fs::create_dir_all(&stage)?;
        install(
            &Path::new("target/release").join(role.binary),
            &stage.join(role.binary),
            0o755,
        )?;
        if role.compliance {
            install(Path::new("LICENSE.md"), &stage.join("LICENSE.md"), 0o644)?;
            install(
                Path::new("distribution/THIRD_PARTY_COMPONENTS.md"),
                &stage.join("THIRD_PARTY_COMPONENTS.md"),
                0o644,
            )?;
            fs::create_dir_all(stage.join("licenses"))?;
            // Preserve the complete license directory, including modes and symlinks.
            checked(
                Command::new("cp")
                    .args(["-a", "distribution/licenses/."])
                    .arg(stage.join("licenses")),
            )?;
            install(&sbom, &stage.join(&sbom_name), 0o644)?;
        }
    }
    fs::create_dir_all(&out_dir)?;
    for role in ROLES {
        let name = package_name(role, version, &arch);
        let archive = out_dir.join(format!("{name}.tar.gz"));
        checked(
            Command::new("tar")
                .args([
                    "--sort=name",
                    "--mtime=UTC 1970-01-01",
                    "--owner=0",
                    "--group=0",
                    "--numeric-owner",
                    "-C",
                ])
                .arg(temp.path())
                .arg("-czf")
                .arg(&archive)
                .arg(&name),
        )?;
        println!("package-binaries: PASS archive={}", archive.display());
    }
    if crate::workflow::env_or("RESTREAM_RELEASE_IMAGE", "1") == "1" {
        package_runtime_image(version)?;
    }
    println!("package-binaries: PASS sbom={}", sbom.display());
    Ok(())
}

fn package_name(role: &PackageRole, version: &str, arch: &str) -> String {
    format!("{}-{version}-{arch}", role.prefix)
}

pub(crate) fn runtime_image_info(version: &str) -> Result<(String, String), Error> {
    require_version("package-runtime-image", version)?;
    Ok((
        format!(
            "{}/restream-{version}-oci.tar.gz",
            crate::workflow::env_or("RESTREAM_RELEASE_DIR", "dist")
        ),
        crate::workflow::env_or("RESTREAM_CONTAINER_IMAGE", "restream:release"),
    ))
}

pub(super) fn package_runtime_image(version: &str) -> WorkflowResult {
    require_version("package-runtime-image", version)?;
    require_commands(&["docker", "gzip"])?;
    let out_dir = PathBuf::from(crate::workflow::env_or("RESTREAM_RELEASE_DIR", "dist"));
    let (archive, image) = runtime_image_info(version)?;
    let seccomp_source = Path::new("distribution/docker/restream-seccomp.json");
    let seccomp = out_dir.join(format!("restream-{version}-seccomp.json"));
    let binary = PathBuf::from(crate::workflow::env_or(
        "RESTREAM_RELEASE_BINARY",
        "target/release/restream",
    ));
    if !crate::workflow::executable(&binary) {
        return Err(format!(
            "package-runtime-image: expected built executable is missing: {}",
            binary.display()
        )
        .into());
    }
    let temp = crate::workflow::TempDir::new("restream-xtask")?;
    let payload = temp.path().join("release-payload");
    fs::create_dir_all(&payload)?;
    fs::copy(&binary, payload.join("restream"))?;
    let commit = git(&["rev-parse", "HEAD"])?;
    let timestamp = git(&["show", "-s", "--format=%cI", "HEAD"])?;
    let mut docker = Command::new("docker");
    let cache = crate::workflow::env_or("RESTREAM_DOCKER_GHA_CACHE", "0") == "1";
    if cache {
        docker.args(["buildx", "build"]);
    } else {
        docker.arg("build");
    }
    docker
        .args([
            "--file",
            "Dockerfile",
            "--target",
            "runtime-artifact",
            "--build-context",
        ])
        .arg(format!("release_payload={}", payload.display()))
        .arg("--build-arg")
        .arg(format!("RESTREAM_BUILD_GIT_COMMIT={commit}"))
        .arg("--build-arg")
        .arg(format!("RESTREAM_BUILD_TIMESTAMP={timestamp}"))
        .arg("-t")
        .arg(&image)
        .arg(".");
    if cache {
        docker.args([
            "--cache-from",
            "type=gha,scope=runtime-release-package",
            "--cache-to",
            "type=gha,mode=max,scope=runtime-release-package",
            "--load",
        ]);
    }
    checked(&mut docker)?;
    fs::create_dir_all(&out_dir)?;
    save_image(&image, Path::new(&archive))?;
    if !crate::workflow::nonempty(seccomp_source) {
        return Err(format!(
            "package-runtime-image: missing supported seccomp profile: {}",
            seccomp_source.display()
        )
        .into());
    }
    install(seccomp_source, &seccomp, 0o644)?;
    println!(
        "package-runtime-image: PASS archive={archive} seccomp={} image={image}",
        seccomp.display()
    );
    Ok(())
}

fn save_image(image: &str, archive: &Path) -> WorkflowResult {
    let destination = File::create(archive)?;
    let mut docker = OwnedChild::spawn(
        Command::new("docker")
            .args(["save", image])
            .stdout(Stdio::piped()),
    )?;
    let pipe = docker
        .child()
        .stdout
        .take()
        .ok_or("docker save stdout pipe unavailable")?;
    let mut gzip = OwnedChild::spawn(
        Command::new("gzip")
            .arg("-n")
            .stdin(Stdio::from(pipe))
            .stdout(destination),
    )?;
    let docker_status = docker.wait()?;
    let gzip_status = gzip.wait()?;
    // Bash pipefail returns the rightmost failed command.
    if !gzip_status.success() {
        return Err(Error::status(gzip_status, "gzip image archive failed"));
    }
    if !docker_status.success() {
        return Err(Error::status(docker_status, "docker save failed"));
    }
    Ok(())
}

pub(super) fn checksums(args: &[&str]) -> WorkflowResult {
    if args.len() > 1 || matches!(args.first(), Some(&"-h" | &"--help")) {
        return Err(Error::usage(
            "usage: cargo xtask release checksums [dist-dir]",
        ));
    }
    let directory = args.first().copied().unwrap_or("dist");
    let mut groups = [Vec::new(), Vec::new(), Vec::new()];
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let filename = entry.file_name();
        let name = filename.to_string_lossy();
        if let Some(group) = checksum_group(&name) {
            groups[group].push(filename);
        }
    }
    for group in &mut groups {
        group.sort();
        if group.is_empty() {
            return Err("release checksums: required archive/JSON artifact group is empty".into());
        }
    }
    let output_file = File::create(Path::new(directory).join("SHA256SUMS"))?;
    checked(
        Command::new("sha256sum")
            .current_dir(directory)
            .args(groups.into_iter().flatten())
            .stdout(output_file),
    )
}

fn checksum_group(name: &str) -> Option<usize> {
    if name.starts_with("restream-") && name.ends_with(".tar.gz") {
        Some(0)
    } else if name.starts_with("restream-") && name.ends_with(".json") {
        Some(1)
    } else if name.starts_with("test-harness-") && name.ends_with(".tar.gz") {
        Some(2)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_roles_keep_helper_archives_minimal() {
        let names: Vec<_> = ROLES
            .iter()
            .map(|role| package_name(role, "v1.2.3", "linux-aarch64"))
            .collect();
        assert_eq!(
            names,
            [
                "restream-v1.2.3-linux-aarch64",
                "restream-mcp-v1.2.3-linux-aarch64",
                "test-harness-v1.2.3-linux-aarch64"
            ]
        );
        assert_eq!(
            ROLES
                .iter()
                .filter(|r| r.compliance)
                .map(|r| r.binary)
                .collect::<Vec<_>>(),
            ["restream"]
        );
        assert_eq!(ROLES[2].binary, "test_harness");
    }

    #[test]
    fn checksum_selection_covers_all_distributed_roles_and_metadata() {
        for name in [
            "restream-v1-oci.tar.gz",
            "restream-mcp-v1-linux-x86_64.tar.gz",
        ] {
            assert_eq!(checksum_group(name), Some(0));
        }
        assert_eq!(
            checksum_group("restream-v1-linux-aarch64.sbom.cdx.json"),
            Some(1)
        );
        assert_eq!(checksum_group("restream-v1-seccomp.json"), Some(1));
        assert_eq!(
            checksum_group("test-harness-v1-linux-aarch64.tar.gz"),
            Some(2)
        );
        assert_eq!(checksum_group("SHA256SUMS"), None);
        assert_eq!(checksum_group("unrelated.tar.gz"), None);
    }
}
