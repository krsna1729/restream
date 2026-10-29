//! Release build, packaging, and gated publication workflows.
mod common;
mod native;
mod package;
mod prepare;
mod publish;

pub(crate) use common::{ref_or_current_branch, require_version};
pub(crate) use native::app_native_without_sbom;
pub(crate) use package::package_binaries;
pub(crate) use prepare::prepare_build_tree;

use crate::workflow::{Error, WorkflowResult};

pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    let Some((command, args)) = args.split_first() else {
        return Err(Error::usage(usage()));
    };
    match *command {
        "prepare-build-tree" => prepare::run(args),
        "app-native" => native::run(args),
        "emit-sbom" => native::emit_sbom_args(args),
        "package-binaries" => package::package_binaries(version_arg(command, args)?),
        "package-runtime-image" => package::package_runtime_image(version_arg(command, args)?),
        "publish-image" => publish::publish_image(version_arg(command, args)?),
        "tag-and-publish" => publish::tag_and_publish(args),
        "checksums" => package::checksums(args),
        "shards" => crate::qualify::shards::run(args),
        "-h" | "--help" | "help" => {
            println!("{}", usage());
            Ok(())
        }
        _ => Err(Error::usage(format!(
            "unknown release command: {command}\n{}",
            usage()
        ))),
    }
}

fn version_arg<'a>(command: &str, args: &'a [&str]) -> Result<&'a str, Error> {
    // Original package/publish scripts consumed the first argument only.
    args.first()
        .copied()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::usage(format!("usage: cargo xtask release {command} <version>")))
}

fn usage() -> &'static str {
    "Usage: cargo xtask release <prepare-build-tree|app-native|emit-sbom|package-binaries|package-runtime-image|tag-and-publish|publish-image|checksums|shards> [args]"
}
