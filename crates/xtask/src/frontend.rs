//! Frontend orchestration; TypeScript, Vite and Playwright remain their canonical owners.
mod e2e;
mod node;

use std::env;
use std::path::PathBuf;

use crate::workflow::{Error, WorkflowResult};

const USAGE: &str =
    "usage: cargo xtask frontend node-tests [--coverage] | e2e [playwright arguments]";

pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    let Some((mode, args)) = args.split_first() else {
        return Err(Error::usage(USAGE));
    };
    if matches!(*mode, "-h" | "--help")
        || (*mode == "node-tests" && matches!(args.first(), Some(&"-h" | &"--help")))
    {
        println!("{USAGE}");
        return Ok(());
    }
    let root = crate::repo_root().canonicalize()?;
    match *mode {
        "node-tests" => node::tests(&root, args),
        "e2e" => e2e::run(&root, args),
        _ => Err(Error::usage(format!(
            "unknown frontend command: {mode}\n{USAGE}"
        ))),
    }
}

fn env_path(name: &str, fallback: impl Into<PathBuf>) -> PathBuf {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback.into())
}
