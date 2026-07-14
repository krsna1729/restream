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

function collectByExtension(directory, extension, files) {
  if (!fs.existsSync(directory)) return;
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const filename = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      collectByExtension(filename, extension, files);
    } else if (
      entry.isFile() &&
      entry.name.toLowerCase().endsWith(extension.toLowerCase())
    ) {
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

const linkPattern = /\[([^\]]+)\]\(([^)]+)\)/g;
const conceptualDiagramGlyphs = /[┌┐┘┬┴┼▼▲►◄║╔╗╚╝]/;
const files = markdownFiles();
const errors = [];

// Keep this gate mechanical. Semantic drift belongs in focused contract tests
// and review, not in terminology blacklists or duplicated source inventories.

const svgFiles = new Set();
collectByExtension(path.join(root, "docs"), ".svg", svgFiles);
for (const filename of svgFiles) {
  errors.push(
    `${relativePath(filename)}: documentation diagrams must use Mermaid, not checked-in SVG`,
  );
}

for (const filename of files) {
  const relative = relativePath(filename);
  const text = fs.readFileSync(filename, "utf8");
  const lines = text.split(/\r?\n/);
  const headings = proseHeadings(lines);
  const h1 = headings.filter(({ level }) => level === 1);
  const h2 = headings.filter(({ level }) => level === 2);
  let fenceLanguage = null;

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
    const fence = /^\s*```([^\s`]*)/.exec(line);
    if (fence) {
      fenceLanguage = fenceLanguage === null ? fence[1].toLowerCase() : null;
      if (
        fenceLanguage === "mermaid" &&
        !/^flowchart (?:LR|TD)$/.test(lines[index + 1] ?? "")
      ) {
        errors.push(
          `${relative}:${lineNumber}: Mermaid diagrams must start with flowchart LR or flowchart TD`,
        );
      }
      return;
    }
    if (fenceLanguage !== "mermaid" && conceptualDiagramGlyphs.test(line)) {
      errors.push(
        `${relative}:${lineNumber}: conceptual diagram should use a Mermaid fence`,
      );
    }
    for (const match of line.matchAll(linkPattern)) {
      const [, label, target] = match;
      const linkTarget = target.split("#", 1)[0];
      if (/\.svg$/i.test(linkTarget)) {
        errors.push(
          `${relative}:${lineNumber}: link documentation diagrams as Mermaid source, not SVG`,
        );
      }
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
