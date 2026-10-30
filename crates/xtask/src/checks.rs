//! The CI gates as data. A step is one command or one check written in Rust;
//! a group runs its steps in order. Every group and step name is a top-level
//! `cargo xtask` command, so CI jobs, the pre-commit hook and local runs call
//! a name here instead of repeating the commands.

use crate::workflow::{self, WorkflowResult};
use crate::{docs_audit, frontend_style, fuzz, scan, source_audit, test_filters};
use std::fmt::{self, Write as _};
use std::process::{Command, Stdio};
use std::time::Instant;

#[cfg(test)]
mod tests;

/// A program run from the repository root, with `env` added to the
/// inherited environment.
#[derive(Clone, Copy)]
struct Cmd {
    program: &'static str,
    args: &'static [&'static str],
    env: &'static [(&'static str, &'static str)],
}

const fn cargo(args: &'static [&'static str]) -> Cmd {
    Cmd {
        program: "cargo",
        args,
        env: &[],
    }
}

impl Cmd {
    const fn env(self, env: &'static [(&'static str, &'static str)]) -> Self {
        Self { env, ..self }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(self.program);
        command.args(self.args).envs(self.env.iter().copied());
        command
    }
}

impl fmt::Display for Cmd {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let quoted = |word: &str| if word.is_empty() { "''" } else { word }.to_owned();
        for (name, value) in self.env {
            write!(formatter, "{name}={} ", quoted(value))?;
        }
        formatter.write_str(self.program)?;
        for arg in self.args {
            write!(formatter, " {}", quoted(arg))?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Run {
    Command(Cmd),
    /// `require-test-filters` over the command: lists the tests each filter
    /// after its `--` selects, without running them; an empty selection
    /// fails instead of passing as a zero-test run.
    Filters(Cmd),
    Builtin(fn() -> WorkflowResult),
}

/// An installed tool the step needs: `probe` must succeed, else the step
/// fails and names `hint`.
#[derive(Clone, Copy)]
struct Tool {
    probe: &'static [&'static str],
    hint: &'static str,
}

#[derive(Clone, Copy)]
struct Step {
    name: &'static str,
    run: Run,
    tool: Option<Tool>,
}

impl Step {
    const fn command(name: &'static str, cmd: Cmd) -> Self {
        Self {
            name,
            run: Run::Command(cmd),
            tool: None,
        }
    }

    const fn filters(name: &'static str, cmd: Cmd) -> Self {
        Self {
            name,
            run: Run::Filters(cmd),
            tool: None,
        }
    }

    const fn builtin(name: &'static str, check: fn() -> WorkflowResult) -> Self {
        Self {
            name,
            run: Run::Builtin(check),
            tool: None,
        }
    }

    const fn needs(self, tool: Tool) -> Self {
        Self {
            tool: Some(tool),
            ..self
        }
    }
}

struct Group {
    name: &'static str,
    about: &'static str,
    steps: &'static [&'static Step],
}

static FMT: Step = Step::command("fmt", cargo(&["fmt", "--all", "--check"]));
static TSC: Step = Step::command(
    "tsc",
    Cmd {
        program: "npx",
        args: &["tsc", "-p", "tsconfig.json", "--noEmit"],
        env: &[],
    },
);

static CLIPPY_LIB: Step = Step::command(
    "clippy-lib",
    cargo(&["clippy", "--lib", "--", "-D", "warnings"]),
);
static CLIPPY_WORKSPACE: Step = Step::command(
    "clippy-workspace",
    cargo(&[
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]),
);
static CLIPPY_MCP: Step = Step::command(
    "clippy-mcp",
    cargo(&[
        "clippy",
        "--workspace",
        "--all-targets",
        "--features",
        "mcp-server,mcp-http-backend",
        "--",
        "-D",
        "warnings",
    ]),
);
// The MCP feature matrix selects the product package explicitly: workspace
// members such as the harness otherwise re-enable its default features.
// Lower layers compile with agent-plane and agent-execution disabled.
static CHECK_MCP_CORE: Step = Step::command(
    "check-mcp-core",
    cargo(&[
        "check",
        "-p",
        "restream",
        "--lib",
        "--no-default-features",
        "--features",
        "mcp-core",
    ]),
);
static CHECK_MCP_SERVER: Step = Step::command(
    "check-mcp-server",
    cargo(&[
        "check",
        "-p",
        "restream",
        "--lib",
        "--no-default-features",
        "--features",
        "mcp-server",
    ]),
);
static CHECK_MCP_BIN: Step = Step::command(
    "check-mcp-bin",
    cargo(&[
        "check",
        "-p",
        "restream",
        "--bin",
        "restream-mcp",
        "--no-default-features",
        "--features",
        "mcp-server,mcp-http-backend",
    ]),
);
/// The compatibility combination mcp-embedded = mcp-core + agent-plane must
/// compile without inventing an in-process MCP backend.
static CHECK_MCP_EMBEDDED: Step = Step::command(
    "check-mcp-embedded",
    cargo(&[
        "check",
        "-p",
        "restream",
        "--lib",
        "--no-default-features",
        "--features",
        "mcp-embedded",
    ]),
);
/// The agent tool list (discovery, MCP catalog, dispatch) and agent output
/// mutations.
static TEST_AGENT_EMBEDDED: Step = Step::command(
    "test-agent-embedded",
    cargo(&[
        "test",
        "-p",
        "restream",
        "--lib",
        "--no-default-features",
        "--features",
        "mcp-embedded,agent-execution",
        "--",
        "agent_mcp",
        "agent_service",
    ]),
);
/// Both compiled-out tests must exist, not merely a passing test binary: a
/// feature-unified build must fail here instead of testing enabled routes.
static AGENT_COMPILED_OUT_FILTERS: Step = Step::filters(
    "agent-compiled-out-filters",
    cargo(&[
        "test",
        "-p",
        "restream",
        "--no-default-features",
        "--test",
        "api",
        "--",
        "agent::agent_plane_returns_404_when_feature_is_compiled_out",
        "agent::agent_context_returns_404_when_feature_is_compiled_out",
    ]),
);
static CLIPPY_NO_DEFAULT: Step = Step::command(
    "clippy-no-default",
    cargo(&[
        "clippy",
        "-p",
        "restream",
        "--lib",
        "--test",
        "api",
        "--no-default-features",
        "--",
        "-D",
        "warnings",
    ]),
);
static TEST_AGENT_COMPILED_OUT: Step = Step::command(
    "test-agent-compiled-out",
    cargo(&[
        "test",
        "-p",
        "restream",
        "--no-default-features",
        "--test",
        "api",
        "--",
        "agent::",
    ]),
);

static SOURCE_AUDIT: Step =
    Step::builtin("source-audit", || source_audit::run().map_err(Into::into));
static DOCS_AUDIT: Step = Step::builtin("docs-audit", docs_audit::run);
static FRONTEND_STYLE: Step = Step::builtin("frontend-style", frontend_style::run);

static TEST_HYGIENE: Step =
    Step::builtin("test-hygiene", || scan::test_hygiene().map_err(Into::into));
static FIXTURE_DISCIPLINE: Step = Step::builtin("fixture-discipline", || {
    scan::fixture_discipline().map_err(Into::into)
});

const MIRI: Tool = Tool {
    probe: &["cargo", "+nightly", "miri", "--version"],
    hint: "rustup toolchain install nightly --profile minimal --component miri,rust-src",
};
// The empty RUSTFLAGS keeps nightly lints from failing the gate without a
// code change.
const MIRI_ENGINE: Cmd = cargo(&[
    "+nightly",
    "miri",
    "test",
    "-p",
    "restream-engine",
    "--lib",
    "--",
    "egress::tls::ktls::tests::ancillary",
    "egress::leaf_arena",
    "egress::scheduler",
])
.env(&[
    ("MIRIFLAGS", "-Zmiri-disable-isolation"),
    ("PROPTEST_CASES", "8"),
    ("RUSTFLAGS", ""),
]);
const MIRI_CORE: Cmd = cargo(&[
    "+nightly",
    "miri",
    "test",
    "-p",
    "restream-core",
    "--lib",
    "--",
    "codec::bits::tests",
])
.env(&[("MIRIFLAGS", "-Zmiri-disable-isolation"), ("RUSTFLAGS", "")]);
static MIRI_ENGINE_FILTERS: Step = Step::filters("miri-engine-filters", MIRI_ENGINE).needs(MIRI);
static MIRI_ENGINE_RUN: Step = Step::command("miri-engine", MIRI_ENGINE).needs(MIRI);
static MIRI_CORE_FILTERS: Step = Step::filters("miri-core-filters", MIRI_CORE).needs(MIRI);
static MIRI_CORE_RUN: Step = Step::command("miri-core", MIRI_CORE).needs(MIRI);

/// Every proof `cargo kani -p restream-engine --lib` finds; a proof that stops compiling (a cfg
/// typo, a moved module) must fail the gate, not vanish.
const KANI_PROOFS: usize = 7;
static KANI: Step = Step::builtin("kani-proofs", kani).needs(Tool {
    probe: &["cargo", "kani", "--version"],
    hint: "cargo install --locked kani-verifier --version 0.68.0 && cargo kani setup",
});

const NIGHTLY: Tool = Tool {
    probe: &["cargo", "+nightly", "--version"],
    hint: "rustup toolchain install nightly --profile minimal --component rust-src",
};
// Rust code is instrumented: build-std with an explicit target, so
// proc-macros and build scripts are not. The flags apply to these commands
// only, never to the xtask build. Leak detection stays off: it was never
// baselined, and process-lifetime statics would report as leaks.
const ASAN_ENV: &[(&str, &str)] = &[
    ("RUSTFLAGS", "-Zsanitizer=address"),
    ("RUSTDOCFLAGS", "-Zsanitizer=address"),
    ("ASAN_OPTIONS", "detect_leaks=0"),
];
const ASAN_ROOT: Cmd = cargo(&[
    "+nightly",
    "test",
    "-Zbuild-std",
    "--target",
    "x86_64-unknown-linux-gnu",
    "-p",
    "restream",
    "--lib",
    "--",
    "runtime_info",
])
.env(ASAN_ENV);
const ASAN_ENGINE: Cmd = cargo(&[
    "+nightly",
    "test",
    "-Zbuild-std",
    "--target",
    "x86_64-unknown-linux-gnu",
    "-p",
    "restream-engine",
    "--lib",
    "--",
    "egress::tls",
    "egress::backends::compio_tcp",
    "external_transcoder",
    "timing",
    "rtmp::listener",
    "rtmp::tests",
    "tcp_stats",
    "mpegts_allocation_tests",
])
.env(ASAN_ENV);
const ASAN_SYNC: Cmd = cargo(&[
    "+nightly",
    "test",
    "-Zbuild-std",
    "--target",
    "x86_64-unknown-linux-gnu",
    "-p",
    "restream-sync",
    "--lib",
    "--",
    "",
])
.env(ASAN_ENV);
const ASAN_MPEGTS: Cmd = cargo(&[
    "+nightly",
    "test",
    "-Zbuild-std",
    "--target",
    "x86_64-unknown-linux-gnu",
    "-p",
    "restream-mpegts",
    "--lib",
    "--",
    "",
])
.env(ASAN_ENV);
const ASAN_EXTRACTED: Cmd = cargo(&[
    "+nightly",
    "test",
    "-Zbuild-std",
    "--target",
    "x86_64-unknown-linux-gnu",
    "-p",
    "restream-sync",
    "-p",
    "restream-mpegts",
    "--lib",
])
.env(ASAN_ENV);
static ASAN_ROOT_FILTERS: Step = Step::filters("asan-root-filters", ASAN_ROOT).needs(NIGHTLY);
static ASAN_ROOT_RUN: Step = Step::command("asan-root", ASAN_ROOT).needs(NIGHTLY);
static ASAN_ENGINE_FILTERS: Step = Step::filters("asan-engine-filters", ASAN_ENGINE).needs(NIGHTLY);
static ASAN_ENGINE_RUN: Step = Step::command("asan-engine", ASAN_ENGINE).needs(NIGHTLY);
static ASAN_SYNC_FILTERS: Step = Step::filters("asan-sync-filters", ASAN_SYNC).needs(NIGHTLY);
static ASAN_MPEGTS_FILTERS: Step = Step::filters("asan-mpegts-filters", ASAN_MPEGTS).needs(NIGHTLY);
static ASAN_EXTRACTED_RUN: Step = Step::command("asan-extracted", ASAN_EXTRACTED).needs(NIGHTLY);

static FUZZ_SEED_CORPUS: Step = Step::builtin("fuzz-seed-corpus", fuzz::seed_corpus);
static FUZZ_TARGETS: Step = Step::builtin("fuzz-targets", fuzz::smoke).needs(Tool {
    probe: &["cargo", "+nightly", "fuzz", "--version"],
    hint: "rustup toolchain install nightly --profile minimal && cargo install cargo-fuzz --locked",
});

static CARGO_DENY: Step = Step::command(
    "cargo-deny",
    cargo(&["deny", "check", "advisories", "licenses", "bans", "sources"]),
)
.needs(Tool {
    probe: &["cargo", "deny", "--version"],
    hint: "cargo install cargo-deny --locked",
});

static GROUPS: &[Group] = &[
    Group {
        name: "format",
        about: "Rust formatting and TypeScript types",
        steps: &[&FMT, &TSC],
    },
    Group {
        name: "lint",
        about: "clippy and the MCP feature matrix",
        steps: &[
            &CLIPPY_LIB,
            &CLIPPY_WORKSPACE,
            &CLIPPY_MCP,
            &CHECK_MCP_CORE,
            &CHECK_MCP_SERVER,
            &CHECK_MCP_BIN,
            &CHECK_MCP_EMBEDDED,
            &TEST_AGENT_EMBEDDED,
            &AGENT_COMPILED_OUT_FILTERS,
            &CLIPPY_NO_DEFAULT,
            &TEST_AGENT_COMPILED_OUT,
        ],
    },
    Group {
        name: "architecture",
        about: "source, Markdown and dashboard audits",
        steps: &[&SOURCE_AUDIT, &DOCS_AUDIT, &FRONTEND_STYLE],
    },
    Group {
        name: "unit",
        about: "full test run with a quiet log, fixture discipline",
        steps: &[&TEST_HYGIENE, &FIXTURE_DISCIPLINE],
    },
    Group {
        name: "miri",
        about: "Miri over the pure modules it can execute (nightly)",
        steps: &[
            &MIRI_ENGINE_FILTERS,
            &MIRI_ENGINE_RUN,
            &MIRI_CORE_FILTERS,
            &MIRI_CORE_RUN,
        ],
    },
    Group {
        name: "kani",
        about: "bounded proofs of egress primitives",
        steps: &[&KANI],
    },
    Group {
        name: "asan",
        about: "AddressSanitizer over FFI and syscall modules (nightly)",
        steps: &[
            &ASAN_ROOT_FILTERS,
            &ASAN_ROOT_RUN,
            &ASAN_ENGINE_FILTERS,
            &ASAN_ENGINE_RUN,
            &ASAN_SYNC_FILTERS,
            &ASAN_MPEGTS_FILTERS,
            &ASAN_EXTRACTED_RUN,
        ],
    },
    Group {
        name: "fuzz",
        about: "seed the corpus, then every parser target for 30 s",
        steps: &[&FUZZ_SEED_CORPUS, &FUZZ_TARGETS],
    },
    Group {
        name: "deny",
        about: "dependency advisories, licenses, bans and sources",
        steps: &[&CARGO_DENY],
    },
    Group {
        name: "precommit",
        about: "the pre-commit hook",
        steps: &[&FMT, &DOCS_AUDIT, &SOURCE_AUDIT],
    },
];

fn steps() -> impl Iterator<Item = &'static Step> {
    GROUPS.iter().flat_map(|group| group.steps.iter().copied())
}

/// Runs the group or step `args` names; `None` when it names neither.
pub(crate) fn run(args: &[&str]) -> Option<WorkflowResult> {
    let [name] = args else {
        return None;
    };
    if let Some(group) = GROUPS.iter().find(|group| group.name == *name) {
        return Some(run_group(group.steps));
    }
    steps().find(|step| step.name == *name).map(run_step)
}

/// The groups and their steps, for `cargo xtask help`.
pub(crate) fn help() -> String {
    const INDENT: usize = 18;
    let mut text = String::from(
        "checks (a group runs every step, prints a summary and fails if any step\n\
         failed; each step is also a command of its own):\n",
    );
    for group in GROUPS {
        let _ = writeln!(
            text,
            "  {:<width$}{}",
            group.name,
            group.about,
            width = INDENT - 2
        );
        let mut line = String::new();
        for step in group.steps {
            if !line.is_empty() && INDENT + line.len() + 1 + step.name.len() > 78 {
                let _ = writeln!(text, "{:INDENT$}{line}", "");
                line.clear();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(step.name);
        }
        let _ = writeln!(text, "{:INDENT$}{line}", "");
    }
    text
}

/// Runs every step even after a failure, then prints a summary; fails if
/// any step failed. An interruption stops the group at once.
fn run_group(steps: &[&Step]) -> WorkflowResult {
    let mut results = Vec::with_capacity(steps.len());
    for step in steps {
        let started = Instant::now();
        let result = run_step(step);
        if let Err(error) = &result {
            eprintln!("xtask: {}: {error}", step.name);
        }
        results.push((step.name, result.is_ok(), started.elapsed().as_secs_f64()));
        if let Some(signal) = workflow::interrupted() {
            summary(&results);
            return Err(workflow::interruption(signal));
        }
    }
    summary(&results);
    let failed: Vec<&str> = results
        .iter()
        .filter(|(_, passed, _)| !passed)
        .map(|(name, _, _)| *name)
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("failed: {}", failed.join(", ")).into())
    }
}

fn summary(results: &[(&str, bool, f64)]) {
    let width = results
        .iter()
        .map(|(name, _, _)| name.len())
        .chain(["step".len()])
        .max()
        .unwrap_or_default();
    eprintln!();
    eprintln!("{:<width$}  result  seconds", "step");
    for (name, passed, seconds) in results {
        let verdict = if *passed { "pass" } else { "FAIL" };
        eprintln!("{name:<width$}  {verdict:<6}  {seconds:>7.1}");
    }
}

fn run_step(step: &Step) -> WorkflowResult {
    eprintln!("==> {}", step.name);
    if let Some(tool) = step.tool {
        require(tool)?;
    }
    match step.run {
        Run::Command(cmd) => {
            eprintln!("    {cmd}");
            workflow::foreground(&mut cmd.command())
        }
        Run::Filters(cmd) => {
            eprintln!("    require-test-filters {cmd}");
            let args: Vec<&str> = std::iter::once(cmd.program)
                .chain(cmd.args.iter().copied())
                .collect();
            test_filters::require(&crate::repo_root(), &args, cmd.env)
        }
        Run::Builtin(check) => check(),
    }
}

fn require(tool: Tool) -> WorkflowResult {
    let found = Command::new(tool.probe[0])
        .args(&tool.probe[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if found {
        Ok(())
    } else {
        Err(format!(
            "`{}` failed; install with: {}",
            tool.probe.join(" "),
            tool.hint
        )
        .into())
    }
}

/// Runs every Kani proof, then requires all of them verified and every
/// cover reached.
fn kani() -> WorkflowResult {
    // No RUSTFLAGS override: Kani passes an empty one on as an argument.
    let (log, passed) = scan::tee_combined_output(
        Command::new("cargo").args(["kani", "-p", "restream-engine", "--lib", "-Z", "stubbing"]),
        &mut std::io::stdout(),
    )?;
    if !passed {
        return Err("cargo kani failed".into());
    }
    kani_verdict(&log, KANI_PROOFS).map_err(Into::into)
}

/// `proofs` harnesses verified with no failure, and at least one cover
/// summary with every cover satisfied: a proof whose interesting branch
/// became unreachable proves nothing about it, and a changed report format
/// must not pass silently.
fn kani_verdict(log: &str, proofs: usize) -> Result<(), String> {
    let complete =
        format!("Complete - {proofs} successfully verified harnesses, 0 failures, {proofs} total");
    if !log.contains(&complete) {
        return Err(format!("kani did not report `{complete}`"));
    }
    let mut covers = 0;
    for line in log.lines() {
        let Some(summary) = line.strip_prefix(" ** ") else {
            continue;
        };
        let words: Vec<&str> = summary.split_whitespace().collect();
        let [reached, "of", total, "cover", "properties", "satisfied", ..] = words[..] else {
            continue;
        };
        let (Ok(reached), Ok(total)) = (reached.parse::<u32>(), total.parse::<u32>()) else {
            continue;
        };
        if reached != total {
            return Err(format!("kani cover unsatisfied: {}", line.trim()));
        }
        covers += 1;
    }
    if covers == 0 {
        return Err("kani reported no cover summary".into());
    }
    Ok(())
}
