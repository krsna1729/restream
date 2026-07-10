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

test("inspector treats URL pipeline selection as authoritative across views", async () => {
  const { document, window } = installFakeDom();
  window.location.href =
    "http://localhost/?mode=pipeline&view=operate&p=pipe-b";
  window.history.pushState = (_state, _title, url) => {
    window.location.href = String(url);
  };
  for (const [tag, id] of [
    ["select", "inspect-pipeline-select"],
    ["button", "inspect-open-pipeline-btn"],
    ["div", "inspect-pipeline-summary"],
    ["div", "inspect-diagnostics-summary"],
    ["button", "inspect-refresh-graph-btn"],
    ["button", "inspect-open-diagnostics-btn"],
    ["div", "inspect-graph-status"],
    ["div", "inspect-graph-container"],
  ]) {
    appendRoot(document, tag, id);
  }
  const makePipeline = (id, name) => ({
    id,
    name,
    key: `${id}-key`,
    input: {
      status: "on",
      probeReady: true,
      probeStatus: "ready",
      readers: 0,
      audioTracks: [],
      video: null,
      publisher: { protocol: "rtmp" },
      bytesReceived: 1,
      bytesSent: 1,
    },
    outs: [],
    stats: { inputBitrateKbps: 1, outputBitrateKbps: 0 },
    hlsPreview: { active: false, segments: 0 },
  });
  globalThis.fetch = async (url) => {
    const pipelineId = decodeURIComponent(String(url).split("/").at(-2));
    return new Response(JSON.stringify({ pipelineId, nodes: [], edges: [] }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };

  const workspace = await loadCompiledFrontendModule(
    "app/pipeline-workspace.js",
  );
  const inspector = await loadCompiledFrontendModule(
    "features/pipeline-inspector.js",
  );
  const { state } = await loadCompiledFrontendModule("core/state.js");
  state.pipelines = [
    makePipeline("pipe-a", "Previously inspected A"),
    makePipeline("pipe-b", "Operate selection B"),
  ];
  inspector.setPipelineInspectorDependencies({
    selectPipeline: (pipelineId) => {
      window.history.pushState(
        {},
        "",
        workspace.pipelineWorkspaceUrl(
          window.location.href,
          "inspect",
          pipelineId,
        ),
      );
    },
  });

  window.history.pushState(
    {},
    "",
    workspace.pipelineWorkspaceUrl(window.location.href, "inspect", "pipe-b"),
  );
  inspector.renderPipelineInspector();
  assert.match(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /Operate selection B/,
  );
  assert.doesNotMatch(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /Previously inspected A/,
  );

  const select = document.getElementById("inspect-pipeline-select");
  select.value = "pipe-a";
  select.onchange();
  assert.equal(new URL(window.location.href).searchParams.get("p"), "pipe-a");

  window.history.pushState(
    {},
    "",
    workspace.pipelineWorkspaceUrl(window.location.href, "operate"),
  );
  assert.equal(
    new URL(window.location.href).searchParams.get("p"),
    "pipe-a",
    "the inspector selection remains the shared workspace selection",
  );
});

test("inspector preserves absent and invalid workspace selections", async () => {
  const { document, window } = installFakeDom();
  for (const [tag, id] of [
    ["select", "inspect-pipeline-select"],
    ["button", "inspect-open-pipeline-btn"],
    ["div", "inspect-pipeline-summary"],
    ["div", "inspect-diagnostics-summary"],
    ["button", "inspect-refresh-graph-btn"],
    ["button", "inspect-open-diagnostics-btn"],
    ["div", "inspect-graph-status"],
    ["div", "inspect-graph-container"],
  ]) {
    appendRoot(document, tag, id);
  }
  const inspector = await loadCompiledFrontendModule(
    "features/pipeline-inspector.js",
  );
  const { state } = await loadCompiledFrontendModule("core/state.js");
  state.pipelines = [
    {
      id: "pipe-a",
      name: "Pipeline A",
      input: { status: "off", audioTracks: [], readers: 0 },
      outs: [],
      stats: {},
      hlsPreview: { active: false, segments: 0 },
    },
  ];

  for (const href of [
    "http://localhost/?mode=pipeline&view=inspect",
    "http://localhost/?mode=pipeline&view=inspect&p=missing",
  ]) {
    window.location.href = href;
    inspector.renderPipelineInspector();
    assert.match(
      document.getElementById("inspect-pipeline-summary").innerHTML,
      /No pipeline selected/,
    );
    assert.equal(
      document.getElementById("inspect-open-pipeline-btn").disabled,
      true,
    );
  }
});
