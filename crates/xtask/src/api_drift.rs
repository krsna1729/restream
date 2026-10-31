use crate::workflow::{Error, WorkflowResult};
use regex::Regex;
use std::{fs, path::Path};

const ROOTS: &[&str] = &[
    "web/ts",
    "web/pages",
    "web/assets",
    "src",
    "crates/restream-agent",
    "crates/restream-agent-contract",
    "crates/restream-health",
    "crates/restream-mcp",
    "tests",
    "test",
];
const BANNED: &[&str] = &[
    r#"(["'`])/api/logs(?:/stream)?\b"#,
    r#"(["'`])/api/media(?:/|["'`])"#,
    r#"(["'`])/audio-caps\b"#,
    r#"(["'`])/api/auth/"#,
    r#"(["'`])/config\b"#,
    r#"(["'`])/stream-keys\b"#,
    r#"(["'`])/pipelines(?:/|["'`])"#,
    r#"(["'`])/health(["'`]|[?`])"#,
];
struct Policy {
    banned: Vec<Regex>,
    fetch: Regex,
    request: Regex,
    route: Regex,
}
impl Policy {
    fn new() -> Self {
        Self {
            banned: BANNED
                .iter()
                .map(|p| Regex::new(&p.replace(r"\b", r"(?-u:\b)")).unwrap())
                .collect(),
            fetch: Regex::new(r#"fetch\s*\(\s*(["'`])/api(?-u:\b)"#).unwrap(),
            request: Regex::new(r"(?-u:\b)apiRequest\s*\(").unwrap(),
            route: Regex::new(r#"(["'`])/api/v1/"#).unwrap(),
        }
    }
    fn scan(&self, file: &str, content: &str, violations: &mut Vec<String>) {
        let mut report = |m: regex::Match<'_>, message: String| {
            let line = content[..m.start()].bytes().filter(|b| *b == b'\n').count() + 1;
            violations.push(format!("{file}:{line}: {message}"));
        };
        for pattern in &self.banned {
            if let Some(m) = pattern.find(content) {
                report(
                    m,
                    format!("found banned pre-v1 API path match: {}", m.as_str()),
                );
            }
        }
        if file != "web/assets/login.js"
            && let Some(m) = self.fetch.find(content)
        {
            report(m, "raw fetch('/api...') is only allowed in web/assets/login.js; use web/ts/core/api.ts".into());
        }
        let central = matches!(file, "web/ts/core/api.ts" | "web/ts/core/api-admin.ts");
        if !central {
            if let Some(m) = self.request.find(content) {
                report(
                    m,
                    "apiRequest() should only be called inside web/ts/core/api.ts".into(),
                );
            }
            if file.starts_with("web/ts/")
                && let Some(m) = self.route.find(content)
            {
                report(m, "route literals should live in web/ts/core/api.ts".into());
            }
        }
    }
}
fn walk(root: &Path, path: &Path, policy: &Policy, errors: &mut Vec<String>) -> WorkflowResult {
    if fs::metadata(path)?.is_file() {
        let name = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        policy.scan(&name, &fs::read_to_string(path)?, errors);
    } else {
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let p = entry.path();
            let kind = entry.file_type()?;
            if (kind.is_dir() && !p.ends_with("test/fixtures"))
                || matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("ts" | "tsx" | "js" | "rs" | "html" | "sh" | "mjs")
                )
            {
                walk(root, &p, policy, errors)?;
            }
        }
    }
    Ok(())
}
pub(crate) fn run(_args: &[&str]) -> WorkflowResult {
    let root = std::env::current_dir()?;
    let policy = Policy::new();
    let mut errors = Vec::new();
    for target in ROOTS {
        walk(&root, &root.join(target), &policy, &mut errors)?;
    }
    if !errors.is_empty() {
        return Err(Error::exit(
            1,
            format!(
                "API contract drift guard failed:\n\n- {}",
                errors.join("\n- ")
            ),
        ));
    }
    println!("API drift guard passed.");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_match_per_policy_and_central_allowlist() {
        let policy = Policy::new();
        let mut errors = vec![];
        policy.scan(
            "web/ts/page.ts",
            "\n'/config'\n'/config'\napiRequest('/api/v1/a')",
            &mut errors,
        );
        assert_eq!(errors.len(), 3);
        assert!(errors[0].starts_with("web/ts/page.ts:2:"));
        errors.clear();
        policy.scan(
            "web/ts/core/api-admin.ts",
            "apiRequest('/api/v1/a')",
            &mut errors,
        );
        assert!(errors.is_empty());
        policy.scan("web/assets/login.js", "fetch('/api/v1/a')", &mut errors);
        assert!(errors.is_empty());
    }
}
