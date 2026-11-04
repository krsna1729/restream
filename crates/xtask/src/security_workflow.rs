//! Execute the scheduled scanner workflow's Bash, not a second reporting policy.

use std::path::Path;
use std::process::{Command, Output};

fn step(name: &str) -> String {
    let workflow =
        std::fs::read_to_string(crate::repo_root().join(".github/workflows/sbom-security.yml"))
            .expect("read security workflow");
    let header = format!("      - name: {name}");
    let mut lines = workflow.lines().skip_while(|line| *line != header);
    assert!(lines.next().is_some(), "missing step: {name}");
    assert!(
        lines.any(|line| line == "        run: |"),
        "missing Bash: {name}"
    );
    lines
        .take_while(|line| line.is_empty() || line.starts_with("          "))
        .map(|line| line.strip_prefix("          ").unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn run(script: &str, directory: &Path, prelude: &str) -> Output {
    Command::new("bash")
        .args([
            "-e",
            "-o",
            "pipefail",
            "-c",
            &format!("{prelude}\n{script}"),
        ])
        .current_dir(directory)
        .output()
        .expect("execute security workflow Bash")
}

#[test]
fn sbom_scanner_execution_errors_fail_the_step() {
    let temp = crate::workflow::TempDir::new("sbom-scanner-errors").unwrap();
    for scanner in ["grype", "trivy"] {
        let output = run(
            &step(&format!("Scan SBOM with {scanner}")),
            temp.path(),
            &format!("{scanner}() {{ return 42; }}"),
        );
        assert_eq!(
            output.status.code(),
            Some(42),
            "{scanner}: preserve failure"
        );
    }
}

#[test]
fn sbom_incomplete_or_malformed_reports_never_count_as_a_clean_scan() {
    let temp = crate::workflow::TempDir::new("sbom-reports").unwrap();
    let script = step("Validate scanner reports");
    let json = temp.path().join("grype-results.json");
    let grype = temp.path().join("grype-results.sarif");
    let trivy = temp.path().join("trivy-results.sarif");
    let sarif = r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"scanner"}}}]}"#;
    assert!(
        !run(&script, temp.path(), "").status.success(),
        "missing reports"
    );
    std::fs::write(&json, r#"{"matches":[]}"#).unwrap();
    std::fs::write(&grype, sarif).unwrap();
    std::fs::write(&trivy, sarif).unwrap();
    assert!(
        run(&script, temp.path(), "").status.success(),
        "complete clean reports"
    );
    for broken in ["{", "{}", r#"{"matches":null}"#, r#"{"matches":"unknown"}"#] {
        std::fs::write(&json, broken).unwrap();
        assert!(
            !run(&script, temp.path(), "").status.success(),
            "bad inventory: {broken}"
        );
    }
    std::fs::write(&json, r#"{"matches":[]}"#).unwrap();
    for file in [&grype, &trivy] {
        for broken in ["", "{}", r#"{"version":"2.1.0","runs":[]}"#] {
            std::fs::write(file, broken).unwrap();
            assert!(
                !run(&script, temp.path(), "").status.success(),
                "bad SARIF: {broken}"
            );
        }
        std::fs::write(file, sarif).unwrap();
    }
}

#[test]
fn sbom_only_high_or_critical_findings_request_an_issue() {
    let temp = crate::workflow::TempDir::new("sbom-severity").unwrap();
    let script = step("File issue for scheduled HIGH/CRITICAL findings");
    // A remote write is never allowed in this test. Its refusal proves whether
    // the real severity policy attempted notification, without echoing a mock API.
    let refuse_remote = "gh() { return 42; }";
    for (severity, exit) in [("LOW", 0), ("MEDIUM", 0), ("high", 42), ("CRITICAL", 42)] {
        let report = serde_json::json!({"matches": [{
            "vulnerability": {"id": "test-advisory", "severity": severity},
            "artifact": {"name": "test-package", "version": "1"}
        }]});
        std::fs::write(temp.path().join("grype-results.json"), report.to_string()).unwrap();
        let output = run(&script, temp.path(), refuse_remote);
        assert_eq!(output.status.code(), Some(exit), "severity: {severity}");
    }
}
