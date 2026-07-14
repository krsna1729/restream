import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const root = process.env.RESTREAM_REPO_ROOT
  ? path.resolve(process.env.RESTREAM_REPO_ROOT)
  : execFileSync("git", ["rev-parse", "--show-toplevel"], {
      encoding: "utf8",
    }).trim();

process.chdir(root);

function relativePath(filename) {
  return path.relative(root, filename).split(path.sep).join("/");
}

function collectMarkdown(directory, files) {
  if (!fs.existsSync(directory)) return;
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const filename = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      collectMarkdown(filename, files);
    } else if (entry.isFile() && entry.name.endsWith(".md")) {
      files.add(path.resolve(filename));
    }
  }
}

function markdownFiles() {
  const tracked = execFileSync("git", ["ls-files", "*.md"], {
    encoding: "utf8",
  })
    .split(/\r?\n/)
    .filter(Boolean);
  const files = new Set(
    tracked.map((filename) => path.resolve(root, filename)),
  );

  // Include new documentation before it is staged.
  collectMarkdown(path.join(root, "docs"), files);
  return [...files].filter(fs.existsSync).sort();
}

function proseHeadings(lines) {
  const headings = [];
  let inFence = false;
  lines.forEach((line, index) => {
    if (/^\s*```/.test(line)) {
      inFence = !inFence;
      return;
    }
    if (inFence) return;
    const match = /^(#{1,6}) (.+?)\s*$/.exec(line);
    if (match) {
      headings.push({
        line: index + 1,
        level: match[1].length,
        text: match[2],
      });
    }
  });
  return headings;
}

const maintainedProse = new Set([
  "README.md",
  "docs/README.md",
  "docs/development.md",
  "docs/architecture.md",
  "docs/media-pipeline.md",
  "docs/high-performance-data-path.md",
  "docs/concurrency-proofing.md",
  "docs/configuration.md",
  "docs/api-reference.md",
  "docs/observability.md",
  "docs/logging.md",
  "docs/current-priorities.md",
  "docs/layering-roadmap.md",
  "docs/testing-strategy.md",
  "docs/testing.md",
  "docs/agent-plane-integration.md",
  "docs/mcp-rust-architecture.md",
  "docs/source-distribution.md",
  "docs/release-compliance.md",
  "docs/release-runbook.md",
  "docs/ffmpeg-versions.md",
]);

const volatileCount =
  /\b\d[\d,]*\s+(?:source\s+)?(?:lines?|routes?|tests?|assertions?|benchmarks?|modules?|callsites?)\b/i;
const highChurnHeadings = new Set([
  "## Callsite Audit",
  "Available suites include:",
]);
const linkPattern = /\[([^\]]+)\]\(([^)]+)\)/g;
const files = markdownFiles();
const errors = [];

for (const filename of files) {
  const relative = relativePath(filename);
  const text = fs.readFileSync(filename, "utf8");
  const lines = text.split(/\r?\n/);
  const headings = proseHeadings(lines);
  const h1 = headings.filter(({ level }) => level === 1);
  const h2 = headings.filter(({ level }) => level === 2);

  // Skill packages optimize for immediate execution, so a TOC is needless
  // preamble. Legal text and one-section shims also need no navigation.
  const requiresContents = h2.length > 0 && path.basename(filename) !== "SKILL.md";
  if (requiresContents) {
    if (h1.length !== 1) {
      errors.push(`${relative}: expected one H1, found ${h1.length}`);
    }
    const contents = h2.filter(({ text: heading }) => heading === "Contents");
    if (contents.length !== 1) {
      errors.push(`${relative}: expected one H2 Contents section`);
    } else {
      const contentsLine = contents[0].line;
      const laterH2 = h2
        .filter(({ line }) => line > contentsLine)
        .map(({ line }) => line);
      const tocEnd = laterH2.length > 0 ? Math.min(...laterH2) : lines.length + 1;
      const toc = lines.slice(contentsLine, tocEnd - 1).join("\n");
      for (const { text: heading } of h2) {
        if (heading === "Contents") continue;
        const label = heading.replace(/`([^`]*)`/g, "$1");
        if (!toc.includes(`[${label}](#`)) {
          errors.push(`${relative}: TOC does not include H2 ${JSON.stringify(heading)}`);
        }
      }
    }
  }

  lines.forEach((line, index) => {
    const lineNumber = index + 1;
    if (maintainedProse.has(relative) && volatileCount.test(line)) {
      errors.push(
        `${relative}:${lineNumber}: volatile count belongs in generated or dated evidence`,
      );
    }
    if (maintainedProse.has(relative) && highChurnHeadings.has(line)) {
      errors.push(
        `${relative}:${lineNumber}: high-churn inventory belongs in source or dated evidence`,
      );
    }

    for (const match of line.matchAll(linkPattern)) {
      const [, label, target] = match;
      const linkTarget = target.split("#", 1)[0];
      if (
        !linkTarget ||
        linkTarget.includes("://") ||
        linkTarget.startsWith("mailto:") ||
        linkTarget.startsWith("/")
      ) {
        continue;
      }
      const destination = path.resolve(path.dirname(filename), linkTarget);
      if (!fs.existsSync(destination)) {
        errors.push(
          `${relative}:${lineNumber}: broken relative link [${label}](${linkTarget})`,
        );
      }
    }
  });
}

// The central index must reach every Markdown file except itself.
const indexRelative = "docs/README.md";
const index = path.join(root, indexRelative);
if (fs.existsSync(index)) {
  const linked = new Set();
  const text = fs.readFileSync(index, "utf8");
  for (const match of text.matchAll(linkPattern)) {
    const target = match[2].split("#", 1)[0];
    if (target.endsWith(".md")) {
      linked.add(path.resolve(path.dirname(index), target));
    }
  }
  for (const filename of files) {
    if (relativePath(filename) === indexRelative) continue;
    if (!linked.has(path.resolve(filename))) {
      errors.push(`${relativePath(filename)}: not linked from docs/README.md`);
    }
  }
}

if (errors.length > 0) {
  process.stderr.write("Documentation checks failed:\n");
  for (const error of errors) process.stderr.write(`- ${error}\n`);
  process.exit(1);
}

process.stdout.write(
  `Documentation checks passed for ${files.length} Markdown files.\n`,
);
