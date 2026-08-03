import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

// Mirrors the backend's architecture_compliance.rs / domain_layering.rs:
// prove the module-layering rule holds by scanning raw source, rather than
// assuming it from convention. core/ is the lowest layer (types, API client,
// pure state) and must not depend upward on features/ or app/; features/
// must not depend upward on app/ (the entry-point/composition layer).
const WEB_TS_ROOT = fileURLToPath(new URL("../../web/ts", import.meta.url));

const IMPORT_SPECIFIER_RE =
  /(?:import|export)(?:[^'"]*?)from\s+["']([^"']+)["']/g;

async function listTsFiles(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = await Promise.all(
    entries.map(async (entry) => {
      const entryPath = path.join(dir, entry.name);
      if (entry.isDirectory()) return listTsFiles(entryPath);
      if (/\.(ts|tsx)$/.test(entry.name)) return [entryPath];
      return [];
    }),
  );
  return files.flat();
}

function extractRelativeImports(source) {
  const specifiers = [];
  for (const match of source.matchAll(IMPORT_SPECIFIER_RE)) {
    const specifier = match[1];
    if (specifier.startsWith(".")) specifiers.push(specifier);
  }
  return specifiers;
}

function resolvedLayer(fromFile, specifier) {
  const resolved = path.posix.normalize(
    path.posix.join(path.posix.dirname(fromFile), specifier),
  );
  const relativeToRoot = path.posix.relative("web/ts", resolved);
  return relativeToRoot.split("/")[0];
}

async function collectViolations(sourceDir, forbiddenLayers) {
  const files = await listTsFiles(path.join(WEB_TS_ROOT, sourceDir));
  const violations = [];
  for (const file of files) {
    const source = await readFile(file, "utf8");
    const posixFile = path
      .relative(WEB_TS_ROOT, file)
      .split(path.sep)
      .join("/");
    const posixFileAsRoot = `web/ts/${posixFile}`;
    for (const specifier of extractRelativeImports(source)) {
      const layer = resolvedLayer(posixFileAsRoot, specifier);
      if (forbiddenLayers.includes(layer)) {
        violations.push(`${posixFile} imports "${specifier}" (layer: ${layer})`);
      }
    }
  }
  return violations;
}

test("core/ does not import upward from features/ or app/", async () => {
  const violations = await collectViolations("core", ["features", "app"]);
  assert.deepEqual(violations, []);
});

test("features/ does not import upward from app/", async () => {
  const violations = await collectViolations("features", ["app"]);
  assert.deepEqual(violations, []);
});
