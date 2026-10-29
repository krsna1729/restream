use crate::workflow::{Error, WorkflowResult};
use regex::Regex;
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
fn collect(
    directory: &Path,
    extension: &str,
    insensitive: bool,
    files: &mut BTreeSet<PathBuf>,
) -> WorkflowResult {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(&entry.path(), extension, insensitive, files)?;
        } else if kind.is_file() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if if insensitive {
                name.to_lowercase().ends_with(extension)
            } else {
                name.ends_with(extension)
            } {
                files.insert(entry.path());
            }
        }
    }
    Ok(())
}
struct Policy {
    heading: Regex,
    fence: Regex,
    links: Regex,
    glyphs: Regex,
    inline_code: Regex,
}
impl Policy {
    fn new() -> Self {
        Self {
            heading: Regex::new(r"^(#{1,6}) (.+?)\s*$").unwrap(),
            fence: Regex::new(r"^\s*```([^\s`]*)").unwrap(),
            links: Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").unwrap(),
            glyphs: Regex::new(r"[┌┐┘┬┴┼▼▲►◄║╔╗╚╝]").unwrap(),
            inline_code: Regex::new(r"`([^`]*)`").unwrap(),
        }
    }
    fn scan(&self, filename: &Path, relative: &str, text: &str, errors: &mut Vec<String>) {
        let lines: Vec<_> = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();
        let mut headings = Vec::new();
        let mut fenced = false;
        for (i, line) in lines.iter().enumerate() {
            if self.fence.is_match(line) {
                fenced = !fenced;
                continue;
            }
            if !fenced && let Some(m) = self.heading.captures(line) {
                headings.push((i + 1, m[1].len(), m[2].to_owned()));
            }
        }
        let h2: Vec<_> = headings
            .iter()
            .filter(|(_, level, _)| *level == 2)
            .collect();
        if !h2.is_empty() && filename.file_name().is_none_or(|n| n != "SKILL.md") {
            let h1 = headings.iter().filter(|(_, level, _)| *level == 1).count();
            if h1 != 1 {
                errors.push(format!("{relative}: expected one H1, found {h1}"));
            }
            let contents: Vec<_> = h2
                .iter()
                .filter(|(_, _, text)| text == "Contents")
                .collect();
            if contents.len() != 1 {
                errors.push(format!("{relative}: expected one H2 Contents section"));
            } else {
                let start = contents[0].0;
                let end = h2
                    .iter()
                    .filter(|(line, _, _)| *line > start)
                    .map(|(line, _, _)| *line)
                    .min()
                    .unwrap_or(lines.len() + 1);
                let toc = lines[start..end - 1].join("\n");
                for (_, _, heading) in &h2 {
                    if heading == "Contents" {
                        continue;
                    }
                    let label = self.inline_code.replace_all(heading, "$1");
                    if !toc.contains(&format!("[{label}](#")) {
                        errors.push(format!(
                            "{relative}: TOC does not include H2 {}",
                            serde_json::to_string(heading).unwrap()
                        ));
                    }
                }
            }
        }
        let mut language: Option<String> = None;
        for (i, line) in lines.iter().enumerate() {
            let number = i + 1;
            if let Some(fence) = self.fence.captures(line) {
                language = if language.is_none() {
                    Some(fence[1].to_lowercase())
                } else {
                    None
                };
                if language.as_deref() == Some("mermaid")
                    && !matches!(
                        lines.get(i + 1).copied().unwrap_or(""),
                        "flowchart LR" | "flowchart TD" | "sequenceDiagram" | "stateDiagram-v2"
                    )
                {
                    errors.push(format!("{relative}:{number}: Mermaid diagrams must start with flowchart LR, flowchart TD, sequenceDiagram or stateDiagram-v2"));
                }
                continue;
            }
            if language.as_deref() != Some("mermaid") && self.glyphs.is_match(line) {
                errors.push(format!(
                    "{relative}:{number}: conceptual diagram should use a Mermaid fence"
                ));
            }
            for m in self.links.captures_iter(line) {
                let target = m[2].split('#').next().unwrap();
                if target.to_lowercase().ends_with(".svg") {
                    errors.push(format!("{relative}:{number}: link documentation diagrams as Mermaid source, not SVG"));
                }
                if target.is_empty()
                    || target.contains("://")
                    || target.starts_with("mailto:")
                    || target.starts_with('/')
                {
                    continue;
                }
                if !filename.parent().unwrap().join(target).exists() {
                    errors.push(format!(
                        "{relative}:{number}: broken relative link [{}]({target})",
                        &m[1]
                    ));
                }
            }
        }
    }
}
pub(crate) fn run(_args: &[&str]) -> WorkflowResult {
    let root = normalize(&crate::repo_root());
    let output = crate::workflow::output(
        Command::new("git")
            .current_dir(&root)
            .args(["ls-files", "*.md"]),
    )?;
    let mut files: BTreeSet<_> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|s| !s.is_empty())
        .map(|name| normalize(&root.join(name)))
        .collect();
    collect(&root.join("docs"), ".md", false, &mut files)?;
    files.retain(|file| {
        file.exists()
            && !file
                .strip_prefix(&root)
                .unwrap_or(file)
                .starts_with("vendor")
    });
    let mut svgs = BTreeSet::new();
    collect(&root.join("docs"), ".svg", true, &mut svgs)?;
    let relative = |file: &Path| {
        file.strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let mut errors: Vec<_> = svgs
        .iter()
        .map(|file| {
            format!(
                "{}: documentation diagrams must use Mermaid, not checked-in SVG",
                relative(file)
            )
        })
        .collect();
    let policy = Policy::new();
    for file in &files {
        policy.scan(
            file,
            &relative(file),
            &String::from_utf8_lossy(&fs::read(file)?),
            &mut errors,
        );
    }
    let index = root.join("docs/README.md");
    if index.exists() {
        let text = fs::read_to_string(&index)?;
        let linked: BTreeSet<_> = policy
            .links
            .captures_iter(&text)
            .filter_map(|m| {
                let target = m[2].split('#').next().unwrap();
                target
                    .ends_with(".md")
                    .then(|| normalize(&index.parent().unwrap().join(target)))
            })
            .collect();
        for file in &files {
            if file != &index && !linked.contains(file) {
                errors.push(format!(
                    "{}: not linked from docs/README.md",
                    relative(file)
                ));
            }
        }
    }
    if !errors.is_empty() {
        return Err(Error::exit(
            1,
            format!("Documentation checks failed:\n- {}", errors.join("\n- ")),
        ));
    }
    println!(
        "Documentation checks passed for {} Markdown files.",
        files.len()
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fenced_headings_do_not_require_contents_but_glyphs_still_fail() {
        let mut errors = vec![];
        Policy::new().scan(
            Path::new("/tmp/page.md"),
            "page.md",
            "```text\n## hidden\n┌\n```",
            &mut errors,
        );
        assert_eq!(
            errors,
            ["page.md:3: conceptual diagram should use a Mermaid fence"]
        );
    }
    #[test]
    fn toc_removes_inline_code_and_mermaid_requires_exact_start() {
        let mut errors = vec![];
        Policy::new().scan(
            Path::new("/tmp/page.md"),
            "page.md",
            "# Title\n## Contents\n[API](#api)\n## `API`\n```mermaid\n flowchart LR\n```",
            &mut errors,
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains(":5: Mermaid"));
    }
    #[test]
    fn relative_links_resolve_against_document_parent() {
        let dir = std::env::temp_dir().join(format!("restream-docs-audit-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("target.md"), "ok").unwrap();
        let mut errors = vec![];
        Policy::new().scan(
            &dir.join("page.md"),
            "page.md",
            "[ok](target.md#anchor) [bad](missing.md)",
            &mut errors,
        );
        assert_eq!(
            errors,
            ["page.md:1: broken relative link [bad](missing.md)"]
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
