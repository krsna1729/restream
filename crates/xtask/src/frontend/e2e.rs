use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::workflow::{self, Error, OwnedChild, WorkflowResult};

pub(super) fn run(root: &Path, args: &[&str]) -> WorkflowResult {
    let work_root = super::env_path("RESTREAM_E2E_WORK_ROOT", root.join(".local/e2e"));
    let work_root = root.join(work_root);
    let fixture = root.join("test/fixtures/media-library/colorbar-timer-2v16a.mp4");
    let app = root.join("target/debug/restream");

    if env::var("RESTREAM_E2E_SKIP_BUILD").as_deref() != Ok("1") {
        workflow::checked(
            Command::new("npm")
                .current_dir(root)
                .args(["run", "build:frontend"]),
        )?;
        // Reuse the native builder, including its linkage verification. Browser
        // tests do not regenerate a distribution SBOM or dirty tracked files.
        crate::release::app_native_without_sbom()?;
    }
    if !crate::workflow::executable(&app) {
        return Err(Error::exit(
            1,
            format!("frontend-e2e: expected app binary at {}", app.display()),
        ));
    }
    if !fixture.is_file() {
        return Err(Error::exit(
            1,
            format!(
                "frontend-e2e: checked-in fixture missing: {}",
                fixture.display()
            ),
        ));
    }

    // These are the only persistent directories this workflow owns. Keep the
    // application log/database and Playwright's configured artifacts on failure.
    remove_work_root(root, &work_root)?;
    for directory in ["media", "logs", "moq"] {
        fs::create_dir_all(work_root.join(directory))?;
    }
    fs::copy(
        &fixture,
        work_root.join("media").join(fixture.file_name().unwrap()),
    )?;
    workflow::checked(
        Command::new("openssl")
            .current_dir(root)
            .args([
                "req",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-nodes",
                "-days",
                "10",
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1",
                "-keyout",
            ])
            .arg(work_root.join("moq/key.pem"))
            .arg("-out")
            .arg(work_root.join("moq/cert.pem"))
            .stderr(Stdio::null()),
    )?;

    let log_path = work_root.join("restream.log");
    let log = File::create(&log_path)?;
    let _backend = OwnedChild::spawn(
        Command::new(&app)
            .current_dir(root)
            .env("RESTREAM_DB_PATH", work_root.join("restream.db"))
            .env("RESTREAM_MEDIA_DIR", work_root.join("media"))
            .env("RESTREAM_LOG_DIR", work_root.join("logs"))
            .env("RESTREAM_INITIAL_ADMIN_PASSWORD", "admin")
            .env("RESTREAM_MOQ_INGEST_LISTEN", "[::]:4443")
            .env("RESTREAM_MOQ_TLS_CERT", work_root.join("moq/cert.pem"))
            .env("RESTREAM_MOQ_TLS_KEY", work_root.join("moq/key.pem"))
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log)),
    )?;

    for _ in 0..30 {
        let healthy = OwnedChild::spawn(
            Command::new("curl")
                .args(["-fsS", "http://127.0.0.1:3030/healthz"])
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )?
        .wait()?
        .success();
        if healthy {
            // Do not replace the canonical Playwright configuration: it owns
            // Chromium/Firefox selection, screenshots, traces and output paths.
            // A separate owned group also contains test-spawned FFmpeg children.
            let status = OwnedChild::spawn(
                Command::new("npx")
                    .current_dir(root)
                    .args(["playwright", "test"])
                    .args(args),
            )?
            .wait()?;
            return if status.success() {
                Ok(())
            } else {
                Err(Error::status(status, "frontend-e2e: Playwright failed"))
            };
        }
        sleep_interruptibly(Duration::from_secs(1))?;
    }

    // Drop terminates only the group created above; never search for an app or
    // FFmpeg by name and never stop a pre-existing host process.
    eprintln!("frontend-e2e: app did not become healthy; log follows:");
    if let Ok(mut log) = File::open(&log_path) {
        let _ = io::copy(&mut log, &mut io::stderr().lock());
        let _ = io::stderr().flush();
    }
    Err(Error::exit(1, "frontend-e2e: app did not become healthy"))
}

/// Deletes the previous run's work root. A directory that is the checkout or
/// one of its ancestors (`.`, `..`, `/`) is refused before any traversal.
fn remove_work_root(root: &Path, path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            if fs::canonicalize(root)?.starts_with(fs::canonicalize(path)?) {
                return Err(io::Error::other(format!(
                    "frontend-e2e: refusing to delete {}: it contains the repository",
                    path.display()
                )));
            }
            fs::remove_dir_all(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sleep_interruptibly(duration: Duration) -> WorkflowResult {
    let deadline = Instant::now() + duration;
    loop {
        if let Some(signal) = workflow::interrupted() {
            return Err(Error::exit(
                (128 + signal) as u8,
                "frontend-e2e: interrupted",
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

#[cfg(test)]
mod tests {
    use super::remove_work_root;
    use std::fs;

    #[test]
    fn work_root_cleanup_refuses_the_checkout_and_its_ancestors() {
        let base = std::env::temp_dir().join(format!("xtask-e2e-root-{}", std::process::id()));
        let repo = base.join("repo");
        let owned = repo.join(".local/e2e");
        fs::create_dir_all(owned.join("logs")).unwrap();
        fs::write(repo.join("Cargo.toml"), "").unwrap();

        for protected in [
            repo.join("."),
            repo.clone(),
            repo.join(".local/.."),
            base.clone(),
        ] {
            assert!(
                remove_work_root(&repo, &protected).is_err(),
                "{}",
                protected.display()
            );
            assert!(repo.join("Cargo.toml").is_file(), "{}", protected.display());
        }
        remove_work_root(&repo, &owned).unwrap();
        assert!(!owned.exists());
        assert!(repo.join("Cargo.toml").is_file());
        fs::remove_dir_all(&base).unwrap();
    }
}
