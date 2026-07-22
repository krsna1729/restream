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
  dashboardGrid.id = "dashboard-grid";
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

test("dashboard v2 loader always enables the v2 dashboard after cutover", async () => {
  installFakeDom();
  const loader = await loadCompiledFrontendModule("app/dashboard-v2-loader.js");

  assert.equal(
    loader.dashboardV2ExperimentEnabled(),
    true,
  );
});

test("compiled dashboard no longer exposes a UI version toggle", async () => {
  const indexHtml = await readFile(
    new URL("../../public/index.html", import.meta.url),
    "utf8",
  );

  assert.doesNotMatch(indexHtml, /dashboard-ui-v2-toggle/);
  assert.doesNotMatch(indexHtml, /Use dashboard UI v2/);
});

test("compiled dashboard keeps the default React seam in a bounded bundle", async () => {
  const appDir = path.join(resolveFrontendModulesDir(), "app");
  const [defaultEntry, v2Entry, checkpointsEntry, sharedRuntime] =
    await Promise.all([
      readFile(path.join(appDir, "dashboard-entry.js")),
      readFile(path.join(appDir, "dashboard-v2-entry.js")),
      readFile(path.join(appDir, "dashboard-v2-checkpoints-entry.js")),
      readFile(path.join(appDir, "dashboard-v2-jsx-runtime.js")),
    ]);

  assert.equal(defaultEntry.includes("dashboard-v2-overview"), false);
  assert.equal(v2Entry.includes("dashboard-v2-overview"), true);
  assert.equal(
    checkpointsEntry.includes("dashboard-v2-pipeline-inspect-root"),
    true,
  );
  assert.equal(v2Entry.includes("dashboard-v2-pipeline-inspect-root"), false);
  const sharedGzip = gzipSync(sharedRuntime).byteLength;
  assert.ok(
    gzipSync(v2Entry).byteLength + sharedGzip <= 77_250,
    "the default Overview/Operate route payload must stay within its recorded gzip budget",
  );
  assert.ok(
    gzipSync(checkpointsEntry).byteLength + sharedGzip <= 69_000,
    "the opt-in checkpoint route payload must stay within its recorded gzip budget",
  );
});
