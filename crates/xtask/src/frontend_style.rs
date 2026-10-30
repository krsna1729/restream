use crate::workflow::{Error, WorkflowResult};
use regex::Regex;
use std::{fs, path::Path};

const RULES: &[(&str, &str)] = &[
    (
        r"\bcard bg-base-200\b|\bcard-body\b|\bcard-title\b",
        "use Panel from ui/panel.tsx",
    ),
    (
        r"\b(select|input|textarea|file-input)-bordered\b",
        "DaisyUI 4 class, a no-op in DaisyUI 5: drop it",
    ),
    (
        r#"\bbadge(-sm|-xs)?\b[^"`]*\bbadge-(success|warning|error|info)\b|\bbadge-(success|warning|error|info)\b[^"`]*\bbadge\b"#,
        "a status is a StatusBadge from ui/tone.tsx",
    ),
    (
        r"-mb-px border-b-2",
        "tabs under a header use the tab-strip classes",
    ),
    (
        r"\bform-control\b|\blabel-text\b",
        "a labelled control is a Field from ui/controls.tsx",
    ),
    (
        r"new Date\([^)]*\)\.toLocale(String|DateString|TimeString)\(",
        "a time is <When> or absoluteTime() from when.ts: one format, with its time zone",
    ),
];
fn scan_text(name: &str, text: &str, rules: &[(Regex, &str)], errors: &mut Vec<String>) {
    let lines: Vec<_> = text.split('\n').collect();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 && lines[i - 1].contains("style-ok:") {
            continue;
        }
        for (rule, fix) in rules {
            if rule.is_match(line) {
                let excerpt: String = line.trim().chars().take(120).collect();
                errors.push(format!("{name}:{}: {fix}\n    {excerpt}", i + 1));
            }
        }
    }
}
fn walk(
    root: &Path,
    dir: &Path,
    rules: &[(Regex, &str)],
    errors: &mut Vec<String>,
) -> WorkflowResult {
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if fs::metadata(&path)?.is_dir() {
            if entry.file_name() != "generated" {
                walk(root, &path, rules, errors)?;
            }
        } else if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("ts" | "tsx")
        ) {
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if !name.starts_with("web/ts/ui/") {
                scan_text(
                    &name,
                    &String::from_utf8_lossy(&fs::read(&path)?),
                    rules,
                    errors,
                );
            }
        }
    }
    Ok(())
}
pub(crate) fn run() -> WorkflowResult {
    let root = crate::repo_root();
    let rules: Vec<_> = RULES
        .iter()
        .map(|(p, m)| (Regex::new(&p.replace(r"\b", r"(?-u:\b)")).unwrap(), *m))
        .collect();
    let mut errors = Vec::new();
    walk(&root, &root.join("web/ts"), &rules, &mut errors)?;
    if !errors.is_empty() {
        return Err(Error::exit(
            1,
            format!(
                "Frontend style check failed ({}):\n{}",
                errors.len(),
                errors.join("\n")
            ),
        ));
    }
    println!("Frontend style check passed.");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exemptions_apply_only_to_following_line() {
        let rules: Vec<_> = RULES
            .iter()
            .map(|(p, m)| (Regex::new(p).unwrap(), *m))
            .collect();
        let mut errors = vec![];
        scan_text(
            "page.tsx",
            "// style-ok: fixture exception\ncard-body\ncard-body",
            &rules,
            &mut errors,
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("page.tsx:3:"));
    }
    #[test]
    fn generated_and_primitive_directories_are_exempt() {
        let temp = crate::workflow::TempDir::new("restream-style-audit").unwrap();
        let root = temp.path();
        for dir in ["web/ts/generated", "web/ts/ui", "web/ts/pages"] {
            fs::create_dir_all(root.join(dir)).unwrap();
            fs::write(root.join(dir).join("example.tsx"), "card-body").unwrap();
        }
        let rules: Vec<_> = RULES
            .iter()
            .map(|(p, m)| (Regex::new(p).unwrap(), *m))
            .collect();
        let mut errors = vec![];
        walk(root, &root.join("web/ts"), &rules, &mut errors).unwrap();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("web/ts/pages/example.tsx:1:"));
    }
}
