//! The FFmpeg and ffprobe executables. The engine links no codec library: all
//! media decoding and encoding (transcodes, file ingest, recording remux,
//! posters, media analysis, picture and sound checks) runs in these processes.
//!
//! Resolution order: `FFMPEG_BIN_PATH` (config, else environment), then the
//! copy installed by `restream ffmpeg-fetch`, then `ffmpeg` on `PATH`. A missing executable never stops the server; only FFmpeg-backed stages
//! fail, and startup logs how to fix it.

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::{info, warn};

static FFMPEG_BIN_PATH: OnceLock<PathBuf> = OnceLock::new();
static CONFIGURED_FFMPEG_PATH: OnceLock<Option<String>> = OnceLock::new();

/// Where `restream ffmpeg-fetch` installs its executable.
const FETCH_DIR: &str = ".restream/runtime/ffmpeg";
/// The pinned BtbN FFmpeg-Builds release: a month's last build, which BtbN
/// keeps for two years (daily builds go after 14 days). The Dockerfile's
/// `ffmpeg-fetch` stage installs the same assets; a test keeps the two equal.
pub const PINNED_RELEASE: &str = "autobuild-2026-09-30-13-08";
const RELEASES: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download";

/// One BtbN static GPL archive and its SHA-256.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: &'static str,
    pub sha256: &'static str,
}

/// The pinned archive for an architecture.
pub fn pinned_asset(arch: &str) -> Option<Asset> {
    match arch {
        "x86_64" => Some(Asset {
            name: "ffmpeg-n9.0.2-17-g2a571b6068-linux64-gpl-9.0.tar.xz",
            sha256: "68ee646831adaae2495618346f3bba94ff207ff83bbd34d643e7004730d66269",
        }),
        "aarch64" => Some(Asset {
            name: "ffmpeg-n9.0.2-17-g2a571b6068-linuxarm64-gpl-9.0.tar.xz",
            sha256: "91afcd7695d4bbbb9e0057aa10baaf9875ec4e1a97303f9059ec971336a2ab46",
        }),
        _ => None,
    }
}

/// Which build `restream ffmpeg-fetch` installs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// The pinned release, verified against the digest in this source.
    Pinned,
    /// BtbN's floating `latest` 9.0 build, verified against that release's
    /// own checksum list (`--latest`).
    Latest,
}

#[derive(Debug, PartialEq)]
enum Resolution {
    Configured(PathBuf),
    Fetched(PathBuf),
    System(PathBuf),
    /// `Some` names a configured path that did not exist.
    Missing(Option<String>),
}

/// Records the configured path (`FFMPEG_BIN_PATH`). Call once at
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

/// The resolved `ffprobe`: `FFPROBE_BIN_PATH`, else the one beside the
/// resolved FFmpeg (the fetched release and most packages ship both), else
/// the first on `PATH`, else the bare name (a spawn then fails clearly).
pub fn ffprobe_bin_path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        if let Some(configured) = std::env::var_os("FFPROBE_BIN_PATH").map(PathBuf::from)
            && is_file(&configured)
        {
            return configured;
        }
        let beside = ffmpeg_bin_path().with_file_name("ffprobe");
        if is_file(&beside) {
            return beside;
        }
        std::env::var_os("PATH")
            .iter()
            .flat_map(std::env::split_paths)
            .map(|dir| dir.join("ffprobe"))
            .find(|candidate| is_file(candidate))
            .unwrap_or_else(|| PathBuf::from("ffprobe"))
    })
}

/// Runs an FFmpeg tool to completion and returns its stdout. It is killed
/// past `within` (and if this future is dropped); a non-zero exit is an
/// error carrying the end of its stderr.
pub async fn run_tool(
    program: &Path,
    args: &[String],
    within: std::time::Duration,
) -> Result<Vec<u8>, String> {
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("{}: {error}", program.display()))?;
    let output = tokio::time::timeout(within, child.wait_with_output())
        .await
        .map_err(|_| format!("{} took longer than {within:?}", program.display()))?
        .map_err(|error| format!("{}: {error}", program.display()))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr
            .trim()
            .chars()
            .rev()
            .take(400)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Err(format!(
            "{} failed ({}): {tail}",
            program.display(),
            output.status
        ))
    }
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

fn latest_asset_name(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz"),
        "aarch64" => Some("ffmpeg-n9.0-latest-linuxarm64-gpl-9.0.tar.xz"),
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

/// The download client. Besides the bundled web roots it trusts the PEM
/// bundle in `SSL_CERT_FILE`, if set: hosts behind a TLS-inspecting proxy
/// name its CA there.
fn http_client() -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder();
    if let Some(path) = std::env::var_os("SSL_CERT_FILE") {
        let pem = std::fs::read(&path).map_err(|error| {
            format!(
                "reading SSL_CERT_FILE {}: {error}",
                Path::new(&path).display()
            )
        })?;
        let certificates = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|error| format!("SSL_CERT_FILE is not a PEM bundle: {error}"))?;
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder
        .build()
        .map_err(|error| format!("building the HTTP client: {}", with_causes(&error)))
}

/// An error and its sources, joined: reqwest's own message omits the cause
/// (DNS, TLS, proxy) an operator needs.
fn with_causes(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `restream ffmpeg-fetch [--latest]`: download the BtbN FFmpeg 9.0 static
/// GPL build for this architecture (the pinned release, or `latest`), verify
/// its SHA-256, and install `ffmpeg` and `ffprobe` under
/// `.restream/runtime/ffmpeg/`. Progress and errors go to `out`. Optional: a
/// system FFmpeg on `PATH` works without it.
pub fn fetch(channel: Channel, out: &mut impl Write) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = writeln!(out, "ffmpeg-fetch: {error}");
            return 1;
        }
    };
    let which = match channel {
        Channel::Pinned => PINNED_RELEASE,
        Channel::Latest => "latest",
    };
    let _ = writeln!(out, "downloading the BtbN FFmpeg 9.0 build ({which}) ...");
    match runtime.block_on(fetch_into(channel, Path::new(FETCH_DIR))) {
        Ok(path) => {
            let version = std::process::Command::new(&path)
                .arg("-version")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .and_then(|text| text.lines().next().map(str::to_string))
                .unwrap_or_default();
            let _ = writeln!(out, "installed {} ({version})", path.display());
            0
        }
        Err(error) => {
            let _ = writeln!(out, "ffmpeg-fetch: {error}");
            1
        }
    }
}

async fn fetch_into(channel: Channel, dir: &Path) -> Result<PathBuf, String> {
    let arch = std::env::consts::ARCH;
    let unsupported = || format!("no release build for {arch}");
    let client = http_client()?;
    let get = |url: String| {
        let client = client.clone();
        async move {
            client
                .get(&url)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|error| format!("GET {url}: {}", with_causes(&error)))
        }
    };

    let (base, asset, expected) = match channel {
        Channel::Pinned => {
            let asset = pinned_asset(arch).ok_or_else(unsupported)?;
            (
                format!("{RELEASES}/{PINNED_RELEASE}"),
                asset.name,
                asset.sha256.to_string(),
            )
        }
        Channel::Latest => {
            let asset = latest_asset_name(arch).ok_or_else(unsupported)?;
            let base = format!("{RELEASES}/latest");
            let listing = get(format!("{base}/checksums.sha256"))
                .await?
                .text()
                .await
                .map_err(|error| format!("reading checksums: {error}"))?;
            let expected = expected_sha256(&listing, asset)
                .ok_or_else(|| format!("{asset} is not in the release checksum list"))?
                .to_ascii_lowercase();
            (base, asset, expected)
        }
    };

    std::fs::create_dir_all(dir).map_err(|error| format!("creating {}: {error}", dir.display()))?;
    let archive = dir.join(format!("{asset}.part"));
    let mut response = get(format!("{base}/{asset}")).await?;
    let mut file = std::fs::File::create(&archive)
        .map_err(|error| format!("creating {}: {error}", archive.display()))?;
    let mut hasher = Sha256::new();
    let downloaded: Result<(), String> = async {
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("downloading {asset}: {}", with_causes(&error)))?
        {
            hasher.update(&chunk);
            file.write_all(&chunk)
                .map_err(|error| format!("writing {}: {error}", archive.display()))?;
        }
        Ok(())
    }
    .await;
    drop(file);
    if let Err(error) = downloaded {
        let _ = std::fs::remove_file(&archive);
        return Err(error);
    }
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
        .args([
            "--strip-components=2",
            "--wildcards",
            "*/bin/ffmpeg",
            "*/bin/ffprobe",
        ])
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
    // ffprobe reads stored media for the library (codec, frame rate, GOP).
    let probe = staging.join("ffprobe");
    if is_file(&probe) {
        let probe_target = dir.join("ffprobe");
        std::fs::rename(&probe, &probe_target)
            .map_err(|error| format!("installing {}: {error}", probe_target.display()))?;
    }
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
            "{a}  ffmpeg-n9.0-latest-linux64-gpl-shared-9.0.tar.xz\n\
             {b} *ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz\n",
            a = "a".repeat(64),
            b = "b".repeat(64),
        );
        assert_eq!(
            expected_sha256(&listing, "ffmpeg-n9.0-latest-linux64-gpl-9.0.tar.xz"),
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
