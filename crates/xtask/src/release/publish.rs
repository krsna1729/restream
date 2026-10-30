use std::process::{Command, Stdio};

use super::common::{git, require_clean_checkout, require_commands, require_tag};
use crate::workflow::{Error, WorkflowResult, checked, output, spawn, wait};
use serde_json::Value;

const TAG_USAGE: &str = "Usage: cargo xtask release tag-and-publish <vX.Y.Z> <successful-dry-run-id>\n\nThe dry-run must be a completed successful Release workflow_dispatch run whose head SHA matches this checkout. Pushing the tag triggers GitHub Release assets and GHCR tag/latest publication.";

pub(super) fn tag_and_publish(args: &[&str]) -> WorkflowResult {
    if matches!(args.first(), Some(&"-h" | &"--help")) {
        println!("{TAG_USAGE}");
        return Ok(());
    }
    let tag = args.first().copied().unwrap_or_default();
    let run_id = args.get(1).copied().unwrap_or_default();
    if tag.is_empty() || run_id.is_empty() {
        return Err(Error::usage(TAG_USAGE));
    }
    require_tag("tag-and-publish", tag)?;
    require_commands(&["gh", "git"])?;
    require_clean_checkout("tag-and-publish")?;
    let head_sha = git(&["rev-parse", "HEAD"])?;
    let run_json = output(Command::new("gh").args([
        "run",
        "view",
        run_id,
        "--json",
        "status,conclusion,headSha,workflowName,event,url,jobs",
    ]))?;
    let run: Value = serde_json::from_slice(&run_json.stdout)
        .map_err(|error| format!("tag-and-publish: invalid workflow run JSON: {error}"))?;
    validate_run(&run, run_id, &head_sha)?;
    let tag_ref = format!("refs/tags/{tag}");
    if succeeds(Command::new("git").args(["rev-parse", "-q", "--verify", &tag_ref]))? {
        return Err(format!("tag-and-publish: local tag already exists: {tag}").into());
    }
    if succeeds(Command::new("git").args([
        "ls-remote",
        "--exit-code",
        "--tags",
        "origin",
        &tag_ref,
    ]))? {
        return Err(format!("tag-and-publish: remote tag already exists: {tag}").into());
    }
    checked(Command::new("git").args(["tag", "-a", tag, "-m", &format!("Release {tag}")]))?;
    checked(Command::new("git").args(["push", "origin", &tag_ref]))?;
    println!("tag-and-publish: pushed {tag} at {head_sha}");
    println!(
        "tag-and-publish: monitor with: gh run list --workflow release.yml --branch {tag} --limit 1"
    );
    Ok(())
}

fn succeeds(command: &mut Command) -> Result<bool, Error> {
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = spawn(command)?;
    Ok(wait(&mut child)?.success())
}

fn string<'a>(run: &'a Value, key: &str) -> &'a str {
    run.get(key).and_then(Value::as_str).unwrap_or("null")
}

fn validate_run(run: &Value, run_id: &str, head_sha: &str) -> WorkflowResult {
    let workflow = string(run, "workflowName");
    let event = string(run, "event");
    let status = string(run, "status");
    let conclusion = string(run, "conclusion");
    let run_sha = string(run, "headSha");
    let url = string(run, "url");
    if workflow != "Release" {
        return Err(format!(
            "tag-and-publish: run {run_id} is workflow '{workflow}', expected Release"
        )
        .into());
    }
    if event != "workflow_dispatch" {
        return Err(format!(
            "tag-and-publish: run {run_id} event is '{event}', expected workflow_dispatch dry-run"
        )
        .into());
    }
    if status != "completed" || conclusion != "success" {
        return Err(format!("tag-and-publish: dry-run is not green: status={status} conclusion={conclusion} url={url}").into());
    }
    for required_job in [
        "Release package and evidence",
        "Release dry-run certification",
    ] {
        let job_conclusion = run
            .get("jobs")
            .and_then(Value::as_array)
            .and_then(|jobs| {
                jobs.iter()
                    .find(|job| job.get("name").and_then(Value::as_str) == Some(required_job))
            })
            .and_then(|job| job.get("conclusion"))
            .and_then(Value::as_str)
            .unwrap_or("missing");
        if job_conclusion != "success" {
            return Err(format!("tag-and-publish: required release job '{required_job}' was '{job_conclusion}', expected success: {url}").into());
        }
    }
    if run_sha != head_sha {
        return Err(format!(
            "tag-and-publish: dry-run SHA {run_sha} does not match HEAD {head_sha}"
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn certified() -> Value {
        json!({"workflowName":"Release", "event":"workflow_dispatch", "status":"completed",
            "conclusion":"success", "headSha":"abc", "url":"https://example.invalid/run/1",
            "jobs":[{"name":"Release package and evidence", "conclusion":"success"},
                {"name":"Release dry-run certification", "conclusion":"success"}]})
    }

    #[test]
    fn publication_requires_matching_green_dispatch_and_both_certification_jobs() {
        assert!(validate_run(&certified(), "1", "abc").is_ok());
        assert!(validate_run(&certified(), "1", "other").is_err());
        for (key, value) in [
            ("workflowName", "Other"),
            ("event", "push"),
            ("status", "in_progress"),
            ("conclusion", "failure"),
        ] {
            let mut run = certified();
            run[key] = json!(value);
            assert!(validate_run(&run, "1", "abc").is_err());
        }
        for index in 0..2 {
            let mut run = certified();
            run["jobs"][index]["conclusion"] = json!("skipped");
            assert!(validate_run(&run, "1", "abc").is_err());
        }
        let mut run = certified();
        run["jobs"] = json!([]);
        assert!(validate_run(&run, "1", "abc").is_err());
    }
}
