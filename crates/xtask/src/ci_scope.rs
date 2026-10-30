//! Tests for the changed-path scope steps in `.github/workflows/ci.yml`.
//! Each test extracts a job's real Bash scope script and runs it with only
//! Git substituted, so the predicates under test are the ones CI executes.

use std::process::Command;

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
            ("crates/xtask/src/fuzz.rs", false, true, false),
            ("crates/xtask/src/fuzz/seeds.rs", false, true, false),
            ("crates/xtask/src/checks.rs", false, true, false),
            ("crates/xtask/src/fetch.rs", false, false, true),
            ("crates/xtask/src/fetch/moxygen.rs", false, false, true),
            ("crates/xtask/src/container/smoke.rs", true, false, false),
            ("crates/xtask/src/release.rs", true, false, false),
            ("crates/xtask/src/qualify/evidence.rs", true, false, false),
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
            "crates/xtask/src/ci_scope.rs",
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
