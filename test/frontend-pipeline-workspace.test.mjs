import assert from "node:assert/strict";
import test from "node:test";

import {
  appendRoot,
  installFakeDom,
  loadCompiledFrontendModule,
} from "./frontend-dashboard-contract/helpers.mjs";

test("pipeline workspace canonicalizes legacy inspect and control URLs", async () => {
  const workspace = await loadCompiledFrontendModule(
    "app/pipeline-workspace.js",
  );

  const inspect = workspace.resolveDashboardLocation(
    "http://localhost/dashboard?mode=inspect&p=pipe%2Fone&extra=kept",
  );
  assert.equal(inspect.mode, "pipeline");
  assert.equal(inspect.pipelineView, "inspect");
  assert.equal(inspect.url.searchParams.get("mode"), "pipeline");
  assert.equal(inspect.url.searchParams.get("view"), "inspect");
  assert.equal(inspect.url.searchParams.get("p"), "pipe/one");
  assert.equal(inspect.url.searchParams.get("extra"), "kept");
  assert.equal(inspect.needsCanonicalReplace, true);

  const control = workspace.resolveDashboardLocation(
    "http://localhost/dashboard?mode=control&p=pipe-2",
  );
  assert.equal(control.mode, "pipeline");
  assert.equal(control.pipelineView, "monitor");
  assert.equal(control.url.searchParams.get("mode"), "pipeline");
  assert.equal(control.url.searchParams.get("view"), "monitor");
  assert.equal(control.url.searchParams.get("p"), "pipe-2");
  assert.equal(control.needsCanonicalReplace, true);
});

test("dashboard URL resolution removes pipeline state outside the workspace", async () => {
  const workspace = await loadCompiledFrontendModule(
    "app/pipeline-workspace.js",
  );

  const overview = workspace.resolveDashboardLocation(
    "http://localhost/?mode=overview&view=inspect&p=pipe-1",
  );
  assert.equal(overview.mode, "overview");
  assert.equal(overview.url.searchParams.has("view"), false);
  assert.equal(overview.url.searchParams.has("p"), false);
  assert.equal(overview.needsCanonicalReplace, true);

  const admin = workspace.resolveDashboardLocation(
    "http://localhost/?mode=admin&view=monitor&p=pipe-2",
  );
  assert.equal(admin.mode, "settings");
  assert.equal(admin.url.searchParams.get("mode"), "settings");
  assert.equal(admin.url.searchParams.has("view"), false);
  assert.equal(admin.url.searchParams.has("p"), false);
  assert.equal(admin.needsCanonicalReplace, true);
});

test("pipeline workspace defaults and URL builders preserve only relevant state", async () => {
  const workspace = await loadCompiledFrontendModule(
    "app/pipeline-workspace.js",
  );

  const selected = workspace.resolveDashboardLocation(
    "http://localhost/?p=pipe-1",
  );
  assert.equal(selected.mode, "pipeline");
  assert.equal(selected.pipelineView, "operate");
  assert.equal(selected.url.searchParams.get("view"), "operate");

  const inspect = workspace.pipelineWorkspaceUrl(
    "http://localhost/?mode=overview&extra=kept",
    "inspect",
    "pipe/one",
  );
  assert.equal(inspect.searchParams.get("mode"), "pipeline");
  assert.equal(inspect.searchParams.get("view"), "inspect");
  assert.equal(inspect.searchParams.get("p"), "pipe/one");
  assert.equal(inspect.searchParams.get("extra"), "kept");

  const settings = workspace.dashboardModeUrl(inspect.href, "settings");
  assert.equal(settings.searchParams.get("mode"), "settings");
  assert.equal(settings.searchParams.has("view"), false);
  assert.equal(settings.searchParams.has("p"), false);
});

test("pipeline workspace shell exposes one active subordinate view", async () => {
  const { document } = installFakeDom();
  const bar = appendRoot(document, "div", "pipeline-workspace-view-bar");
  bar.classList.add("hidden");
  const operate = document.createElement("button");
  operate.dataset.pipelineWorkspaceView = "operate";
  const inspect = document.createElement("button");
  inspect.dataset.pipelineWorkspaceView = "inspect";
  const monitor = document.createElement("button");
  monitor.dataset.pipelineWorkspaceView = "monitor";
  document.body.append(operate, inspect, monitor);
  const operatePanel = appendRoot(document, "section", "dashboard-grid");
  const inspectPanel = appendRoot(document, "section", "inspect-mode-panel");
  const monitorPanel = appendRoot(document, "section", "control-mode-panel");

  const workspace = await loadCompiledFrontendModule(
    "app/pipeline-workspace.js",
  );
  workspace.syncPipelineWorkspaceShell("pipeline", "inspect");

  assert.equal(bar.classList.contains("hidden"), false);
  assert.equal(operate.getAttribute("aria-pressed"), "false");
  assert.equal(inspect.getAttribute("aria-pressed"), "true");
  assert.equal(monitor.getAttribute("aria-pressed"), "false");
  assert.equal(operatePanel.classList.contains("hidden"), true);
  assert.equal(inspectPanel.classList.contains("hidden"), false);
  assert.equal(monitorPanel.classList.contains("hidden"), true);

  workspace.syncPipelineWorkspaceShell("overview", "inspect");
  assert.equal(bar.classList.contains("hidden"), true);
  assert.equal(inspectPanel.classList.contains("hidden"), true);
});
