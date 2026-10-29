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
        if any(file, &["src/media/*", "crates/restream-*/*", "benches/*"]) {
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
        if (file.ends_with(".rs") || file.ends_with("/Cargo.toml"))
            && let Some(package) = file
                .strip_prefix("crates/")
                .and_then(|path| path.split('/').next())
            && library_tests(&mut plan, package)
        {
            continue;
        }
        if file.ends_with(".rs") {
            if file.starts_with("crates/") {
                // Command packages retain their own test harness.
                plan.follow_up(&package_test_command(
                    file,
                    module_filter(file).unwrap_or(""),
                ));
            } else if !is_lifecycle(file)
                && !is_fixture_or_harness(file)
                && let Some(filter) = module_filter(file)
            {
                plan.follow_up(&package_test_command(file, filter));
            }
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
        || file.ends_with("/Cargo.toml")
        || matches!(
            file,
            "Cargo.toml" | "Cargo.lock" | "rust-toolchain.toml" | ".cargo/config.toml"
        )
}

/// Select actual owner suites and consumers retained by the application.
fn library_tests(plan: &mut Plan, package: &str) -> bool {
    let (target, filter) = match package {
        "restream-core" => ("--lib", "media::codec::tests"),
        "restream-sync" => ("--test standby_gop", ""),
        "restream-mpegts" => ("--lib", "media::mpegts::tests"),
        "restream-rtmp" => ("--lib", "media::rtmp::tests"),
        "restream-srt" => ("--lib", "media::srt::"),
        "restream-moq" => ("--lib", "media::moq::"),
        _ => return false,
    };
    // MPEG-TS's behavior suite remains entirely in the root package.
    if package != "restream-mpegts" {
        plan.follow_up(&guarded_test_command(
            &format!("cargo test -p {package} --lib"),
            "",
        ));
    }
    plan.follow_up(&guarded_test_command(
        &format!("cargo test -p restream {target}"),
        filter,
    ));
    true
}

fn guarded_test_command(command: &str, filter: &str) -> String {
    let filter = if filter.is_empty() { "''" } else { filter };
    format!("scripts/ci/require-test-filters.sh {command} -- {filter} && {command} -- {filter}")
}

fn package_test_command(file: &str, filter: &str) -> String {
    if let Some(crate_path) = file.strip_prefix("crates/")
        && let Some(pkg) = crate_path.split('/').next()
    {
        return guarded_test_command(&format!("cargo test -p {pkg}"), filter);
    }
    guarded_test_command("cargo test", filter)
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
            "src/media/byte_queue.rs",
            "src/media/recording.rs",
            "src/media/recording/*.rs",
            "src/media/external_transcoder.rs",
            "src/media/external_transcoder/*.rs",
            "src/media/external_file_ingest.rs",
            "src/media/external_file_ingest/*.rs",
            "crates/restream-sync/*",
            "crates/restream-srt/*",
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
            "crates/test_harness/src/test_harness/api_client.rs",
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
            "crates/xtask/src/bins.rs",
            "crates/test_harness/src/main.rs",
            "crates/test_harness/src/test_harness/*",
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
            "crates/restream-rtmp/*",
            "crates/restream-srt/*",
            "crates/restream-mpegts/*",
            "crates/restream-moq/*",
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
                "crates/*.rs",
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
            "crates/*/src/main.rs",
            "crates/*/src/lib.rs",
            "*/mod.rs",
            "benches/*.rs",
        ],
    ) {
        return None;
    }
    if !(glob("tests/*.rs", file) || glob("src/*.rs", file) || glob("crates/*.rs", file)) {
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
        assert_eq!(module_filter("crates/test_harness/src/main.rs"), None);
        assert_eq!(module_filter("benches/api_health.rs"), None);
        assert_eq!(module_filter("crates/xtask/src/main.rs"), None);
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

    #[test]
    fn extracted_crate_routing_selects_package_tests_and_manifest_lints() {
        for (package, module, lifecycle, protocol) in [
            ("restream-core", "codec", false, false),
            ("restream-sync", "byte_queue", true, false),
            ("restream-mpegts", "demux", false, true),
            ("restream-rtmp", "flv", false, true),
            ("restream-srt", "calls", true, true),
            ("restream-moq", "media", false, true),
        ] {
            let source = format!("crates/{package}/src/{module}.rs");
            let plan = plan_for(&[&source]);
            if package != "restream-mpegts" {
                assert!(
                    plan.follow_ups
                        .iter()
                        .any(|command| command
                            .contains(&format!("cargo test -p {package} --lib -- ''"))),
                    "{source}: missing owning library suite"
                );
            } else {
                assert!(
                    !plan
                        .follow_ups
                        .iter()
                        .any(|command| command.contains("cargo test -p restream-mpegts")),
                    "do not recommend MPEG-TS's empty library suite"
                );
            }
            assert_eq!(
                plan.follow_ups
                    .contains(&"cargo xtask concurrency contract".to_owned()),
                lifecycle,
                "{source}"
            );
            assert_eq!(
                plan.manual
                    .contains(&"target/debug/test_harness correctness*".to_owned()),
                protocol,
                "{source}"
            );
            assert!(
                plan.manual
                    .contains(&"relevant cargo bench --bench <name>".to_owned()),
                "{source}"
            );
            let root = format!("crates/{package}/src/lib.rs");
            assert!(
                plan_for(&[&root])
                    .follow_ups
                    .iter()
                    .any(|command| { command.contains("cargo test -p restream ") }),
                "{root}: missing retained consumer suite"
            );
            let manifest = format!("crates/{package}/Cargo.toml");
            let plan = plan_for(&[&manifest]);
            assert_eq!(
                plan.auto.into_iter().collect::<Vec<_>>(),
                vec![
                    AutoGate::ClippyLib,
                    AutoGate::ClippyWorkspace,
                    AutoGate::ClippyMcp,
                ],
                "{manifest}"
            );
        }
        for (source, command) in [
            (
                "crates/test_harness/src/main.rs",
                "cargo test -p test_harness",
            ),
            (
                "crates/test_harness/src/test_harness/api_client.rs",
                "cargo test -p test_harness -- api_client",
            ),
        ] {
            let plan = plan_for(&[source]);
            assert!(
                plan.follow_ups
                    .iter()
                    .any(|guarded| guarded.contains(command)),
                "{source}"
            );
            assert!(
                plan.follow_ups
                    .contains(&"cargo xtask fixture-discipline".to_owned()),
                "{source}"
            );
        }
        assert!(
            plan_for(&["src/media/hls/segmenter.rs"])
                .follow_ups
                .iter()
                .any(|command| command.contains("cargo test -- segmenter"))
        );
    }

    #[test]
    fn mpegts_changes_select_the_retained_consumer_suite() {
        for path in [
            "crates/restream-mpegts/src/lib.rs",
            "crates/restream-mpegts/src/demux.rs",
            "crates/restream-mpegts/src/mux.rs",
            "crates/restream-mpegts/Cargo.toml",
        ] {
            let plan = plan_for(&[path]);
            assert!(
                plan.follow_ups.iter().any(|command| {
                    command.starts_with("scripts/ci/require-test-filters.sh ")
                        && command.contains("cargo test -p restream --lib -- media::mpegts::tests")
                }),
                "{path}: missing guarded root MPEG-TS tests"
            );
        }
    }

    #[test]
    fn focused_recommendations_reject_empty_selection_before_running() {
        struct TempProject(std::path::PathBuf);
        impl Drop for TempProject {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let project = TempProject(std::env::temp_dir().join(format!(
            "restream-test-selection-{}-{unique}",
            std::process::id()
        )));
        std::fs::create_dir_all(project.0.join("src")).unwrap();
        std::fs::write(project.0.join("Cargo.toml"),
            "[package]\nname = \"selection_fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n"
        ).unwrap();
        std::fs::write(
            project.0.join("src/lib.rs"),
            r#"
#[test]
fn records_execution() {
    std::fs::write(std::env::var_os("GATE_TEST_MARKER").unwrap(), b"executed").unwrap();
}
"#,
        )
        .unwrap();
        let marker = project.0.join("executed");
        let command = format!(
            "cargo test --offline --manifest-path '{}' --lib",
            project.0.join("Cargo.toml").display()
        );
        for (filter, should_run) in [
            ("records_execution", true),
            ("missing_test", false),
            ("", true),
        ] {
            let output = Command::new("bash")
                .args(["-c", &guarded_test_command(&command, filter)])
                .current_dir(crate::repo_root())
                .env("CARGO_TARGET_DIR", project.0.join("target"))
                .env("CARGO_BUILD_JOBS", "1")
                .env("RUSTC_WORKSPACE_WRAPPER", "")
                .env("GATE_TEST_MARKER", &marker)
                .output()
                .unwrap();
            assert_eq!(
                output.status.success(),
                should_run,
                "{filter}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(marker.exists(), should_run);
            if should_run {
                std::fs::remove_file(&marker).unwrap();
            }
        }
    }

    /// Extract the workflow's scope step verbatim, without duplicating its
    /// predicates or requiring a YAML dependency (or Cargo in the CI step).
    fn workflow_scope(workflow: &str, job: &str) -> String {
        let job_header = format!("  {job}:");
        let mut lines = workflow
            .lines()
            .skip_while(|line| *line != job_header)
            .skip(1)
            .take_while(|line| !line.starts_with("  ") || line.starts_with("    "));
        assert!(
            lines.any(|line| line == "      - id: scope"),
            "{job}: missing scope step"
        );
        assert!(
            lines.any(|line| line == "        run: |"),
            "{job}: missing scope script"
        );
        let script = lines
            .take_while(|line| line.is_empty() || line.starts_with("          "))
            .map(|line| line.strip_prefix("          ").unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!script.is_empty(), "{job}: empty scope script");
        script
    }

    fn scope_runs(script: &str, paths: &str, event: &str, base: &str) -> bool {
        // Only Git is substituted: the real workflow's Bash and grep execute.
        // Writing the Actions output to stdout avoids shared process state or
        // temporary files when tests run concurrently.
        let script = format!(
            "git() {{\n\
             case \"$1\" in\n\
             fetch) return 0 ;;\n\
             diff) printf '%s\\n' \"$CHANGED_FILES\" ;;\n\
             *) return 2 ;;\n\
             esac\n\
             }}\n{script}"
        );
        let output = Command::new("bash")
            .args(["-e", "-o", "pipefail", "-c", &script])
            .env("EVENT_NAME", event)
            .env("BASE_SHA", base)
            .env("BEFORE_SHA", base)
            .env("GITHUB_SHA", "head")
            .env("GITHUB_OUTPUT", "/dev/stdout")
            .env("CHANGED_FILES", paths)
            .output()
            .expect("execute workflow scope with Bash");
        assert!(
            output.status.success(),
            "scope failed for {paths}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        match String::from_utf8_lossy(&output.stdout).trim() {
            "run=true" => true,
            "run=false" => false,
            other => panic!("invalid scope output for {paths}: {other}"),
        }
    }

    #[test]
    fn ci_workflow_scopes_execute_extracted_crate_changes() {
        let workflow = std::fs::read_to_string(crate::repo_root().join(".github/workflows/ci.yml"))
            .expect("read ci.yml");
        for job in ["docker-runtime-smoke", "fuzz-smoke", "moq-interop"] {
            let scope = workflow_scope(&workflow, job);
            for (package, module, affects_moq) in [
                ("restream-core", "codec", true),
                ("restream-sync", "byte_queue", true),
                ("restream-mpegts", "demux", false),
                ("restream-rtmp", "flv", false),
                ("restream-srt", "calls", false),
                ("restream-moq", "media", true),
            ] {
                for path in [
                    format!("crates/{package}/src/{module}.rs"),
                    format!("crates/{package}/Cargo.toml"),
                ] {
                    assert_eq!(
                        scope_runs(&scope, &path, "pull_request", "base"),
                        job != "moq-interop" || affects_moq,
                        "{job}: {path}"
                    );
                }
            }
            for (path, docker, fuzz, moq) in [
                ("src/media/rtmp/egress.rs", true, true, false),
                ("src/media/egress/output.rs", true, true, true),
                ("crates/test_harness/src/main.rs", true, true, false),
                ("crates/test_harness/Cargo.toml", true, true, false),
                (
                    "crates/test_harness/src/test_harness/moq_dev_interop/run.rs",
                    true,
                    true,
                    true,
                ),
                (
                    "crates/test_harness/src/test_harness/moq_dev_interop/peers.rs",
                    true,
                    true,
                    true,
                ),
                (
                    "crates/test_harness/src/test_harness/moq_dev_interop.rs",
                    true,
                    true,
                    true,
                ),
                ("fuzz/fuzz_targets/flv.rs", false, true, false),
                ("scripts/dev/moq/fetch-moq-dev.sh", false, false, true),
            ] {
                let expected = match job {
                    "docker-runtime-smoke" => docker,
                    "fuzz-smoke" => fuzz,
                    "moq-interop" => moq,
                    _ => unreachable!(),
                };
                assert_eq!(
                    scope_runs(&scope, path, "pull_request", "base"),
                    expected,
                    "{job}: existing scope {path}"
                );
            }
            for path in [
                "README.md",
                "web/ts/core/api.ts",
                "crates/xtask/src/gates.rs",
                "crates/unrelated/Cargo.toml",
                "docs/crates/restream-moq/src/media.rs",
            ] {
                assert!(
                    !scope_runs(&scope, path, "pull_request", "base"),
                    "{job}: unrelated path {path}"
                );
            }
            assert!(
                !scope_runs(&scope, "", "pull_request", "base"),
                "{job}: empty diff"
            );
            assert!(
                scope_runs(&scope, "README.md", "pull_request", ""),
                "{job}: missing base must run"
            );
            // Docker retains targeted push routing; parser/MoQ pushes always run.
            assert_eq!(
                scope_runs(&scope, "README.md", "push", "base"),
                job != "docker-runtime-smoke",
                "{job}: push routing"
            );
            assert!(
                scope_runs(
                    &scope,
                    "README.md\ncrates/restream-core/src/codec.rs\nweb/ts/core/api.ts",
                    "pull_request",
                    "base",
                ),
                "{job}: mixed diff"
            );
        }
    }
}
