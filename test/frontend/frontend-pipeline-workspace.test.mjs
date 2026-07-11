import assert from "node:assert/strict";
import test from "node:test";

import {
  appendRoot,
  installFakeDom,
  loadCompiledFrontendModule,
} from "./dashboard-contract/helpers.mjs";

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
  assert.equal(operate.getAttribute("aria-selected"), "false");
  assert.equal(inspect.getAttribute("aria-selected"), "true");
  assert.equal(monitor.getAttribute("aria-selected"), "false");
  assert.equal(operate.tabIndex, -1);
  assert.equal(inspect.tabIndex, 0);
  assert.equal(monitor.tabIndex, -1);
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
    const expectedSummary = href.includes("p=missing")
      ? /No pipeline selected/
      : /Whole Runtime/;
    assert.match(
      document.getElementById("inspect-pipeline-summary").innerHTML,
      expectedSummary,
    );
    assert.equal(
      document.getElementById("inspect-open-pipeline-btn").disabled,
      true,
    );
  }
});

test("inspector renders runtime resource graph with accuracy labels", async () => {
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&view=inspect";
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
  state.pipelines = [];
  globalThis.fetch = async () =>
    new Response(
      JSON.stringify({
        scope: { kind: "runtime" },
        summary: {
          cpuPercent: 7.5,
          totalMemoryBytes: 104857600,
          processThreadCount: 12,
          srtSenderThreads: 1,
          srtSenderThreadLimit: 512,
          externalFfmpegCount: 1,
          retainedPayloadBytes: 4096,
        },
        nodes: [
          {
            id: "runtime:restream",
            kind: "runtime_process",
            label: "restream",
            execution: "process",
            cpuPercent: 2.5,
            memory: {
              attributedBytes: 52428800,
              confidence: "measured",
            },
            threads: { process: 12 },
            hotspots: ["control"],
          },
          {
            id: "runtime:external-ffmpeg",
            kind: "child_process_group",
            label: "External FFmpeg",
            execution: "child_process",
            cpuPercent: 5,
            memory: {
              attributedBytes: 52428800,
              confidence: "measured",
            },
            threads: { childProcess: 1 },
            hotspots: ["transcoding"],
          },
        ],
      }),
      { status: 200, headers: { "content-type": "application/json" } },
    );

  inspector.renderPipelineInspector();
  await inspector.refreshPipelineInspectorGraph();

  const graphHtml = document.getElementById(
    "inspect-graph-container",
  ).innerHTML;
  assert.match(graphHtml, /Runtime Resource Graph/);
  assert.match(graphHtml, /Measured/);
  assert.match(graphHtml, /Derived/);
  assert.match(graphHtml, /runtime-resource-graph/);
});

test("inspector uses grouped resource view for large-output pipelines", async () => {
  const { document, window } = installFakeDom();
  window.location.href =
    "http://localhost/?mode=pipeline&view=inspect&p=pipe-large";
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
  const requests = [];
  globalThis.fetch = async (url) => {
    requests.push(String(url));
    return new Response(
      JSON.stringify({
        scope: { kind: "pipeline", pipelineId: "pipe-large" },
        view: "grouped",
        limits: {
          topN: 25,
          totalNodeCount: 125,
          returnedNodeCount: 4,
          truncatedNodeCount: 121,
        },
        summary: {
          cpuPercent: 12,
          totalMemoryBytes: 104857600,
          processThreadCount: 30,
          srtSenderThreads: 10,
          srtSenderThreadLimit: 512,
          externalFfmpegCount: 2,
          retainedPayloadBytes: 4096,
        },
        nodes: [],
      }),
      { status: 200, headers: { "content-type": "application/json" } },
    );
  };

  const inspector = await loadCompiledFrontendModule(
    "features/pipeline-inspector.js",
  );
  const { state } = await loadCompiledFrontendModule("core/state.js");
  state.pipelines = [
    {
      id: "pipe-large",
      name: "Large Pipeline",
      input: {
        status: "on",
        probeReady: true,
        probeStatus: "ready",
        readers: 0,
        audioTracks: [],
        publisher: { protocol: "rtmp" },
      },
      outs: Array.from({ length: 51 }, (_, index) => ({
        id: `out-${index}`,
        name: `Output ${index}`,
        desiredState: "started",
        status: "running",
        url: `rtmp://example/${index}`,
        config: { video: { mode: "source" }, audio: { mode: "all" } },
      })),
      stats: { inputBitrateKbps: 1, outputBitrateKbps: 1 },
      hlsPreview: { active: false, segments: 0 },
    },
  ];

  inspector.renderPipelineInspector();
  await inspector.refreshPipelineInspectorGraph();

  assert.equal(
    requests.some((url) => url.includes("/graph")),
    false,
    "large pipelines should not fetch the raw processing graph by default",
  );
  assert.equal(
    requests.some((url) =>
      url.includes(
        "/api/v1/engine/resource-map?pipeline_id=pipe-large&view=grouped&top_n=25",
      ),
    ),
    true,
  );
  assert.match(
    document.getElementById("inspect-graph-status").textContent,
    /grouped resources/,
  );
});

test("monitor consumes and propagates the shared workspace selection", async () => {
  installFakeDom();
  const controlRoom = await loadCompiledFrontendModule(
    "features/control-room.js",
  );
  const { state } = await loadCompiledFrontendModule("core/state.js");
  const makePipeline = (id, outputId) => ({
    id,
    name: id,
    outs: [
      {
        id: outputId,
        name: outputId,
        monitoringUrl: `https://example.com/${outputId}`,
        status: "running",
      },
    ],
  });
  state.pipelines = [
    makePipeline("pipe-a", "out-a"),
    makePipeline("pipe-b", "out-b"),
  ];

  let sharedSelection = "pipe-b";
  const propagatedSelections = [];
  const openedMonitorSelections = [];
  controlRoom.setControlRoomWorkspaceDependencies({
    selectedPipelineId: () => sharedSelection,
    selectPipeline: (pipelineId) => propagatedSelections.push(pipelineId),
    openMonitorView: (pipelineId) => openedMonitorSelections.push(pipelineId),
  });

  assert.equal(controlRoom.syncControlRoomWorkspaceSelection(), "pipe-b");
  sharedSelection = "missing";
  assert.equal(
    controlRoom.syncControlRoomWorkspaceSelection(),
    null,
    "invalid URL selection must not revive a persisted/default pipeline",
  );
  sharedSelection = null;
  assert.equal(controlRoom.syncControlRoomWorkspaceSelection(), null);

  controlRoom.selectControlRoomPipeline("pipe-a");
  assert.deepEqual(propagatedSelections, ["pipe-a"]);

  controlRoom.openControlRoomForOutput("out-b");
  assert.deepEqual(openedMonitorSelections, ["pipe-b"]);
});
