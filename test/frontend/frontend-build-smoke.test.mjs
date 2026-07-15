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

test("compiled dashboard bootstrap remains idempotent", async () => {
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline";

  const dashboardGrid = document.createElement("div");
  dashboardGrid.id = "dashboard-grid";
  document.body.appendChild(dashboardGrid);

  const app = await loadCompiledFrontendModule("app/dashboard-app.js");

  app.initDashboardApp();
  const firstSetDashboardMode = window.setDashboardMode;
  app.initDashboardApp();

  assert.equal(typeof firstSetDashboardMode, "function");
  assert.equal(window.setDashboardMode, firstSetDashboardMode);
});

test("dashboard v2 loader only enables the exact opt-in query", async () => {
  installFakeDom();
  const loader = await loadCompiledFrontendModule("app/dashboard-v2-loader.js");

  assert.equal(loader.dashboardV2ExperimentEnabled("?mode=overview"), false);
  assert.equal(
    loader.dashboardV2ExperimentEnabled("?mode=overview&ui=v2"),
    true,
  );
  assert.equal(loader.dashboardV2ExperimentEnabled("?ui=V2"), false);
});

test("compiled dashboard keeps the opt-in React seam in a bounded bundle", async () => {
  const appDir = path.join(resolveFrontendModulesDir(), "app");
  const [defaultEntry, v2Entry] = await Promise.all([
    readFile(path.join(appDir, "dashboard-entry.js")),
    readFile(path.join(appDir, "dashboard-v2-entry.js")),
  ]);

  assert.equal(defaultEntry.includes("dashboard-v2-overview"), false);
  assert.equal(v2Entry.includes("dashboard-v2-overview"), true);
  assert.ok(
    gzipSync(v2Entry).byteLength <= 75_000,
    "the opt-in component seam must stay within its recorded gzip budget",
  );
});
