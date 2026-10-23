#!/usr/bin/env node
// Keeps the control room built from its shared primitives (web/ts/ui/):
// fails on markup that pages hand-rolled before them, which is where the
// visual drift (mismatched cards, misaligned badges, no-op classes) came
// from. The primitives themselves (web/ts/ui/) may use the raw classes.
//
// A line that must keep a pattern says why on the line before:
//   // style-ok: <reason>

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const root = new URL("../..", import.meta.url).pathname;
const RULES = [
  { pattern: /\bcard bg-base-200\b|\bcard-body\b|\bcard-title\b/, fix: "use Panel from ui/panel.tsx" },
  { pattern: /\b(select|input|textarea|file-input)-bordered\b/, fix: "DaisyUI 4 class, a no-op in DaisyUI 5: drop it" },
  {
    pattern: /\bbadge(-sm|-xs)?\b[^"`]*\bbadge-(success|warning|error|info)\b|\bbadge-(success|warning|error|info)\b[^"`]*\bbadge\b/,
    fix: "a status is a StatusBadge from ui/tone.tsx",
  },
  { pattern: /-mb-px border-b-2/, fix: "tabs under a header use the tab-strip classes" },
  { pattern: /\bform-control\b|\blabel-text\b/, fix: "a labelled control is a Field from ui/controls.tsx" },
  {
    pattern: /new Date\([^)]*\)\.toLocale(String|DateString|TimeString)\(/,
    fix: "a time is <When> or absoluteTime() from when.ts: one format, with its time zone",
  },
];

function files(directory, found = []) {
  for (const entry of readdirSync(directory)) {
    const path = join(directory, entry);
    if (statSync(path).isDirectory()) {
      if (entry !== "generated") files(path, found);
    } else if (path.endsWith(".tsx") || path.endsWith(".ts")) {
      found.push(path);
    }
  }
  return found;
}

const errors = [];
for (const path of files(join(root, "web/ts"))) {
  const name = relative(root, path);
  if (name.startsWith("web/ts/ui/")) continue;
  const lines = readFileSync(path, "utf8").split("\n");
  lines.forEach((line, index) => {
    if (index > 0 && lines[index - 1].includes("style-ok:")) return;
    for (const rule of RULES) {
      if (rule.pattern.test(line)) errors.push(`${name}:${index + 1}: ${rule.fix}\n    ${line.trim().slice(0, 120)}`);
    }
  });
}

if (errors.length > 0) {
  console.error(`Frontend style check failed (${errors.length}):\n${errors.join("\n")}`);
  process.exit(1);
}
console.log("Frontend style check passed.");
