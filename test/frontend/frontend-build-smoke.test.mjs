import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { gzipSync } from "node:zlib";

import {
  installFakeDom,
  loadCompiledFrontendModule,
} from "../support/helpers/fake-dom.mjs";
import { resolveFrontendModulesDir } from "../support/helpers/frontend-module-loader.mjs";

async function flushAsyncWork() {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

test("compiled dashboard bootstrap remains idempotent", async () => {
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline";

  const dashboardGrid = document.createElement("div");
  dashboardGrid.id = "dashboard-v2-operate-panel";
  document.body.appendChild(dashboardGrid);
  for (const id of [
    "dashboard-v2-root",
    "dashboard-v2-pipeline-selector-root",
    "dashboard-v2-pipeline-header-root",
    "dashboard-v2-pipeline-input-status-root",
    "dashboard-v2-pipeline-output-overview-root",
  ]) {
    const container = document.createElement("div");
    container.id = id;
    document.body.appendChild(container);
  }

  const app = await loadCompiledFrontendModule("app/dashboard-app.js");

  app.initDashboardApp();
  const firstSetDashboardMode = window.setDashboardMode;
  app.initDashboardApp();
  await flushAsyncWork();

  assert.equal(typeof firstSetDashboardMode, "function");
  assert.equal(window.setDashboardMode, firstSetDashboardMode);
});

// The static imports of a shipped module, transitively: what the browser
// fetches before the module runs.
async function staticClosure(modulesDir, entry, seen = new Set()) {
  const file = path.resolve(modulesDir, entry);
  if (seen.has(file)) return seen;
  seen.add(file);
  const source = await readFile(file, "utf8");
  for (const match of source.matchAll(/(?:\bfrom|\bimport)\s*["']([^"']+\.js)["']/g)) {
    await staticClosure(path.dirname(file), match[1], seen);
  }
  return seen;
}

async function gzipBytes(files) {
  let total = 0;
  for (const file of files) total += gzipSync(await readFile(file)).byteLength;
  return total;
}

test("the dashboard loads its React views lazily and within the page budget", async () => {
  const modulesDir = resolveFrontendModulesDir();
  const shell = await staticClosure(modulesDir, "app/dashboard-entry.js");
  const views = [
    path.resolve(modulesDir, "app/dashboard-v2-entry.js"),
    path.resolve(modulesDir, "app/dashboard-v2-checkpoints-entry.js"),
  ];
  for (const view of views) {
    assert.equal(shell.has(view), false, `${view} is fetched on first use`);
  }

  const overview = await staticClosure(
    modulesDir,
    "app/dashboard-v2-entry.js",
    new Set(shell),
  );
  assert.ok(
    (await gzipBytes(overview)) <= 268_000,
    "the Overview page payload (shell plus the React overview, gzip) must stay within its recorded budget",
  );
});
