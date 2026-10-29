//! Optional developer worktree setup. Cache state is copied, never shared.
mod cache;
mod frontend;
#[cfg(test)]
mod tests;

use crate::workflow::{Error, WorkflowResult, checked, output, spawn, wait};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HELP: &str = "Usage: cargo xtask worktree [options] <id>

Create, refresh, or clean up an agent worktree with warm caches.
Defaults: <repo-common-root>/.local/worktrees/<id>, branch codex/<id>, base HEAD,
source current worktree; copied pruned debug target, .cargo and node_modules.
Incomplete frontend dependencies are repaired with locked npm ci.

  --cleanup                 remove the checkout, keeping its branch
  --force-cleanup           pass -f to git worktree remove (locked trees may refuse)
  --path <path>             override destination (relative to current directory)
  --branch <name>           override branch
  --base <rev>              base revision for a new branch
  --source <path>           source cache tree (relative to repository root)
  --full-target-cache       copy the full target tree
  --with-incremental        include a pruned debug incremental cache slice
  --no-target-cache         skip target seeding
  --no-cargo-config         skip .cargo seeding
  --no-node-modules         skip node_modules seeding and repair
  --check-frontend-deps <path>  check frontend dependencies and exit
  --dry-run                 print mutations without executing them
  -h, --help                show help

Default subset: debug/deps, debug/build, debug/.fingerprint and root binaries.
Incremental state is omitted unless requested. Caches belong to the destination.
Cleanup delegates dirty/locked protections to Git and never kills processes.";

#[derive(Debug)]
struct Options {
    id: String,
    path: Option<PathBuf>,
    branch: Option<String>,
    base: String,
    source: Option<PathBuf>,
    dry: bool,
    cleanup: bool,
    force: bool,
    target: bool,
    cargo: bool,
    node: bool,
    full: bool,
    incremental: bool,
    frontend_check: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            id: String::new(),
            path: None,
            branch: None,
            base: "HEAD".into(),
            source: None,
            dry: false,
            cleanup: false,
            force: false,
            target: true,
            cargo: true,
            node: true,
            full: false,
            incremental: false,
            frontend_check: None,
        }
    }
}

fn parse(args: &[&str]) -> Result<Option<Options>, Error> {
    let mut options = Options::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index];
        match arg {
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(None);
            }
            "--" => break, // The original helper stops parsing and ignores the tail.
            "--cleanup" => options.cleanup = true,
            "--force-cleanup" => options.force = true,
            "--dry-run" => options.dry = true,
            "--full-target-cache" => options.full = true,
            "--with-incremental" => options.incremental = true,
            "--no-target-cache" => options.target = false,
            "--no-cargo-config" => options.cargo = false,
            "--no-node-modules" => options.node = false,
            "--path" | "--branch" | "--base" | "--source" | "--check-frontend-deps" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| Error::from(format!("{arg} requires a value")))?;
                match arg {
                    "--path" => options.path = nonempty_path(value),
                    "--branch" => options.branch = (!value.is_empty()).then(|| (*value).into()),
                    "--base" => options.base = (*value).into(),
                    "--source" => options.source = Some(PathBuf::from(value)),
                    _ => options.frontend_check = nonempty_path(value).map(crate::user_path),
                }
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown option: {value}").into());
            }
            value => {
                if !options.id.is_empty() {
                    return Err(format!("unexpected extra argument: {value}").into());
                }
                options.id = value.into();
            }
        }
        index += 1;
    }
    if options.frontend_check.is_none() && options.id.is_empty() {
        return Err(Error::usage(HELP));
    }
    Ok(Some(options))
}

fn nonempty_path(value: &str) -> Option<PathBuf> {
    (!value.is_empty()).then(|| PathBuf::from(value))
}

pub(crate) fn run(args: &[&str]) -> WorkflowResult {
    let Some(options) = parse(args)? else {
        return Ok(());
    };
    if let Some(path) = &options.frontend_check {
        return if frontend::ready(path) {
            Ok(())
        } else {
            Err(Error::exit(1, "frontend dependencies are incomplete"))
        };
    }
    let root = crate::repo_root();
    let source = options.source.as_deref().unwrap_or(&root);
    let source = if source.is_absolute() {
        source.to_path_buf()
    } else {
        root.join(source)
    };
    let source = fs::canonicalize(source)?;
    let common = common_repo_root(&source)?;
    let destination = options
        .path
        .clone()
        .unwrap_or_else(|| common.join(".local/worktrees").join(&options.id));
    let destination = crate::user_path(destination);
    let branch = options
        .branch
        .clone()
        .unwrap_or_else(|| format!("codex/{}", options.id));
    let actions = Actions { dry: options.dry };
    info(format!("source tree: {}", source.display()));
    info(format!("common repo root: {}", common.display()));
    info(format!("worktree id: {}", options.id));
    info(format!("destination: {}", destination.display()));
    info(format!("branch: {branch}"));
    if options.cleanup {
        cleanup(&actions, &source, &destination, options.force)?;
        info(format!(
            "cleanup complete\n  worktree: {}\n  branch kept: {branch}",
            destination.display()
        ));
        return Ok(());
    }
    if destination.exists() {
        if !git_succeeds(&destination, &["rev-parse", "--show-toplevel"]) {
            return Err(format!(
                "destination path already exists and is not a git worktree: {}",
                destination.display()
            )
            .into());
        }
        info("destination worktree already exists; refreshing caches and metadata");
    } else {
        let mut command = git(&source);
        command.args(["worktree", "add"]);
        if git_succeeds(
            &source,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        ) {
            info(format!("create worktree from existing branch {branch}"));
            command.arg(&destination).arg(&branch);
        } else {
            info(format!(
                "create worktree from {} on new branch {branch}",
                options.base
            ));
            command
                .arg("-b")
                .arg(&branch)
                .arg(&destination)
                .arg(&options.base);
        }
        actions.command(&mut command)?;
    }
    if options.target {
        if options.full {
            cache::sync_tree(
                &actions,
                &source.join("target"),
                &destination.join("target"),
                "target cache",
            )?;
        } else {
            cache::seed_subset(
                &actions,
                &source.join("target"),
                &destination.join("target"),
                options.incremental,
            )?;
        }
    }
    if options.cargo {
        cache::sync_tree(
            &actions,
            &source.join(".cargo"),
            &destination.join(".cargo"),
            ".cargo config",
        )?;
    }
    if options.node {
        cache::sync_tree(
            &actions,
            &source.join("node_modules"),
            &destination.join("node_modules"),
            "node_modules",
        )?;
        frontend::ensure(&actions, &destination)?;
    } else {
        info("skip node_modules seeding by request");
    }
    let work_root = destination
        .join(".local/artifacts/agents")
        .join(&options.id);
    write_state(
        &actions,
        &options,
        &branch,
        &source,
        &destination,
        &work_root,
    )?;
    if destination.join("crates/xtask/src/skills.rs").is_file() {
        info("generate local Claude Code skill shims");
        actions.command(
            Command::new("cargo")
                .args(["xtask", "setup-skills"])
                .current_dir(&destination),
        )?;
    } else {
        info("skip skill shims: crates/xtask not present in worktree");
    }
    info(format!(
        "ready\n  worktree: {}\n  branch: {branch}\n  source: {}\n  work root: {}\n  cleanup: cargo xtask worktree --cleanup {}",
        destination.display(),
        source.display(),
        work_root.display(),
        options.id
    ));
    Ok(())
}

fn common_repo_root(source: &Path) -> Result<PathBuf, Error> {
    let common = PathBuf::from(git_text(Some(source), &["rev-parse", "--git-common-dir"])?);
    let common = if common.is_absolute() {
        common
    } else {
        fs::canonicalize(source.join(common))?
    };
    common
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "git common directory has no parent".into())
}

fn git(source: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(source);
    command
}

fn git_text(source: Option<&Path>, args: &[&str]) -> Result<String, Error> {
    let mut command = source.map(git).unwrap_or_else(|| Command::new("git"));
    let result = output(command.args(args))?;
    Ok(String::from_utf8(result.stdout)
        .map_err(|error| error.to_string())?
        .trim_end_matches('\n')
        .into())
}

fn git_succeeds(source: &Path, args: &[&str]) -> bool {
    let mut command = git(source);
    command
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    spawn(&mut command)
        .and_then(|mut child| wait(&mut child))
        .is_ok_and(|status| status.success())
}

fn cleanup(actions: &Actions, source: &Path, destination: &Path, force: bool) -> WorkflowResult {
    if !destination.is_dir() && !destination.is_symlink() {
        return Err(format!("cleanup target does not exist: {}", destination.display()).into());
    }
    info(format!("cleanup worktree: {}", destination.display()));
    let mut command = git(source);
    command.args(["worktree", "remove"]);
    if force {
        command.arg("-f");
    }
    command.arg(destination);
    actions.command(&mut command)?;
    info("cleanup stale worktree metadata");
    actions.command(git(source).args(["worktree", "prune"]))
}

pub(super) struct Actions {
    dry: bool,
}
impl Actions {
    fn command(&self, command: &mut Command) -> WorkflowResult {
        if self.dry {
            println!("dry-run: {command:?}");
            Ok(())
        } else {
            checked(command)
        }
    }
    fn mkdir(&self, path: &Path) -> WorkflowResult {
        if self.dry {
            println!("dry-run: create directory {}", path.display());
        } else {
            fs::create_dir_all(path)?;
        }
        Ok(())
    }
    fn remove(&self, path: &Path) -> WorkflowResult {
        if self.dry {
            println!("dry-run: remove {}", path.display());
            return Ok(());
        }
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(path)?,
            Ok(_) => fs::remove_file(path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}

fn info(message: impl AsRef<str>) {
    println!("agent-worktree: {}", message.as_ref());
}
fn warn(message: impl AsRef<str>) {
    eprintln!("agent-worktree: {}", message.as_ref());
}

// These files are sourced by the developer, never by this workflow. Quote values
// so paths/branches containing shell metacharacters remain literal data.
fn shell_value(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_state(
    actions: &Actions,
    options: &Options,
    branch: &str,
    source: &Path,
    destination: &Path,
    work_root: &Path,
) -> WorkflowResult {
    let state = destination.join(".agent-state");
    info(format!("write agent state in {}", state.display()));
    actions.mkdir(&state)?;
    if actions.dry {
        return Ok(());
    }
    let cleanup = format!(
        "cargo xtask worktree --cleanup {}",
        shell_value(&options.id)
    );
    let mode = if options.full { "full" } else { "subset" };
    let values = [
        ("AGENT_WORKTREE_ID", options.id.clone()),
        ("AGENT_WORKTREE_BRANCH", branch.into()),
        (
            "AGENT_WORKTREE_PATH",
            destination.to_string_lossy().into_owned(),
        ),
        ("AGENT_SOURCE_TREE", source.to_string_lossy().into_owned()),
        ("AGENT_WORK_ROOT", work_root.to_string_lossy().into_owned()),
        (
            "AGENT_WORK_DIR_TEMPLATE",
            format!("{}/<mode>", work_root.display()),
        ),
        ("AGENT_TARGET_CACHE_MODE", mode.into()),
        (
            "AGENT_TARGET_CACHE_WITH_INCREMENTAL",
            u8::from(options.incremental).to_string(),
        ),
        ("AGENT_WORKTREE_CLEANUP_COMMAND", cleanup.clone()),
    ];
    let content: String = values
        .iter()
        .map(|(key, value)| format!("{key}={}\n", shell_value(value)))
        .collect();
    fs::write(state.join("setup.env"), content)?;
    fs::write(
        state.join("README.txt"),
        format!(
            "This directory belongs to the agent worktree setup helper.\n\nUseful defaults:\n  source setup.env\n  export WORK_ROOT={}\n\nLive harness examples:\n  WORK_DIR={} target/debug/test_harness mixed-anchor\n  WORK_ROOT={} cargo run --bin test_harness -- suite --work-root {}\n\nCleanup when finished:\n  {cleanup}\n",
            shell_value(&work_root.to_string_lossy()),
            shell_value(&work_root.join("mixed-anchor").to_string_lossy()),
            shell_value(&work_root.join("suite").to_string_lossy()),
            shell_value(&work_root.join("suite").to_string_lossy())
        ),
    )?;
    Ok(())
}
