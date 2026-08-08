#!/usr/bin/env node

// Guards against the "referenced DOM id was never created anywhere" bug class:
// a getElementById()/querySelector('#...') call surviving a markup rename or a
// never-finished feature, often masked by an `as any` cast around the payload
// it feeds. See docs/agent-guidance/skills (frontend bug sweep) for the
// originating incident: several settings-page save handlers silently no-op'd
// because their ids never matched the rendered markup.

import { promises as fs } from "node:fs";
import path from "node:path";

const repoRoot = process.cwd();

// Ids that are intentionally unresolved by this static scan: known dead code
// pending a product decision (wire up vs delete), tracked separately rather
// than blocking this check.
const knownUnresolvedIds = new Set([
  // web/ts/features/ingest-url-details.ts: renderProtocolDetails is fully
  // built (URL parsing + "Operator Fields" rendering) but has zero production
  // callers - no page ever mounts a container for it. Parked pending a
  // decision to wire it up or delete it.
  "ingest-url-details-heading",
  "ingest-url-details-note",
  // web/ts/features/settings/config-sections.ts populateSrtIngestSettings/
  // saveSrtIngest: the same dangling-id bug this script exists to catch,
  // already fixed on the in-flight codex/srt-buffer-rightsizing branch
  // (PR #112) against a different set of real ids. Drop this entry once that
  // PR merges to master.
  "settings-srt-enabled",
  "settings-srt-port",
  "settings-srt-latency",
  "settings-srt-passphrase",
]);

const scanRoots = ["web/ts", "web/pages"];
const sourceExtensions = new Set([".ts", ".html"]);

const referencedIdPatterns = [
  /getElementById\(\s*["'`]([a-zA-Z0-9_-]+)["'`]\s*\)/g,
  /querySelector(?:All)?\(\s*["'`]#([a-zA-Z0-9_-]+)["'`]\s*\)/g,
];

const createdIdPatterns = [
  // HTML attribute literal: id="foo" (also matches inside .ts template literals)
  /\bid\s*=\s*["']([a-zA-Z0-9_-]+)["']/g,
  // DOM property assignment: el.id = "foo"
  /\.id\s*=\s*["'`]([a-zA-Z0-9_-]+)["'`]/g,
];

async function walk(target) {
  const fullPath = path.join(repoRoot, target);
  const stat = await fs.stat(fullPath);
  if (stat.isFile()) return [target];

  const out = [];
  for (const entry of await fs.readdir(fullPath, { withFileTypes: true })) {
    const relative = path.join(target, entry.name);
    if (entry.isDirectory()) {
      out.push(...(await walk(relative)));
    } else if (sourceExtensions.has(path.extname(entry.name))) {
      out.push(relative);
    }
  }
  return out;
}

function findLineNumber(content, matchIndex) {
  return content.slice(0, matchIndex).split("\n").length;
}

async function main() {
  const files = (
    await Promise.all(scanRoots.map((entry) => walk(entry)))
  ).flat();

  const referenced = new Map(); // id -> [{file, line}]
  const created = new Set();

  for (const file of files) {
    const content = await fs.readFile(path.join(repoRoot, file), "utf8");

    if (file.endsWith(".ts")) {
      for (const pattern of referencedIdPatterns) {
        pattern.lastIndex = 0;
        let match;
        while ((match = pattern.exec(content))) {
          const id = match[1];
          const line = findLineNumber(content, match.index);
          if (!referenced.has(id)) referenced.set(id, []);
          referenced.get(id).push(`${file}:${line}`);
        }
      }
    }

    for (const pattern of createdIdPatterns) {
      pattern.lastIndex = 0;
      let match;
      while ((match = pattern.exec(content))) {
        created.add(match[1]);
      }
    }
  }

  const violations = [];
  for (const [id, locations] of referenced) {
    if (created.has(id) || knownUnresolvedIds.has(id)) continue;
    for (const location of locations) {
      violations.push(
        `${location}: references id "${id}", but no "${id}" is ever created in web/ts or web/pages`,
      );
    }
  }
  violations.sort();

  if (violations.length > 0) {
    console.error("DOM id drift guard failed:\n");
    for (const violation of violations) {
      console.error(`- ${violation}`);
    }
    console.error(
      "\nEach id above is read via getElementById()/querySelector() but never appears in any id=\"...\" markup or .id = \"...\" assignment. Fix the id, or add it to knownUnresolvedIds in this script with a reason if it's an intentionally-parked case.",
    );
    process.exit(1);
  }

  console.log(
    `DOM id drift guard passed (${referenced.size} referenced ids, ${created.size} created ids).`,
  );
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
