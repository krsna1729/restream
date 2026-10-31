//! Frontend orchestration; TypeScript, Vite and Playwright remain their canonical owners.
//! Unit tests need no orchestration: `npm run test:frontend` runs them directly.
mod e2e;

use std::env;
use std::path::PathBuf;

use crate::workflow::{Error, WorkflowResult};

const USAGE: &str = "usage: cargo xtask frontend e2e [playwright arguments]";

pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    let Some((mode, args)) = args.split_first() else {
        return Err(Error::usage(USAGE));
    };
    if matches!(*mode, "-h" | "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let root = crate::repo_root().canonicalize()?;
    match *mode {
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
