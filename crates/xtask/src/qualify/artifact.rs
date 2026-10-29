use super::support;
use crate::workflow::{Error, OwnedChild, WorkflowResult, checked};
use std::fs::{self, File};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const USAGE: &str =
    "usage: cargo xtask qualify release-artifact-smoke <restream-*-linux-x86_64.tar.gz>";

pub(super) fn run(args: &[&str]) -> WorkflowResult {
    if matches!(args.first(), Some(&"-h" | &"--help")) {
        println!("{USAGE}");
        return Ok(());
    }
    let bundle = args
        .first()
        .copied()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::usage(USAGE))?;
    if !crate::workflow::nonempty(Path::new(bundle)) {
        return Err(format!("release-artifact-smoke: bundle not found: {bundle}").into());
    }
    for tool in ["curl", "tar"] {
        support::require_command(tool, "release-artifact-smoke")?;
    }
    let temp = crate::workflow::TempDir::new("restream-qualify")?;
    checked(
        Command::new("tar")
            .args(["-xzf", bundle, "-C"])
            .arg(temp.path()),
    )?;
    let binary = validate_stage(temp.path())?;
    let port = tcp_port()?;
    let rtmp_port = tcp_port()?;
    let srt_port = UdpSocket::bind(("127.0.0.1", 0))?.local_addr()?.port();
    let runtime = temp.path().join("runtime");
    for directory in ["media", "logs", "cwd"] {
        fs::create_dir_all(runtime.join(directory))?;
    }
    let log_path = runtime.join("restream.log");
    let log = File::create(&log_path)?;
    let mut app = OwnedChild::spawn(
        Command::new(binary)
            .current_dir(runtime.join("cwd"))
            .env("RESTREAM_HTTP_BIND_ADDR", "127.0.0.1")
            .env("RESTREAM_HTTP_PORT", port.to_string())
            .env("RESTREAM_RTMP_PORT", rtmp_port.to_string())
            .env("RESTREAM_SRT_PORT", srt_port.to_string())
            .env("RESTREAM_DB_PATH", runtime.join("restream.db"))
            .env("RESTREAM_MEDIA_DIR", runtime.join("media"))
            .env("RESTREAM_LOG_DIR", runtime.join("logs"))
            .env("RESTREAM_INITIAL_ADMIN_PASSWORD", "release-smoke-password")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log),
    )?;
    fs::write(runtime.join("app.pid"), format!("{}\n", app.child().id()))?;
    let base = format!("http://127.0.0.1:{port}");
    for _ in 0..30 {
        if healthy(&base)? {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    if !healthy(&base)? {
        eprintln!("release-artifact-smoke: app did not become healthy; log follows");
        if let Ok(log) = fs::read_to_string(log_path) {
            eprint!("{log}");
        }
        return Err("release-artifact-smoke: app did not become healthy".into());
    }
    for (path, marker) in [
        ("/login", "Restream Login"),
        ("/output.css", "--color-base-100"),
        ("/base-path.js", "__RESTREAM_BASE_PATH__"),
        ("/js/control-room/main.js", "createRoot"),
        ("/js/vendor/react/jsx-runtime.js", "jsx"),
    ] {
        let file = download(&base, path, &runtime)?;
        contains(&file, path, marker)?;
    }
    let logo = download(&base, "/logo.png", &runtime)?;
    if !crate::workflow::nonempty(&logo) {
        return Err("release-artifact-smoke: /logo.png was empty".into());
    }
    let cookies = runtime.join("cookies.txt");
    checked(
        Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "-c"])
            .arg(&cookies)
            .args([
                "-H",
                "Content-Type: application/json",
                "-d",
                "{\"password\":\"release-smoke-password\"}",
            ])
            .arg(format!("{base}/api/v1/auth/login"))
            .stdout(Stdio::null()),
    )?;
    let index = runtime.join("index.html");
    checked(
        Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "-b"])
            .arg(cookies)
            .arg(format!("{base}/"))
            .arg("-o")
            .arg(&index),
    )?;
    contains(&index, "authenticated index", "js/control-room/main.js")?;
    println!("release-artifact-smoke: PASS bundle={bundle}");
    Ok(())
}

fn validate_stage(root: &Path) -> Result<PathBuf, Error> {
    let mut roots = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            roots.push(entry.path());
        }
    }
    let [stage] = roots.as_slice() else {
        return Err(
            "release-artifact-smoke: expected one top-level directory in binary bundle".into(),
        );
    };
    let binary = stage.join("restream");
    if !crate::workflow::executable(&binary) {
        return Err(format!(
            "release-artifact-smoke: missing executable restream binary: {}",
            binary.display()
        )
        .into());
    }
    for forbidden in ["run", "rootfs", "README.txt"] {
        if stage.join(forbidden).exists() {
            return Err(format!(
                "release-artifact-smoke: host binary archive must not contain {}",
                stage.join(forbidden).display()
            )
            .into());
        }
    }
    for required in [
        "LICENSE.md",
        "THIRD_PARTY_COMPONENTS.md",
        "licenses/Apache-2.0.txt",
        "licenses/GPL-3.0.txt",
    ] {
        if !stage.join(required).is_file() {
            return Err(format!("release-artifact-smoke: restream archive is missing release compliance file: {required}").into());
        }
    }
    if support::sboms(stage, false)?.len() != 1 {
        return Err("release-artifact-smoke: expected exactly one SBOM in restream archive".into());
    }
    // Current directory changes below; a relative TMPDIR must not make the binary relative.
    Ok(fs::canonicalize(binary)?)
}

fn tcp_port() -> Result<u16, Error> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

fn healthy(base: &str) -> Result<bool, Error> {
    let mut child = OwnedChild::spawn(
        Command::new("curl")
            .args(["--fail", "--silent", "--show-error"])
            .arg(format!("{base}/healthz"))
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )?;
    Ok(child.wait()?.success())
}

fn download(base: &str, path: &str, runtime: &Path) -> Result<PathBuf, Error> {
    let safe: String = path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let destination = runtime.join(format!("asset-{safe}"));
    checked(
        Command::new("curl")
            .args(["--fail", "--silent", "--show-error"])
            .arg(format!("{base}{path}"))
            .arg("-o")
            .arg(&destination),
    )?;
    Ok(destination)
}

fn contains(file: &Path, path: &str, marker: &str) -> WorkflowResult {
    if fs::read(file)?
        .windows(marker.len())
        .any(|bytes| bytes == marker.as_bytes())
    {
        Ok(())
    } else {
        Err(
            format!("release-artifact-smoke: {path} did not contain expected marker: {marker}")
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn archive_boundary_requires_compliance_and_exactly_one_top_level_sbom() {
        let temp = crate::workflow::TempDir::new("restream-qualify").unwrap();
        assert!(validate_stage(temp.path()).is_err());
        let stage = temp.path().join("restream-v1");
        fs::create_dir_all(stage.join("licenses")).unwrap();
        let binary = stage.join("restream");
        fs::write(&binary, b"binary").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        for file in [
            "LICENSE.md",
            "THIRD_PARTY_COMPONENTS.md",
            "licenses/Apache-2.0.txt",
            "licenses/GPL-3.0.txt",
        ] {
            fs::write(stage.join(file), b"license").unwrap();
        }
        assert!(validate_stage(temp.path()).is_err());
        fs::write(stage.join("release.sbom.cdx.json"), b"{}").unwrap();
        assert!(validate_stage(temp.path()).is_ok());
        fs::write(stage.join("extra.sbom.cdx.json"), b"{}").unwrap();
        assert!(validate_stage(temp.path()).is_err());
        fs::remove_file(stage.join("extra.sbom.cdx.json")).unwrap();
        fs::create_dir(stage.join("rootfs")).unwrap();
        assert!(validate_stage(temp.path()).is_err());
        fs::remove_dir(stage.join("rootfs")).unwrap();
        fs::create_dir(temp.path().join("extra-root")).unwrap();
        assert!(validate_stage(temp.path()).is_err());
    }

    #[test]
    fn expected_frontend_marker_must_be_in_downloaded_bytes() {
        let temp = crate::workflow::TempDir::new("restream-qualify").unwrap();
        let asset = temp.path().join("asset");
        fs::write(&asset, b"\xffcreateRoot\x00").unwrap();
        assert!(contains(&asset, "/entry.js", "createRoot").is_ok());
        assert!(contains(&asset, "/entry.js", "Restream Login").is_err());
    }
}
