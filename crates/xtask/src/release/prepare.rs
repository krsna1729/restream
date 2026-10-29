use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

use super::common::require_commands;
use crate::workflow::{Error, WorkflowResult, checked};

const REQUIRED_ASSETS: &[&str] = &[
    "public/index.html",
    "public/login.html",
    "public/output.css",
    "public/js/control-room/main.js",
    "public/js/vendor/react/jsx-runtime.js",
];
const INPUTS: &[&str] = &[
    "web",
    "scripts/dev/frontend",
    "package.json",
    "package-lock.json",
    "tsconfig.json",
    "vite.config.ts",
];
const OUTPUTS: &[&str] = &[
    "public/index.html",
    "public/login.html",
    "public/output.css",
    "public/js",
];

pub(super) fn run(args: &[&str]) -> WorkflowResult {
    if matches!(args.first(), Some(&"-h" | &"--help")) {
        println!(
            "Usage: cargo xtask release prepare-build-tree\n\nInstalls locked frontend dependencies and builds public/ for release/harness embedding.\nRESTREAM_RELEASE_NPM_CI=if-needed|always|never (default: if-needed)\nRESTREAM_RELEASE_FRONTEND_BUILD=if-needed|always|never (default: if-needed)"
        );
        return Ok(());
    }
    if !args.is_empty() {
        return Err(Error::usage(
            "Usage: cargo xtask release prepare-build-tree",
        ));
    }
    prepare_build_tree()
}

pub(crate) fn prepare_build_tree() -> WorkflowResult {
    require_commands(&["npm", "cargo"])?;
    match crate::workflow::env_or("RESTREAM_RELEASE_NPM_CI", "if-needed").as_str() {
        "always" => checked(Command::new("npm").args(["ci", "--include=optional"]))?,
        "if-needed" if !Path::new("node_modules").is_dir() => {
            checked(Command::new("npm").args(["ci", "--include=optional"]))?;
        }
        "if-needed" => println!("prepare-build-tree: reusing existing node_modules"),
        "never" => {}
        value => {
            return Err(Error::usage(format!(
                "prepare-build-tree: invalid RESTREAM_RELEASE_NPM_CI={value}"
            )));
        }
    }
    match crate::workflow::env_or("RESTREAM_RELEASE_FRONTEND_BUILD", "if-needed").as_str() {
        "always" => checked(Command::new("npm").args(["run", "build:frontend"]))?,
        "if-needed" if !frontend_assets_fresh() => {
            checked(Command::new("npm").args(["run", "build:frontend"]))?;
        }
        "if-needed" => println!("prepare-build-tree: reusing generated frontend assets"),
        "never" => {}
        value => {
            return Err(Error::usage(format!(
                "prepare-build-tree: invalid RESTREAM_RELEASE_FRONTEND_BUILD={value}"
            )));
        }
    }
    for asset in REQUIRED_ASSETS {
        if !crate::workflow::nonempty(Path::new(asset)) {
            return Err(format!(
                "prepare-build-tree: required generated asset is missing: {asset}"
            )
            .into());
        }
    }
    println!("prepare-build-tree: release build tree is ready");
    Ok(())
}

fn frontend_assets_fresh() -> bool {
    if !REQUIRED_ASSETS
        .iter()
        .all(|asset| crate::workflow::nonempty(Path::new(asset)))
    {
        return false;
    }
    let mut newest_input = None;
    for path in INPUTS {
        collect_time(Path::new(path), true, &mut newest_input);
    }
    let mut oldest_output = None;
    for path in OUTPUTS {
        collect_time(Path::new(path), false, &mut oldest_output);
    }
    matches!((newest_input, oldest_output), (Some(input), Some(output)) if output >= input)
}

// Like find -type f, do not follow symlinks while determining freshness.
fn collect_time(path: &Path, newest: bool, result: &mut Option<SystemTime>) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            if newest && entry.file_name() == "node_modules" {
                continue;
            }
            collect_time(&entry.path(), newest, result);
        }
    } else if metadata.is_file() {
        let Ok(time) = metadata.modified() else {
            return;
        };
        if result.is_none_or(|previous| {
            if newest {
                time > previous
            } else {
                time < previous
            }
        }) {
            *result = Some(time);
        }
    }
}
