//! `cargo xtask gates`: route changed files to the narrowest checks from
//! AGENTS.md. Runs the cheap pre-commit gates and prints the broader
//! follow-up gates; `.githooks/pre-commit` calls it on the staged diff.

use std::collections::BTreeSet;
use std::process::Command;

use crate::source_audit::glob;
use crate::{StepResult, cargo, run, source_audit};

/// Gates run before a commit, in run order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum AutoGate {
    Fmt,
    ClippyLib,
    ClippyWorkspace,
    ClippyMcp,
    ShellSyntax,
    FrontendFormat,
    Docs,
    SourceAudit,
}

const CLIPPY_MCP: &str =
    "cargo clippy --workspace --all-targets --features mcp-server,mcp-http-backend -- -D warnings";

impl AutoGate {
    fn label(self) -> &'static str {
        match self {
            Self::Fmt => "cargo fmt --all --check",
            Self::ClippyLib => "cargo clippy --lib -- -D warnings",
            Self::ClippyWorkspace => "cargo clippy --workspace --all-targets -- -D warnings",
            Self::ClippyMcp => CLIPPY_MCP,
            Self::ShellSyntax => "bash -n staged shell files",
            Self::FrontendFormat => "npm run format:check",
            Self::Docs => "node scripts/check/docs.mjs",
            Self::SourceAudit => "cargo xtask source-audit",
        }
    }
}

enum Diff {
    Staged,
    Unstaged,
    Base(String),
}

impl Diff {
    /// `git diff` arguments selecting this diff.
    fn git_args(&self) -> Vec<String> {
        match self {
            Self::Staged => vec!["--cached".into()],
            Self::Unstaged => Vec::new(),
            Self::Base(base) => vec![format!("{base}...HEAD")],
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
            Self::Base(_) => "base",
        }
    }
}

const USAGE: &str = "usage: cargo xtask gates [--staged | --unstaged | --base <ref>] [--dry-run]

Route changed files to the narrowest repo checks from AGENTS.md.
Run pre-commit gates automatically; print broader follow-up gates.";

pub(crate) fn run_gates(args: &[&str]) -> StepResult {
    let mut diff = Diff::Staged;
    let mut dry_run = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match *arg {
            "--staged" => diff = Diff::Staged,
            "--unstaged" => diff = Diff::Unstaged,
            "--base" => {
                let base = rest.next().ok_or("gates: --base requires a ref")?;
                diff = Diff::Base((*base).to_owned());
            }
            "--dry-run" => dry_run = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("gates: unknown option: {other}\n{USAGE}")),
        }
    }

    let changed = git_lines(&diff, &["--name-only", "--diff-filter=ACMR"])?;
    if changed.is_empty() {
        println!("gates: no changed files for {} diff", diff.name());
        return Ok(());
    }
    let rust: Vec<&str> = changed
        .iter()
        .map(String::as_str)
        .filter(|f| f.ends_with(".rs"))
        .collect();
    let mut plan = route(&changed);
    if !rust.is_empty() {
        let mut args = vec!["-U0", "--"];
        args.extend(&rust);
        if git_lines(&diff, &args)?
            .iter()
            .any(|line| adds_concurrency(line))
        {
            plan.follow_up("cargo xtask concurrency fast");
        }
    }
    for file in &rust {
        if !is_lifecycle(file)
            && !is_fixture_or_harness(file)
            && let Some(filter) = module_filter(file)
        {
            plan.follow_up(&format!("cargo test {filter}"));
        }
    }

    plan.print(&diff, &changed);
    if dry_run {
        return Ok(());
    }
    plan.run(&changed)
}

#[derive(Default)]
struct Plan {
    auto: BTreeSet<AutoGate>,
    follow_ups: Vec<String>,
    manual: Vec<String>,
}

impl Plan {
    fn follow_up(&mut self, gate: &str) {
        if !self.follow_ups.iter().any(|known| known == gate) {
            self.follow_ups.push(gate.to_owned());
        }
    }

    fn manual(&mut self, gate: &str) {
        if !self.manual.iter().any(|known| known == gate) {
            self.manual.push(gate.to_owned());
        }
    }

    fn print(&self, diff: &Diff, changed: &[String]) {
        println!(
            "gates: {} diff selected {} file(s)",
            diff.name(),
            changed.len()
        );
        for file in changed {
            println!("  {file}");
        }
        println!();
        println!("gates: pre-commit gates");
        if self.auto.is_empty() {
            println!("  (none)");
        }
        for gate in &self.auto {
            println!("  {}", gate.label());
        }
        for (title, gates) in [
            ("recommended follow-up gates", &self.follow_ups),
            ("additional manual recommendations", &self.manual),
        ] {
            if !gates.is_empty() {
                println!();
                println!("gates: {title}");
                for gate in gates {
                    println!("  {gate}");
                }
            }
        }
    }

    fn run(&self, changed: &[String]) -> StepResult {
        for gate in &self.auto {
            println!();
            println!("gates: running {}", gate.label());
            let result = match gate {
                AutoGate::Fmt => cargo(&["fmt", "--all", "--check"]),
                AutoGate::ClippyLib => cargo(&["clippy", "--lib", "--", "-D", "warnings"]),
                AutoGate::ClippyWorkspace => cargo(&[
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ]),
                AutoGate::ClippyMcp => cargo(&[
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--features",
                    "mcp-server,mcp-http-backend",
                    "--",
                    "-D",
                    "warnings",
                ]),
                AutoGate::ShellSyntax => changed
                    .iter()
                    .filter(|file| is_shell(file))
                    .try_for_each(|file| run("bash", &["-n", file], &[])),
                AutoGate::FrontendFormat => run("npm", &["run", "format:check"], &[]),
                AutoGate::Docs => run("node", &["scripts/check/docs.mjs"], &[]),
                AutoGate::SourceAudit => source_audit::run(),
            };
            result.map_err(|error| format!("gates: failed: {}: {error}", gate.label()))?;
        }
        Ok(())
    }
}

/// The routing table: which gates each changed file selects.
fn route(changed: &[String]) -> Plan {
    let mut plan = Plan::default();
    for file in changed.iter().map(String::as_str) {
        if file.ends_with(".md") || file == "scripts/check/docs.mjs" {
            plan.auto.insert(AutoGate::Docs);
        }
        if file.ends_with(".rs") {
            plan.auto.insert(AutoGate::Fmt);
        }
        if is_rust_lint_scope(file) {
            plan.auto.extend([
                AutoGate::ClippyLib,
                AutoGate::ClippyWorkspace,
                AutoGate::ClippyMcp,
            ]);
        }
        if is_shell(file) {
            plan.auto.insert(AutoGate::ShellSyntax);
        }
        if is_frontend(file) {
            plan.auto.insert(AutoGate::FrontendFormat);
            plan.follow_up("npm run test:frontend");
        }
        if is_lifecycle(file) {
            plan.follow_up("cargo xtask concurrency contract");
        }
        if is_api_contract(file) {
            plan.follow_up("cargo xtask api-contract");
        }
        if is_fixture_or_harness(file) {
            plan.follow_up("cargo xtask fixture-discipline");
        }
        if any(file, &["src/media/*", "benches/*"]) {
            plan.manual("relevant cargo bench --bench <name>");
        }
        if is_protocol(file) {
            plan.manual("target/debug/test_harness correctness*");
        }
        if is_mcp_surface(file) && !is_rust_lint_scope(file) {
            plan.follow_up(CLIPPY_MCP);
        }
        if is_source_audit_scope(file) {
            plan.auto.insert(AutoGate::SourceAudit);
        }
    }
    plan
}

fn any(file: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|pattern| glob(pattern, file))
}

fn is_shell(file: &str) -> bool {
    file.ends_with(".sh") || file.starts_with(".githooks/")
}

fn is_rust_lint_scope(file: &str) -> bool {
    file.ends_with(".rs")
        || matches!(
            file,
            "Cargo.toml" | "Cargo.lock" | "rust-toolchain.toml" | ".cargo/config.toml"
        )
}

fn is_frontend(file: &str) -> bool {
    any(
        file,
        &["web/ts/*.ts", "web/pages/*.html", "web/styles/input.css"],
    )
}

fn is_lifecycle(file: &str) -> bool {
    any(
        file,
        &[
            "src/media/engine.rs",
            "src/media/srt.rs",
            "src/media/ts_chunk_ring.rs",
            "src/media/avio.rs",
            "src/media/recording.rs",
            "src/media/recording/*.rs",
            "src/media/file_ingest.rs",
            "src/media/external_transcoder.rs",
            "src/media/external_transcoder/*.rs",
        ],
    )
}

fn is_api_contract(file: &str) -> bool {
    any(
        file,
        &[
            "src/api.rs",
            "src/api/*.rs",
            "src/api_runtime_views.rs",
            "src/api_runtime_views/*.rs",
            "src/api_view_models.rs",
            "src/bin/test_harness/api_client.rs",
            "web/ts/core/api.ts",
            "web/ts/types.ts",
            "docs/api-reference.md",
        ],
    )
}

fn is_fixture_or_harness(file: &str) -> bool {
    any(
        file,
        &[
            "test/*",
            "tests/fixtures.rs",
            "src/test_fixtures.rs",
            "benches/*",
            "scripts/fixtures/*",
            "scripts/harness/*",
            "scripts/build/bench-harness.sh",
            "src/bin/test_harness.rs",
            "src/bin/test_harness/*",
        ],
    )
}

fn is_protocol(file: &str) -> bool {
    any(
        file,
        &[
            "src/media/rtmp.rs",
            "src/media/rtmp/*",
            "src/media/srt.rs",
            "src/media/srt_*.rs",
            "src/media/srt/*",
            "src/media/hls.rs",
            "src/media/hls/*",
        ],
    )
}

fn is_mcp_surface(file: &str) -> bool {
    any(
        file,
        &[
            "src/api/agent.rs",
            "src/agent_plane.rs",
            "src/agent_backends/*",
            "src/agent_core/*",
            "src/agent_mcp/*",
            "src/bin/restream-mcp.rs",
            "src/lib.rs",
            "Cargo.toml",
        ],
    )
}

fn is_source_audit_scope(file: &str) -> bool {
    !file.starts_with("test/fixtures/")
        && any(
            file,
            &[
                "src/*.rs",
                "web/ts/*.ts",
                "test/*.rs",
                "test/*.ts",
                "test/*.mjs",
                "test/*.js",
            ],
        )
}

/// `cargo test` filter for a changed Rust file: the file stem, except for
/// binaries, crate roots, `mod.rs` and benches, which have no useful filter.
fn module_filter(file: &str) -> Option<&str> {
    if any(
        file,
        &[
            "src/bin/*",
            "src/main.rs",
            "src/lib.rs",
            "*/mod.rs",
            "benches/*.rs",
        ],
    ) {
        return None;
    }
    if !(glob("tests/*.rs", file) || glob("src/*.rs", file)) {
        return None;
    }
    let name = file.rsplit('/').next()?;
    name.strip_suffix(".rs")
}

const CONCURRENCY_WORDS: &[&str] = &[
    "tokio::spawn",
    "spawn_blocking",
    "std::thread",
    "thread::spawn",
    "Mutex",
    "RwLock",
    "Notify",
    "Semaphore",
    "JoinHandle",
    "catch_unwind",
];
const CONCURRENCY_SCOPES: &[&str] = &["mpsc::", "watch::", "broadcast::", "oneshot::"];

/// An added diff line that introduces a concurrency primitive.
fn adds_concurrency(line: &str) -> bool {
    if !line.starts_with('+') {
        return false;
    }
    let word_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // (start, end) of every occurrence of `needle` at a word start.
    let at_word_start = |needle: &str| -> Vec<usize> {
        line.match_indices(needle)
            .filter(|(at, _)| !line[..*at].ends_with(word_char))
            .map(|(at, found)| at + found.len())
            .collect()
    };
    CONCURRENCY_WORDS
        .iter()
        .any(|word| at_word_start(word).into_iter().any(|end| !line[end..].starts_with(word_char)))
        || CONCURRENCY_SCOPES.iter().any(|scope| !at_word_start(scope).is_empty())
        // Any identifier starting with `Atomic` (AtomicBool, AtomicU64, ...).
        || !at_word_start("Atomic").is_empty()
}

fn git_lines(diff: &Diff, args: &[&str]) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("diff")
        .args(diff.git_args())
        .args(args)
        .output()
        .map_err(|error| format!("cannot run git diff: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_for(files: &[&str]) -> Plan {
        route(
            &files
                .iter()
                .map(|file| (*file).to_owned())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn routing_selects_gates_per_file_kind() {
        let plan = plan_for(&["src/media/engine.rs", "web/ts/core/api.ts", "docs/x.md"]);
        assert_eq!(
            plan.auto.iter().copied().collect::<Vec<_>>(),
            vec![
                AutoGate::Fmt,
                AutoGate::ClippyLib,
                AutoGate::ClippyWorkspace,
                AutoGate::ClippyMcp,
                AutoGate::FrontendFormat,
                AutoGate::Docs,
                AutoGate::SourceAudit,
            ]
        );
        assert_eq!(
            plan.follow_ups,
            vec![
                "cargo xtask concurrency contract",
                "npm run test:frontend",
                "cargo xtask api-contract",
            ]
        );
        assert_eq!(plan.manual, vec!["relevant cargo bench --bench <name>"]);
    }

    #[test]
    fn fixtures_are_outside_the_source_audit_and_docs_only_runs_docs() {
        assert!(plan_for(&["test/fixtures/a.ts"]).auto.is_empty());
        let docs = plan_for(&["README.md"]);
        assert_eq!(
            docs.auto.into_iter().collect::<Vec<_>>(),
            vec![AutoGate::Docs]
        );
        assert!(docs.follow_ups.is_empty());
    }

    #[test]
    fn module_filter_uses_the_stem_except_for_roots_and_benches() {
        assert_eq!(
            module_filter("src/media/hls/segmenter.rs"),
            Some("segmenter")
        );
        assert_eq!(module_filter("tests/api.rs"), Some("api"));
        assert_eq!(module_filter("src/media/mod.rs"), None);
        assert_eq!(module_filter("src/lib.rs"), None);
        assert_eq!(module_filter("src/bin/test_harness/main.rs"), None);
        assert_eq!(module_filter("benches/api_health.rs"), None);
        assert_eq!(module_filter("xtask/src/main.rs"), None);
    }

    #[test]
    fn concurrency_detection_needs_an_added_whole_word() {
        assert!(adds_concurrency("+    let m = Mutex::new(0);"));
        assert!(adds_concurrency(
            "+    let n: AtomicU64 = AtomicU64::new(0);"
        ));
        assert!(adds_concurrency("+    let (tx, rx) = mpsc::channel();"));
        assert!(adds_concurrency("+tokio::spawn(async {});"));
        assert!(!adds_concurrency("-    let m = Mutex::new(0);"));
        assert!(!adds_concurrency(
            "+    let my_mutex_guard = MutexGuardLike;"
        ));
        assert!(!adds_concurrency("+    let x = NonAtomicCounter;"));
        assert!(!adds_concurrency("+    let x = self.mpsc_queue;"));
    }
}
