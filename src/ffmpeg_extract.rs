//! Extract embedded FFmpeg binary to temp directory on startup.
//!
//! If the `FFMPEG_BIN_PATH` environment variable is set before startup the
//! embedded binary is **not** extracted — the provided path is used directly.
//! Set it to a system FFmpeg (e.g. `/usr/bin/ffmpeg`) to skip the temp-dir
//! extraction entirely, keeping RSS baseline low.
//!
//! When the env var is absent the embedded binary (via `rust-embed`) is written
//! to a versioned shared cache under `runtime/ffmpeg/`, made executable,
//! and then reused across processes. Startup is intentionally atomic and
//! multi-process safe so correctness harness modes can boot in parallel.
//!
//! The resolved path is cached in a [`OnceLock`] and served via [`ffmpeg_bin_path`]
//! so the external transcoder and other consumers don't need environment variables.

use crate::api::EmbeddedAssets;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::{info, warn};

static FFMPEG_BIN_PATH: OnceLock<PathBuf> = OnceLock::new();
static CONFIGURED_FFMPEG_PATH: OnceLock<Option<String>> = OnceLock::new();

/// Initialise the FFmpeg binary path from [`crate::AppConfig`].
///
/// Must be called once at startup, before any consumer calls
/// [`ensure_ffmpeg_extracted`] or [`ffmpeg_bin_path`].
pub fn init(configured: Option<String>) {
    let _ = CONFIGURED_FFMPEG_PATH.set(configured);
    let _ = FFMPEG_BIN_PATH.get_or_init(resolve_ffmpeg_bin_path);
}

fn select_configured_ffmpeg_path(
    configured: Option<&Option<String>>,
    env_value: Option<String>,
) -> Option<String> {
    configured
        .and_then(|opt| opt.as_ref().cloned())
        .or(env_value)
}

fn configured_ffmpeg_path() -> Option<String> {
    select_configured_ffmpeg_path(
        CONFIGURED_FFMPEG_PATH.get(),
        std::env::var("FFMPEG_BIN_PATH").ok(),
    )
}

fn resolve_ffmpeg_bin_path() -> PathBuf {
    if let Some(user_path) = configured_ffmpeg_path() {
        let path = PathBuf::from(&user_path);
        if path.exists() && path.is_file() {
            info!("[startup] configured FFmpeg binary: {}", path.display());
            path
        } else {
            warn!(path = %user_path, "configured FFmpeg binary does not exist; using embedded FFmpeg");
            extract_embedded()
        }
    } else {
        extract_embedded()
    }
}

/// Return the resolved FFmpeg binary path, initialising it via [`init`]
/// on first call if [`init`] was not called explicitly.
///
/// Prefer calling [`init`] with the configured path at startup so the
/// `OnceLock` is seeded before any consumer runs.
pub fn ensure_ffmpeg_extracted() -> &'static Path {
    FFMPEG_BIN_PATH
        .get_or_init(|| {
            let _ = CONFIGURED_FFMPEG_PATH.set(None);
            resolve_ffmpeg_bin_path()
        })
        .as_path()
}

fn embedded_cache_root() -> PathBuf {
    PathBuf::from("runtime").join("ffmpeg")
}

fn embedded_cache_key(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut key = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut key, "{byte:02x}");
    }
    key
}

fn embedded_cache_dir(bytes: &[u8]) -> PathBuf {
    embedded_cache_root().join(embedded_cache_key(bytes))
}

fn set_executable(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn cached_ffmpeg_is_valid(path: &Path, expected_bytes: &[u8]) -> Result<bool, std::io::Error> {
    let metadata = std::fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if !file_type.is_file() || file_type.is_symlink() {
        return Ok(false);
    }

    let actual = std::fs::read(path)?;
    Ok(Sha256::digest(&actual) == Sha256::digest(expected_bytes))
}

fn remove_invalid_cache_entry(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path)
        }
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn write_embedded_ffmpeg_cache(
    temp_dir: &Path,
    ffmpeg_path: &Path,
    ffmpeg_bytes: &[u8],
) -> Result<(), String> {
    let temp_path = temp_dir.join(format!(
        "ffmpeg.tmp.{}.{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::write(&temp_path, ffmpeg_bytes)
        .map_err(|error| format!("Failed to write extracted FFmpeg binary: {error}"))?;
    if let Err(error) = set_executable(&temp_path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(format!("Failed to make FFmpeg executable: {error}"));
    }

    match std::fs::rename(&temp_path, ffmpeg_path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(format!(
                "Failed to install extracted FFmpeg binary: {error}"
            ))
        }
    }
}

/// Extract the embedded FFmpeg binary into a versioned cache directory.
///
/// This is intentionally multi-process safe:
/// - a content-hash directory avoids cross-version clobbering
/// - we never remove the shared cache root at startup
/// - installation uses a unique temp file + atomic rename
/// - existing cache entries are reused only after type and digest validation
fn extract_embedded() -> PathBuf {
    let ffmpeg_data = EmbeddedAssets::get("bin/ffmpeg")
        .expect("Embedded FFmpeg binary not found in public/bin/ffmpeg");
    let ffmpeg_bytes = ffmpeg_data.data.as_ref();
    let temp_dir = embedded_cache_dir(ffmpeg_bytes);
    std::fs::create_dir_all(&temp_dir).expect("Failed to create temp ffmpeg directory");

    let ffmpeg_path = temp_dir.join("ffmpeg");
    match cached_ffmpeg_is_valid(&ffmpeg_path, ffmpeg_bytes) {
        Ok(true) => {
            set_executable(&ffmpeg_path).expect("Failed to make cached FFmpeg executable");
            return ffmpeg_path;
        }
        Ok(false) => {
            remove_invalid_cache_entry(&ffmpeg_path)
                .expect("Failed to remove invalid cached FFmpeg binary");
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            panic!("Failed to validate cached FFmpeg binary: {error}");
        }
    }

    match write_embedded_ffmpeg_cache(&temp_dir, &ffmpeg_path, ffmpeg_bytes) {
        Ok(()) => {}
        Err(error) if ffmpeg_path.exists() => {
            if cached_ffmpeg_is_valid(&ffmpeg_path, ffmpeg_bytes).unwrap_or(false) {
                set_executable(&ffmpeg_path).expect("Failed to make cached FFmpeg executable");
            } else {
                panic!("{error}");
            }
            warn!(
                path = %ffmpeg_path.display(),
                err = %error,
                "another process finished embedded ffmpeg install first; reusing cached binary"
            );
        }
        Err(error) => {
            panic!("{error}");
        }
    }

    info!(
        "[startup] Extracted embedded FFmpeg to {}",
        ffmpeg_path.display()
    );

    ffmpeg_path
}

/// Return the resolved FFmpeg binary path.
///
pub fn ffmpeg_bin_path() -> &'static Path {
    ensure_ffmpeg_extracted()
}

/// Remove the FFmpeg cache on shutdown.
///
/// This is intentionally a no-op for embedded binaries since the shared cache
/// may be in use by concurrent processes. User-supplied `FFMPEG_BIN_PATH`
/// values are external and likewise left untouched.
pub fn cleanup_ffmpeg() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "restream-ffmpeg-cache-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn embedded_cache_dir_is_stable_for_same_bytes() {
        let bytes = b"same-binary";
        assert_eq!(embedded_cache_dir(bytes), embedded_cache_dir(bytes));
    }

    #[test]
    fn embedded_cache_dir_changes_when_bytes_change() {
        assert_ne!(
            embedded_cache_dir(b"binary-a"),
            embedded_cache_dir(b"binary-b")
        );
    }

    #[test]
    fn embedded_cache_uses_the_owned_runtime_directory() {
        assert_eq!(embedded_cache_root(), PathBuf::from("runtime/ffmpeg"));
    }

    #[test]
    fn embedded_cache_key_uses_full_sha256_digest() {
        let key = embedded_cache_key(b"same-binary");

        assert_eq!(key.len(), 64);
        assert_eq!(
            key,
            "a1f82722bc8e33aa9100d16001377c07366a779a2c42bc58fdeba9cf8fa9f1fd"
        );
    }

    #[test]
    fn configured_ffmpeg_path_falls_back_to_env_for_direct_unit_consumers() {
        let configured = None;
        assert_eq!(
            select_configured_ffmpeg_path(Some(&configured), Some("/usr/bin/ffmpeg".to_string())),
            Some("/usr/bin/ffmpeg".to_string())
        );

        let configured = Some("/custom/ffmpeg".to_string());
        assert_eq!(
            select_configured_ffmpeg_path(Some(&configured), Some("/usr/bin/ffmpeg".to_string())),
            Some("/custom/ffmpeg".to_string())
        );
    }

    #[test]
    fn cached_ffmpeg_validation_requires_matching_digest() {
        let dir = temp_dir("digest");
        let path = dir.join("ffmpeg");
        std::fs::write(&path, b"wrong").unwrap();

        assert!(!cached_ffmpeg_is_valid(&path, b"expected").unwrap());
        std::fs::write(&path, b"expected").unwrap();
        assert!(cached_ffmpeg_is_valid(&path, b"expected").unwrap());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn cached_ffmpeg_validation_rejects_symlink() {
        let dir = temp_dir("symlink");
        let target = dir.join("target");
        let link = dir.join("ffmpeg");
        std::fs::write(&target, b"expected").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(!cached_ffmpeg_is_valid(&link, b"expected").unwrap());

        let _ = std::fs::remove_dir_all(dir);
    }
}
