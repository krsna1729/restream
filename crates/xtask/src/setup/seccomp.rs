use crate::workflow::{Error, WorkflowResult, checked};
use serde_json::{Value, json};
use std::{fs, process::Command};

fn transform(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    if String::from_utf8_lossy(bytes).contains("io_uring") {
        return Err("the baseline already mentions io_uring: drop the delta instead of adding a redundant rule".into());
    }
    let mut profile: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    profile.get_mut("syscalls").and_then(Value::as_array_mut)
        .ok_or_else(|| "seccomp baseline has no syscalls array".to_string())?
        .push(json!({"names": ["io_uring_enter", "io_uring_register", "io_uring_setup"], "action": "SCMP_ACT_ALLOW"}));
    let pretty = serde_json::to_string_pretty(&profile).map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    for line in pretty.lines() {
        let spaces = line.bytes().take_while(|byte| *byte == b' ').count();
        result.extend(std::iter::repeat_n(b'\t', spaces / 2));
        result.extend_from_slice(&line.as_bytes()[spaces..]);
        result.push(b'\n');
    }
    Ok(result)
}

pub(super) fn run(args: &[&str]) -> WorkflowResult {
    if args
        .first()
        .is_some_and(|arg| matches!(*arg, "-h" | "--help"))
    {
        println!("usage: xtask setup refresh-docker-seccomp <moby-tag, e.g. docker-v29.8.1>");
        return Ok(());
    }
    let tag = args.first().filter(|tag| !tag.is_empty()).ok_or_else(|| {
        Error::usage("usage: xtask setup refresh-docker-seccomp <moby-tag, e.g. docker-v29.8.1>")
    })?;
    let temp = crate::workflow::TempDir::new("restream-xtask")?;
    let baseline = temp.path().join("default.json");
    let url = format!(
        "https://raw.githubusercontent.com/moby/moby/{tag}/vendor/github.com/moby/profiles/seccomp/default.json"
    );
    checked(
        Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                &url,
                "-o",
            ])
            .arg(&baseline),
    )?;
    let sha = super::sha256(&baseline)?;
    let bytes = fs::read(&baseline)?;
    let transformed = transform(&bytes)?;
    let out = crate::repo_root().join("distribution/docker");
    fs::write(out.join(format!("moby-default-seccomp-{tag}.json")), bytes)?;
    fs::write(out.join("restream-seccomp.json"), transformed)?;
    println!("refresh-docker-seccomp: baseline {tag} sha256={sha}");
    println!("refresh-docker-seccomp: update distribution/docker/README.md provenance, the");
    println!(
        "refresh-docker-seccomp: BASE path in tests/seccomp_profile.rs, then run the container smoke"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn baseline_preserved_and_only_delta_added() {
        let source = json!({"defaultAction":"SCMP_ACT_ERRNO", "syscalls":[{"names":["read"],"action":"SCMP_ACT_ALLOW"}]});
        let result: Value =
            serde_json::from_slice(&transform(&serde_json::to_vec(&source).unwrap()).unwrap())
                .unwrap();
        assert_eq!(result["defaultAction"], source["defaultAction"]);
        assert_eq!(result["syscalls"][0], source["syscalls"][0]);
        assert_eq!(
            result["syscalls"][1]["names"],
            json!(["io_uring_enter", "io_uring_register", "io_uring_setup"])
        );
        assert_eq!(result["syscalls"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn reject_redundant_or_malformed_baselines() {
        assert!(transform(br#"{"syscalls":[],"note":"io_uring"}"#).is_err());
        assert!(transform(br#"{"syscalls":{}}"#).is_err());
        assert!(transform(b"not json").is_err());
    }
}
