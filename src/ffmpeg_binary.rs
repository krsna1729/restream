//! The FFmpeg executable for subprocess work: external transcodes, subprocess
//! file ingest and recording remux.
//!
//! Resolution order: `FFMPEG_BIN_PATH` (config, else environment), then the
//! copy installed by `restream ffmpeg-fetch`, then `ffmpeg` on `PATH`. The
//! in-process FFmpeg libraries are linked at build time and do not use this.
//! A missing executable never stops the server; only FFmpeg-backed stages
//! fail, and startup logs how to fix it.

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::{info, warn};

static FFMPEG_BIN_PATH: OnceLock<PathBuf> = OnceLock::new();
static CONFIGURED_FFMPEG_PATH: OnceLock<Option<String>> = OnceLock::new();

/// Where `restream ffmpeg-fetch` installs its executable.
const FETCH_DIR: &str = ".restream/runtime/ffmpeg";
const FETCH_RELEASE: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest";

#[derive(Debug, PartialEq)]
enum Resolution {
    Configured(PathBuf),
    Fetched(PathBuf),
    System(PathBuf),
    /// `Some` names a configured path that did not exist.
    Missing(Option<String>),
}

/// Records the configured path from [`crate::AppConfig`]. Call once at
/// startup before any consumer calls [`ffmpeg_bin_path`]; the server then
/// resolves it after logging starts so the outcome is logged.
pub fn init(configured: Option<String>) {
    let _ = CONFIGURED_FFMPEG_PATH.set(configured);
}

/// The resolved FFmpeg executable. Falls back to the bare name `ffmpeg` when
/// nothing was found, so a spawn fails with a clear "not found" error.
pub fn ffmpeg_bin_path() -> &'static Path {
    FFMPEG_BIN_PATH.get_or_init(|| {
        let configured = CONFIGURED_FFMPEG_PATH
            .get_or_init(|| None)
            .clone()
            .or_else(|| std::env::var("FFMPEG_BIN_PATH").ok());
        let resolution = resolve(
            configured,
            &Path::new(FETCH_DIR).join("ffmpeg"),
            std::env::var_os("PATH"),
        );
        match resolution {
            Resolution::Configured(path) => {
                info!(path = %path.display(), "[startup] configured FFmpeg executable");
                path
            }
            Resolution::Fetched(path) => {
                info!(path = %path.display(), "[startup] FFmpeg executable installed by `restream ffmpeg-fetch`");
                path
            }
            Resolution::System(path) => {
                info!(path = %path.display(), "[startup] system FFmpeg executable");
                path
            }
            Resolution::Missing(configured) => {
                warn!(
                    configured = configured.as_deref().unwrap_or(""),
                    "no FFmpeg executable found: external transcodes, subprocess file ingest \
                     and recording remux will fail; install ffmpeg or run `restream ffmpeg-fetch`",
                );
                PathBuf::from("ffmpeg")
            }
        }
    })
}

fn is_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|meta| meta.is_file())
}

fn resolve(configured: Option<String>, fetched: &Path, path_env: Option<OsString>) -> Resolution {
    if let Some(configured) = configured {
        let path = PathBuf::from(&configured);
        if is_file(&path) {
            return Resolution::Configured(path);
        }
        warn!(path = %configured, "configured FFmpeg executable does not exist; falling back");
        return fallback(fetched, path_env, Some(configured));
    }
    fallback(fetched, path_env, None)
}

fn fallback(fetched: &Path, path_env: Option<OsString>, configured: Option<String>) -> Resolution {
    if is_file(fetched) {
        return Resolution::Fetched(fetched.to_path_buf());
    }
    path_env
        .iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join("ffmpeg"))
        .find(|candidate| is_file(candidate))
        .map_or(Resolution::Missing(configured), Resolution::System)
}

fn release_asset() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz"),
        "aarch64" => Some("ffmpeg-n8.1-latest-linuxarm64-gpl-8.1.tar.xz"),
        _ => None,
    }
}

/// Finds `asset`'s SHA-256 in a `sha256sum`-format listing.
fn expected_sha256<'a>(listing: &'a str, asset: &str) -> Option<&'a str> {
    listing.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let digest = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == asset && digest.len() == 64).then_some(digest)
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `restream ffmpeg-fetch`: download the BtbN FFmpeg 8.1 static GPL build
/// for this architecture, verify it against the release's checksum list, and
/// install `bin/ffmpeg` under `.restream/runtime/ffmpeg/`. Optional: a
/// system FFmpeg on `PATH` works without it.
pub fn fetch() -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("ffmpeg-fetch: {error}");
            return 1;
        }
    };
    match runtime.block_on(fetch_into(Path::new(FETCH_DIR))) {
        Ok(path) => {
            let version = std::process::Command::new(&path)
                .arg("-version")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .and_then(|text| text.lines().next().map(str::to_string))
                .unwrap_or_default();
            println!("installed {} ({version})", path.display());
            0
        }
        Err(error) => {
            eprintln!("ffmpeg-fetch: {error}");
            1
        }
    }
}

async fn fetch_into(dir: &Path) -> Result<PathBuf, String> {
    let asset = release_asset()
        .ok_or_else(|| format!("no release build for {}", std::env::consts::ARCH))?;
    let client = reqwest::Client::new();
    let get = |url: String| {
        let client = client.clone();
        async move {
            client
                .get(&url)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|error| format!("GET {url}: {error}"))
        }
    };

    let listing = get(format!("{FETCH_RELEASE}/checksums.sha256"))
        .await?
        .text()
        .await
        .map_err(|error| format!("reading checksums: {error}"))?;
    let expected = expected_sha256(&listing, asset)
        .ok_or_else(|| format!("{asset} is not in the release checksum list"))?
        .to_ascii_lowercase();

    std::fs::create_dir_all(dir).map_err(|error| format!("creating {}: {error}", dir.display()))?;
    let archive = dir.join(format!("{asset}.part"));
    let mut file = std::fs::File::create(&archive)
        .map_err(|error| format!("creating {}: {error}", archive.display()))?;
    let mut response = get(format!("{FETCH_RELEASE}/{asset}")).await?;
    let mut hasher = Sha256::new();
    println!("downloading {asset} ...");
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("downloading {asset}: {error}"))?
    {
        hasher.update(&chunk);
        file.write_all(&chunk)
            .map_err(|error| format!("writing {}: {error}", archive.display()))?;
    }
    drop(file);
    let actual = hex(&hasher.finalize());
    if actual != expected {
        let _ = std::fs::remove_file(&archive);
        return Err(format!(
            "checksum mismatch for {asset}: got {actual}, want {expected}"
        ));
    }

    let staging = dir.join("staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|error| format!("creating {}: {error}", staging.display()))?;
    let extracted = std::process::Command::new("tar")
        .arg("-xJf")
        .arg(&archive)
        .arg("-C")
        .arg(&staging)
        .args(["--strip-components=2", "--wildcards", "*/bin/ffmpeg"])
        .status()
        .map_err(|error| format!("running tar: {error}"))?;
    let _ = std::fs::remove_file(&archive);
    let binary = staging.join("ffmpeg");
    if !extracted.success() || !is_file(&binary) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err("extracting bin/ffmpeg failed (needs GNU tar with xz support)".into());
    }
    let target = dir.join("ffmpeg");
    std::fs::rename(&binary, &target)
        .map_err(|error| format!("installing {}: {error}", target.display()))?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "restream-ffmpeg-resolve-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolution_prefers_configured_then_fetched_then_path() {
        let dir = temp_dir("order");
        let configured = dir.join("configured");
        let fetched = dir.join("fetched");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for file in [&configured, &fetched, &bin.join("ffmpeg")] {
            std::fs::write(file, b"").unwrap();
        }
        let path = Some(bin.clone().into_os_string());
        let configured_text = Some(configured.display().to_string());

        assert_eq!(
            resolve(configured_text, &fetched, path.clone()),
            Resolution::Configured(configured.clone())
        );
        assert_eq!(
            resolve(None, &fetched, path.clone()),
            Resolution::Fetched(fetched.clone())
        );
        std::fs::remove_file(&fetched).unwrap();
        assert_eq!(
            resolve(None, &fetched, path),
            Resolution::System(bin.join("ffmpeg"))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_configured_path_falls_back_and_is_reported_when_nothing_exists() {
        let dir = temp_dir("missing");
        let gone = dir.join("gone").display().to_string();
        assert_eq!(
            resolve(Some(gone.clone()), &dir.join("fetched"), None),
            Resolution::Missing(Some(gone))
        );
        // A directory named ffmpeg on PATH is not an executable.
        std::fs::create_dir_all(dir.join("ffmpeg")).unwrap();
        assert_eq!(
            resolve(
                None,
                &dir.join("fetched"),
                Some(dir.clone().into_os_string())
            ),
            Resolution::Missing(None)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn checksum_lookup_matches_the_exact_asset_name() {
        let listing = format!(
            "{a}  ffmpeg-n8.1-latest-linux64-gpl-shared-8.1.tar.xz\n\
             {b} *ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz\n",
            a = "a".repeat(64),
            b = "b".repeat(64),
        );
        assert_eq!(
            expected_sha256(&listing, "ffmpeg-n8.1-latest-linux64-gpl-8.1.tar.xz"),
            Some("b".repeat(64).as_str())
        );
        assert_eq!(expected_sha256(&listing, "missing.tar.xz"), None);
    }

    proptest::proptest! {
        /// For any listing of unique asset names, the lookup returns exactly
        /// the digest on that asset's line, and nothing for an absent name.
        #[test]
        fn checksum_lookup_returns_only_the_named_assets_digest(
            entries in proptest::collection::btree_map("[a-z0-9.-]{1,40}", "[0-9a-f]{64}", 1..12),
            absent in "[A-Z]{1,8}",
        ) {
            let listing: String = entries
                .iter()
                .map(|(name, digest)| format!("{digest}  {name}\n"))
                .collect();
            for (name, digest) in &entries {
                proptest::prop_assert_eq!(expected_sha256(&listing, name), Some(digest.as_str()));
            }
            proptest::prop_assert_eq!(expected_sha256(&listing, &absent), None);
        }
    }
}
