use super::{DOCS_AUDIT, FMT, SOURCE_AUDIT, run_step};
use crate::workflow::{self, WorkflowResult};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

pub(super) fn run() -> WorkflowResult {
    let diff = workflow::output(Command::new("git").args([
        "diff",
        "--cached",
        "--no-renames",
        "--name-status",
        "-z",
    ]))?;
    let paths = String::from_utf8_lossy(&diff.stdout);
    let mut scope = Scope::default();
    let mut fields = paths.split_terminator('\0');
    while let Some(status) = fields.next() {
        let path = fields.next().ok_or("git diff omitted a path")?;
        scope.add(path, status == "D");
    }
    if scope.rust {
        run_step(&FMT)?;
    }
    if scope.docs {
        run_step(&DOCS_AUDIT)?;
    }
    if scope.source {
        run_step(&SOURCE_AUDIT)?;
    }
    if scope.shared {
        eprintln!(
            "precommit: shared Rust inputs changed; run `cargo xtask clippy-workspace` before opening or updating the PR"
        );
    }
    for package in &scope.packages {
        clippy(package, &["--all-targets"])?;
    }
    for (package, name) in scope.tests {
        if !scope.packages.contains(package) {
            clippy(package, &["--test", name])?;
        }
    }
    Ok(())
}

#[derive(Default)]
struct Scope<'a> {
    packages: BTreeSet<&'a str>,
    tests: BTreeSet<(&'a str, &'a str)>,
    rust: bool,
    docs: bool,
    source: bool,
    shared: bool,
}

impl<'a> Scope<'a> {
    fn add(&mut self, path: &'a str, deleted: bool) {
        let outside_workspace = path.starts_with("vendor/") || path.starts_with("fuzz/");
        let shared = outside_workspace
            || path.starts_with(".cargo/")
            || Path::new(path).file_name().is_some_and(|name| {
                matches!(
                    name.to_str(),
                    Some(
                        "Cargo.toml"
                            | "Cargo.lock"
                            | "clippy.toml"
                            | ".clippy.toml"
                            | "rustfmt.toml"
                            | ".rustfmt.toml"
                            | "rust-toolchain"
                            | "rust-toolchain.toml"
                    )
                )
            });
        self.shared |= shared;
        self.docs |= deleted || path.ends_with(".md") || path.starts_with("docs/");
        self.source |= path.ends_with(".rs") || path.starts_with("web/ts/") || shared;
        self.rust |= shared && !outside_workspace;
        if !path.ends_with(".rs") || outside_workspace {
            return;
        }
        self.rust = true;
        let (package, relative) = if let Some((package, relative)) = path
            .strip_prefix("crates/")
            .and_then(|rest| rest.split_once('/'))
        {
            if deleted
                && !Path::new("crates")
                    .join(package)
                    .join("Cargo.toml")
                    .is_file()
            {
                // An entirely removed member can no longer be selected by Cargo.
                self.shared = true;
                return;
            }
            (package, relative)
        } else {
            ("restream", path)
        };
        if let Some(name) = relative
            .strip_prefix("tests/")
            .and_then(|test| test.strip_suffix(".rs"))
            .filter(|name| !deleted && !name.contains('/'))
        {
            self.tests.insert((package, name));
        } else {
            self.packages.insert(package);
        }
    }
}

fn clippy(package: &str, scope: &[&str]) -> WorkflowResult {
    eprintln!(
        "==> cargo clippy -p {package} {} -- -D warnings",
        scope.join(" ")
    );
    workflow::foreground(
        Command::new("cargo")
            .args(["clippy", "-p", package])
            .args(scope)
            .args(["--", "-D", "warnings"]),
    )
}
