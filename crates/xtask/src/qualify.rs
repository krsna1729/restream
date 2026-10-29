//! Release qualification workflows; packaging policy remains in `release`.
use crate::workflow::{Error, WorkflowResult, checked, output};
use std::process::Command;

mod artifact;
mod evidence;
pub(crate) mod shards;
mod support;

pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    match args.split_first() {
        Some((&"local-due-diligence", rest)) => local_due_diligence(rest),
        Some((&"release-artifact-smoke", rest)) => artifact::run(rest),
        Some((&"release-evidence", rest)) => evidence::run(rest),
        Some((&"harness-shard", rest)) => shards::harness(rest),
        Some((&"dispatch-dry-run", rest)) => dispatch(rest),
        _ => Err(Error::usage(
            "usage: cargo xtask qualify <local-due-diligence|release-artifact-smoke|release-evidence|harness-shard|dispatch-dry-run> [args]",
        )),
    }
}

const LOCAL_USAGE: &str = "usage: cargo xtask qualify local-due-diligence <version> [--skip-harness]\nRuns formatting, frontend/API/hygiene/fixture gates, live harness (unless skipped), packaging, security evidence and artifact/container smoke. Requires cargo-audit, cargo-deny, grype, trivy, and Docker.";

fn local_options<'a>(args: &'a [&str]) -> Result<Option<(&'a str, bool)>, Error> {
    let mut version = "";
    let mut skip = false;
    for arg in args {
        match *arg {
            "--skip-harness" => skip = true,
            "-h" | "--help" => return Ok(None),
            option if option.starts_with('-') => {
                return Err(Error::usage(format!(
                    "local-due-diligence: unknown option: {option}\n{LOCAL_USAGE}"
                )));
            }
            value => {
                if !version.is_empty() {
                    return Err(Error::usage(format!(
                        "local-due-diligence: version already supplied: {version}\n{LOCAL_USAGE}"
                    )));
                }
                version = value;
            }
        }
    }
    if version.is_empty() {
        return Err(Error::usage(LOCAL_USAGE));
    }
    Ok(Some((version, skip)))
}

fn local_due_diligence(args: &[&str]) -> WorkflowResult {
    let Some((version, skip)) = local_options(args)? else {
        println!("{LOCAL_USAGE}");
        return Ok(());
    };
    crate::release::require_version("local-due-diligence", version)?;
    let sha = output(Command::new("git").args(["rev-parse", "--short=12", "HEAD"]))?;
    let sha = String::from_utf8_lossy(&sha.stdout).trim().to_owned();
    let run_id = format!("local-release-{version}-{sha}");
    let arch = match std::env::var("RESTREAM_RELEASE_ARCH") {
        Ok(value) if !value.is_empty() => value,
        _ => {
            let machine = output(Command::new("uname").arg("-m"))?;
            format!("linux-{}", String::from_utf8_lossy(&machine.stdout).trim())
        }
    };
    let dist = crate::workflow::env_or("RESTREAM_RELEASE_DIR", "dist");
    let bundle = format!("{dist}/restream-{version}-{arch}.tar.gz");
    let archive = format!("{dist}/restream-{version}-oci.tar.gz");
    let sbom = format!("{dist}/restream-{version}-{arch}.sbom.cdx.json");
    checked(Command::new("cargo").args(["fmt", "--all", "--check"]))?;
    crate::release::prepare_build_tree()?;
    checked(Command::new("npm").args(["run", "test:frontend"]))?;
    for gate in ["api-contract", "test-hygiene", "fixture-discipline"] {
        checked(support::xtask()?.arg(gate))?;
    }
    if skip {
        println!("local-due-diligence: skipping full live harness by request");
    } else {
        checked(
            support::xtask()?
                .args([
                    "harness",
                    "suite",
                    "--",
                    "--run-id",
                    &run_id,
                    "--continue-on-fail",
                ])
                .env(
                    "BENCH_BUILD",
                    crate::workflow::env_or("BENCH_BUILD", "if-needed"),
                ),
        )?;
    }
    crate::release::package_binaries(version)?;
    evidence::run(&[&archive, &sbom, &bundle])?;
    let sha = output(Command::new("git").args(["rev-parse", "HEAD"]))?;
    println!(
        "local-due-diligence: PASS version={version} sha={}",
        String::from_utf8_lossy(&sha.stdout).trim()
    );
    Ok(())
}

fn dispatch(args: &[&str]) -> WorkflowResult {
    const USAGE: &str = "usage: cargo xtask qualify dispatch-dry-run [ref]\nDispatches the non-publishing Release workflow for the supplied ref or current branch.";
    if matches!(args.first(), Some(&"-h" | &"--help")) {
        println!("{USAGE}");
        return Ok(());
    }
    if args.len() > 1 {
        return Err(Error::usage(USAGE));
    }
    let reference =
        crate::release::ref_or_current_branch(args.first().copied().filter(|s| !s.is_empty()))?;
    for tool in ["gh", "jq"] {
        support::require_command(tool, "dispatch-dry-run")?;
    }
    checked(Command::new("gh").args(["workflow", "run", "release.yml", "--ref", &reference]))?;
    // GNU sleep accepts fractional durations and unit suffixes; retain that CLI.
    checked(Command::new("sleep").arg(crate::workflow::env_or(
        "RESTREAM_RELEASE_DISPATCH_SETTLE_SECS",
        "3",
    )))?;
    let result = output(Command::new("gh").args([
        "run",
        "list",
        "--workflow",
        "release.yml",
        "--branch",
        &reference,
        "--limit",
        "1",
        "--json",
        "databaseId,status,conclusion,headSha,createdAt,url",
    ]))?;
    support::with_input(Command::new("jq").arg(".[0]"), &result.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_selection_requires_one_version_and_only_supported_switches() {
        assert!(local_options(&[]).is_err());
        assert!(local_options(&["v1", "v2"]).is_err());
        assert!(local_options(&["v1", "--skip-evidence"]).is_err());
        assert_eq!(
            local_options(&["--skip-harness", "v1"]).unwrap(),
            Some(("v1", true))
        );
        assert_eq!(local_options(&["v1"]).unwrap(), Some(("v1", false)));
        assert_eq!(local_options(&["--help"]).unwrap(), None);
    }
}
