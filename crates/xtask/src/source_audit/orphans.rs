//! Orphaned Rust sources: a `.rs` file no crate root reaches through `mod`
//! declarations (with `#[path]`) or `include!` is never compiled, so its
//! code and tests silently stop existing. The walk is syntactic and
//! cfg-agnostic: a module behind `#[cfg(...)]` still counts as reached.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::scan::{files_under, read_text};

/// Directories never searched for sources or manifests.
const SKIPPED: &[&str] = &["target", "vendor", ".local", "node_modules", ".git"];

/// Per-package root files Cargo discovers without a manifest entry.
const AUTO_ROOTS: &[&str] = &["src/lib.rs", "src/main.rs", "build.rs"];
/// Per-package directories whose direct `.rs` children are roots.
const AUTO_ROOT_DIRS: &[&str] = &["src/bin", "tests", "benches", "examples", "fuzz_targets"];

static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)"(?:\\.|[^"\\])*"|//[^\n]*|/\*.*?\*/|#\[path\s*=\s*"(?P<path>[^"]+)"\]|include!\(\s*"(?P<include>[^"]+)"\s*\)|\bmod\s+(?:r#)?(?P<mod>\w+)\s*(?P<term>[;{])|[{}]"#,
    )
    .expect("static regex")
});
static MANIFEST_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)^\s*path\s*=\s*"([^"]+\.rs)""#).expect("static regex"));

/// Every `.rs` file under the workspace that no crate root reaches.
pub(crate) fn orphans() -> Result<Vec<PathBuf>, String> {
    let sources: BTreeSet<PathBuf> = workspace_files("rs")?;
    let roots = crate_roots()?;
    let reached = reach(&roots)?;
    Ok(sources.difference(&reached).cloned().collect())
}

fn skipped(path: &Path) -> bool {
    path.components()
        .any(|part| SKIPPED.contains(&part.as_os_str().to_str().unwrap_or("")))
}

fn workspace_files(extension: &str) -> Result<BTreeSet<PathBuf>, String> {
    Ok(files_under(&["."])?
        .into_iter()
        .map(|path| normalize(&path))
        .filter(|path| !skipped(path) && path.extension().is_some_and(|ext| ext == extension))
        .collect())
}

fn crate_roots() -> Result<BTreeSet<PathBuf>, String> {
    let mut roots = BTreeSet::new();
    for manifest in files_under(&["."])?
        .into_iter()
        .map(|path| normalize(&path))
        .filter(|path| !skipped(path) && path.file_name().is_some_and(|name| name == "Cargo.toml"))
    {
        let package = manifest.parent().unwrap_or(Path::new("")).to_path_buf();
        for root in AUTO_ROOTS {
            let path = package.join(root);
            if path.is_file() {
                roots.insert(path);
            }
        }
        for dir in AUTO_ROOT_DIRS {
            let dir = package.join(dir);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = normalize(&entry.path());
                if path.extension().is_some_and(|ext| ext == "rs") {
                    roots.insert(path);
                } else if path.join("main.rs").is_file() {
                    roots.insert(path.join("main.rs"));
                }
            }
        }
        for path in MANIFEST_PATH.captures_iter(&read_text(&manifest)?) {
            roots.insert(normalize(&package.join(&path[1])));
        }
    }
    Ok(roots)
}

fn reach(roots: &BTreeSet<PathBuf>) -> Result<BTreeSet<PathBuf>, String> {
    let mut reached = BTreeSet::new();
    let mut pending: Vec<PathBuf> = roots.iter().cloned().collect();
    while let Some(file) = pending.pop() {
        if !file.is_file() || !reached.insert(file.clone()) {
            continue;
        }
        let module_dir = module_dir(&file, roots.contains(&file));
        pending.extend(children(&file, &module_dir, &read_text(&file)?));
    }
    Ok(reached)
}

/// Where `mod child;` in `file` looks: a crate root and `mod.rs` beside
/// themselves, any other `name.rs` in `name/`.
fn module_dir(file: &Path, root: bool) -> PathBuf {
    let base = file.parent().unwrap_or(Path::new(""));
    match file.file_stem().and_then(|stem| stem.to_str()) {
        Some(stem) if !root && stem != "mod" => base.join(stem),
        _ => base.to_path_buf(),
    }
}

/// The files `file`'s `mod` declarations and `include!`s name. Inline
/// `mod name { ... }` blocks nest the lookup directory, as rustc does.
fn children(file: &Path, module_dir: &Path, source: &str) -> Vec<PathBuf> {
    let base = file.parent().unwrap_or(Path::new(""));
    let mut found = Vec::new();
    let mut inline: Vec<(String, usize)> = Vec::new();
    let mut depth = 0usize;
    let mut path_attr: Option<String> = None;
    for token in TOKEN.captures_iter(source) {
        let text = &token[0];
        if text.starts_with('"') || text.starts_with("//") || text.starts_with("/*") {
            continue;
        }
        if let Some(path) = token.name("path") {
            path_attr = Some(path.as_str().to_string());
            continue;
        }
        if let Some(include) = token.name("include") {
            found.push(normalize(&base.join(include.as_str())));
            continue;
        }
        if let (Some(name), Some(term)) = (token.name("mod"), token.name("term")) {
            let dir = inline
                .iter()
                .fold(module_dir.to_path_buf(), |dir, (name, _)| dir.join(name));
            if term.as_str() == ";" {
                match path_attr.take() {
                    // `#[path]` is relative to the file, or to the inline
                    // module's directory when nested in one.
                    Some(path) if inline.is_empty() => found.push(normalize(&base.join(path))),
                    Some(path) => found.push(normalize(&dir.join(path))),
                    None => {
                        let flat = dir.join(format!("{}.rs", name.as_str()));
                        let nested = dir.join(name.as_str()).join("mod.rs");
                        found.push(normalize(if flat.is_file() { &flat } else { &nested }));
                    }
                }
            } else {
                depth = depth.saturating_add(1);
                inline.push((name.as_str().to_string(), depth));
                path_attr = None;
            }
            continue;
        }
        match text {
            "{" => depth = depth.saturating_add(1),
            "}" => {
                if inline.last().is_some_and(|(_, at)| *at == depth) {
                    inline.pop();
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
        path_attr = None;
    }
    found
}

/// `a/./b/../c` as `a/c`, without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(found: Vec<PathBuf>) -> Vec<String> {
        found
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn inline_modules_nest_the_lookup_and_attributes_resolve_relative_to_the_file() {
        let source = r#"
            // mod commented_out;
            const NOTE: &str = "mod in_a_string;";
            #[cfg(test)]
            mod tests {
                mod arguments;
                #[path = "fixtures.rs"]
                mod fixtures;
            }
            #[path = "../shared/util.rs"]
            mod util;
            mod after;
            fn f() { include!("generated/table.rs"); }
        "#;
        let found = children(
            Path::new("crates/x/src/engine.rs"),
            Path::new("crates/x/src/engine"),
            source,
        );
        assert_eq!(
            paths(found),
            [
                "crates/x/src/engine/tests/arguments/mod.rs",
                "crates/x/src/engine/tests/fixtures.rs",
                "crates/x/shared/util.rs",
                "crates/x/src/engine/after/mod.rs",
                "crates/x/src/generated/table.rs",
            ]
        );
    }

    #[test]
    fn a_flat_file_wins_over_mod_rs_and_roots_look_beside_themselves() {
        let dir = std::env::temp_dir().join(format!("xtask-orphans-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).expect("temp dir");
        std::fs::write(dir.join("src/flat.rs"), "").expect("temp file");
        let root = dir.join("src/lib.rs");
        let found = children(&root, &module_dir(&root, true), "mod flat;");
        std::fs::remove_dir_all(&dir).expect("temp cleanup");
        assert_eq!(found, [dir.join("src/flat.rs")]);
        assert_eq!(
            module_dir(Path::new("src/api/mod.rs"), false),
            Path::new("src/api")
        );
        assert_eq!(
            module_dir(Path::new("src/api/router.rs"), false),
            Path::new("src/api/router")
        );
    }
}
