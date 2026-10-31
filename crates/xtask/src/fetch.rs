use crate::workflow::{Error, WorkflowResult, checked};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};

mod moxygen;

/// The one MediaMTX pin, shared with `scripts/dev/bootstrap-runtime.sh`.
const MEDIAMTX_PIN: &str = include_str!("../../../scripts/data/mediamtx.tsv");
const MOQ_DEV_COMMIT: &str = "e2c5b50572cc305afcfffb4f40184200e9965204";
struct Release {
    tag: &'static str,
    archive: &'static str,
    sha: &'static str,
    binary: &'static str,
    target: &'static str,
}
/// The pinned linux/amd64 MediaMTX release.
fn mediamtx_release() -> Result<Release, Error> {
    MEDIAMTX_PIN
        .lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let mut fields = line.split('\t');
            match (fields.next(), fields.next(), fields.next(), fields.next()) {
                (Some(tag), Some("x86_64"), Some(archive), Some(sha)) => Some(Release {
                    tag,
                    archive,
                    sha,
                    binary: "mediamtx",
                    target: "mediamtx",
                }),
                _ => None,
            }
        })
        .ok_or_else(|| "scripts/data/mediamtx.tsv has no x86_64 row".into())
}
const MOQ: [Release; 2] = [
    Release {
        tag: "moq-cli-v0.14.7",
        archive: "moq-cli-v0.14.7-x86_64-unknown-linux-gnu.tar.gz",
        sha: "19b174b98274f6f2e20403469a4472c9d91b91be0a1ae041ceecd5beeb0444ec",
        binary: "moq",
        target: "moq-0.14.7",
    },
    Release {
        tag: "moq-relay-v0.17.7",
        archive: "moq-relay-v0.17.7-x86_64-unknown-linux-gnu.tar.gz",
        sha: "2a644213cc9dec7805658f91725e3cfc3624577bc761f8a4171cc47ddd59062d",
        binary: "moq-relay",
        target: "moq-relay-0.17.7",
    },
];

pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    let (name, args) = args
        .split_first()
        .ok_or_else(|| Error::usage("usage: xtask fetch mediamtx|moq-dev|moxygen"))?;
    match *name {
        "mediamtx" => mediamtx(args),
        "moq-dev" => moq(args),
        "moxygen" => moxygen::run(args),
        other => Err(Error::usage(format!("fetch: unknown command: {other}"))),
    }
}

fn pinned_host(label: &str) -> WorkflowResult {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err(format!(
            "{label}: only linux/amd64 is pinned (host: {}/{})",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
        .into());
    }
    Ok(())
}

fn version(text: &str) -> Option<&str> {
    for (index, _) in text.match_indices('v') {
        let tail = &text[index + 1..];
        if tail.starts_with(|character: char| character.is_ascii_digit()) {
            let len = tail
                .bytes()
                .take_while(|byte| byte.is_ascii_digit() || *byte == b'.')
                .count();
            return Some(&text[index..index + 1 + len]);
        }
    }
    None
}
fn installed_version(target: &Path) -> Result<Option<String>, Error> {
    let result =
        crate::setup::probe_output(Command::new(target).arg("--version").stderr(Stdio::null()))?;
    Ok(result
        .and_then(|result| version(&String::from_utf8_lossy(&result.stdout)).map(str::to_owned)))
}

fn download(url: &str, target: &Path) -> WorkflowResult {
    checked(
        Command::new("curl")
            .args([
                "--fail",
                "--show-error",
                "--silent",
                "--location",
                "--connect-timeout",
                "20",
                "--max-time",
            ])
            .arg(
                std::env::var_os("RESTREAM_BOOTSTRAP_DOWNLOAD_TIMEOUT_SECS")
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "120".into()),
            )
            .args([
                "--retry",
                "3",
                "--retry-delay",
                "2",
                "--retry-all-errors",
                url,
                "-o",
            ])
            .arg(target),
    )
}
fn verify(actual: &str, release: &Release) -> WorkflowResult {
    if actual != release.sha {
        return Err(format!(
            "SHA256 mismatch for {}\n  expected {}\n  actual   {actual}",
            release.archive, release.sha
        )
        .into());
    }
    Ok(())
}

fn extract(
    release: &Release,
    url: &str,
    member: &str,
    temp: &Path,
    target: &Path,
) -> WorkflowResult {
    let archive = temp.join(release.archive);
    download(url, &archive)?;
    verify(&crate::setup::sha256(&archive)?, release)?;
    checked(
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(temp)
            .arg(member),
    )?;
    let source = temp.join(member);
    if !crate::workflow::executable(&source) {
        return Err(format!(
            "{} did not contain an executable {}",
            release.archive, release.binary
        )
        .into());
    }
    fs::create_dir_all(target.parent().ok_or("fetch target has no parent")?)?;
    crate::setup::install_atomic(&source, target)
}

fn mediamtx_flags(args: &[&str]) -> Result<Option<bool>, Error> {
    let mut print_path = false;
    for arg in args {
        match *arg {
            "--print-path" => print_path = true,
            "-h" | "--help" => return Ok(None),
            other => {
                return Err(Error::usage(format!(
                    "fetch-mediamtx: unknown argument: {other}\nusage: xtask fetch mediamtx [--print-path]"
                )));
            }
        }
    }
    Ok(Some(print_path))
}
fn mediamtx(args: &[&str]) -> WorkflowResult {
    let release = mediamtx_release()?;
    let version = release.tag;
    let Some(print_path) = mediamtx_flags(args)? else {
        println!(
            "Usage: xtask fetch mediamtx [--print-path]\nFetch linux/amd64 MediaMTX {version} (scripts/data/mediamtx.tsv), verify its pinned SHA256, and install .local/bin/{}.",
            release.target
        );
        return Ok(());
    };
    pinned_host("fetch-mediamtx")?;
    let target = crate::repo_root().join(".local/bin").join(release.target);
    let current = crate::workflow::executable(&target)
        && installed_version(&target)?.as_deref() == Some(version);
    if !current {
        let temp = crate::workflow::TempDir::new("restream-xtask")?;
        let url = format!(
            "https://github.com/bluenviron/mediamtx/releases/download/{}/{}",
            release.tag, release.archive
        );
        eprintln!("fetch-mediamtx: downloading {url}");
        extract(&release, &url, "mediamtx", temp.path(), &target)?;
        let reported = installed_version(&target)?.unwrap_or_default();
        if reported != version {
            return Err(format!(
                "fetch-mediamtx: installed binary reports '{reported}', expected {version}"
            )
            .into());
        }
    }
    if print_path {
        println!("{}", target.display());
    } else if current {
        println!(
            "fetch-mediamtx: {} already reports {version}",
            target.display()
        );
    } else {
        println!(
            "fetch-mediamtx: installed {} ({version}, sha256 verified)",
            target.display()
        );
    }
    Ok(())
}

fn moq(args: &[&str]) -> WorkflowResult {
    match args.first().copied() {
        None | Some("") => {}
        Some("-h" | "--help") => {
            println!(
                "Usage: xtask fetch moq-dev\nFetch moq-cli-v0.14.7 and moq-relay-v0.17.7 ({MOQ_DEV_COMMIT}), verify pinned archive hashes, and install .local/bin/moq-0.14.7 and .local/bin/moq-relay-0.17.7."
            );
            return Ok(());
        }
        Some(other) => {
            return Err(Error::usage(format!(
                "fetch-moq-dev: unknown argument: {other}"
            )));
        }
    }
    pinned_host("fetch-moq-dev")?;
    let temp = crate::workflow::TempDir::new("restream-xtask")?;
    let bin = crate::repo_root().join(".local/bin");
    fs::create_dir_all(&bin)?;
    for release in MOQ {
        let target = bin.join(release.target);
        let stamp = bin.join(format!("{}.sha256", release.target));
        if crate::workflow::executable(&target)
            && fs::read_to_string(&stamp)
                .is_ok_and(|text| text.trim_end_matches('\n') == release.sha)
        {
            println!(
                "fetch-moq-dev: {} already installed from {}",
                target.display(),
                release.archive
            );
            continue;
        }
        eprintln!(
            "fetch-moq-dev: downloading {}/{}",
            release.tag, release.archive
        );
        let url = format!(
            "https://github.com/moq-dev/moq/releases/download/{}/{}",
            release.tag, release.archive
        );
        let member = format!(
            "{}/bin/{}",
            release.archive.trim_end_matches(".tar.gz"),
            release.binary
        );
        extract(&release, &url, &member, temp.path(), &target)?;
        fs::write(stamp, format!("{}\n", release.sha))?;
        println!(
            "fetch-moq-dev: installed {} (sha256 verified)",
            target.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checksums_reject_wrong_identity() {
        let mediamtx = mediamtx_release().expect("the pin file has an x86_64 row");
        for release in [&mediamtx, &MOQ[0], &MOQ[1]] {
            assert!(verify(release.sha, release).is_ok());
            assert!(verify(&"0".repeat(64), release).is_err());
            assert_eq!(release.sha.len(), 64);
            assert!(release.sha.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }
    #[test]
    fn print_flag_and_reported_version_boundaries() {
        assert_eq!(mediamtx_flags(&[]).unwrap(), Some(false));
        assert_eq!(mediamtx_flags(&["--print-path"]).unwrap(), Some(true));
        assert!(mediamtx_flags(&["--version"]).is_err());
        assert_eq!(version("MediaMTX v1.21.2\n"), Some("v1.21.2"));
        assert_eq!(version("version v1.21.20"), Some("v1.21.20"));
        assert_eq!(version("invalid"), None);
    }
}
