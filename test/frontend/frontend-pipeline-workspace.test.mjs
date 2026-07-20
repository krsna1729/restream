import assert from "node:assert/strict";
import test from "node:test";

import {
  appendRoot,
  flushAsyncWork,
  installFakeDom,
  loadCompiledFrontendModule,
} from "./dashboard-contract/helpers.mjs";

test("pipeline workspace canonicalizes legacy inspect and control URLs", async () => {
  const workspace = await loadCompiledFrontendModule(
    "core/pipeline-workspace.js",
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
    "core/pipeline-workspace.js",
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
    "core/pipeline-workspace.js",
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

  const shell = await loadCompiledFrontendModule(
    "features/pipeline-workspace-shell.js",
  );
  shell.syncPipelineWorkspaceShell("pipeline", "inspect");

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

  shell.syncPipelineWorkspaceShell("overview", "inspect");
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
    ["div", "inspect-resource-details"],
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
    "core/pipeline-workspace.js",
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

test("inspector keeps explicit runtime scope even if workspace refresh restores a pipeline id", async () => {
  const { document, window } = installFakeDom();
  window.location.href =
    "http://localhost/?mode=pipeline&view=inspect&p=pipe-a";
  window.history.replaceState = (_state, _title, url) => {
    window.location.href = String(url);
  };
  for (const [tag, id] of [
    ["select", "inspect-pipeline-select"],
    ["button", "inspect-open-pipeline-btn"],
    ["div", "inspect-pipeline-summary"],
    ["div", "inspect-diagnostics-summary"],
    ["div", "inspect-resource-details"],
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
      input: { status: "on", audioTracks: [], readers: 0 },
      outs: [],
      stats: {},
      hlsPreview: { active: false, segments: 0 },
    },
  ];
  globalThis.fetch = async (url) => {
    if (String(url).includes("/resource-map")) {
      return new Response(
        JSON.stringify({
          scope: { kind: "runtime" },
          summary: {},
          nodes: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    return new Response(JSON.stringify({ pipelineId: "pipe-a", nodes: [], edges: [] }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };

  inspector.renderPipelineInspector();
  assert.match(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /Pipeline A/,
  );

  const select = document.getElementById("inspect-pipeline-select");
  select.value = "__runtime";
  select.onchange();
  assert.equal(new URL(window.location.href).searchParams.get("p"), null);
  assert.match(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /Whole Runtime/,
  );

  window.location.href = "http://localhost/?mode=pipeline&view=inspect&p=pipe-a";
  inspector.renderPipelineInspector();
  assert.equal(select.value, "__runtime");
  assert.match(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /Whole Runtime/,
    "explicit runtime scope must not be overridden by a restored p parameter",
  );

  select.value = "pipe-a";
  select.onchange();
  assert.match(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /Pipeline A/,
  );
});

test("inspector preserves absent and invalid workspace selections", async () => {
  const { document, window } = installFakeDom();
  for (const [tag, id] of [
    ["select", "inspect-pipeline-select"],
    ["button", "inspect-open-pipeline-btn"],
    ["div", "inspect-pipeline-summary"],
    ["div", "inspect-diagnostics-summary"],
    ["div", "inspect-resource-details"],
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

test("inspector renders runtime resource overview with accuracy labels", async () => {
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&view=inspect";
  for (const [tag, id] of [
    ["select", "inspect-pipeline-select"],
    ["button", "inspect-open-pipeline-btn"],
    ["div", "inspect-pipeline-summary"],
    ["div", "inspect-diagnostics-summary"],
    ["div", "inspect-resource-details"],
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
          {
            id: "group:stage:child_process",
            kind: "resource_group",
            label: "child_process stages (1)",
            execution: "child_process",
            cpuPercent: 5,
            memory: {
              attributedBytes: 52428800,
              confidence: "measured",
            },
            threads: { childProcess: 1 },
            hotspots: ["transcoding"],
          },
          {
            id: "group:source_ring",
            kind: "resource_group",
            label: "Source rings (1)",
            execution: "shared",
            cpuPercent: 0,
            memory: {
              attributedBytes: 4096,
              confidence: "derived",
            },
            threads: {},
          },
          {
            id: "group:egress:srt:os_thread",
            kind: "resource_group",
            label: "SRT outputs (2)",
            execution: "os_thread",
            cpuPercent: 0,
            memory: {
              attributedBytes: 0,
              confidence: "derived",
            },
            threads: { appOwned: 2 },
          },
          {
            id: "group:egress:rtmp:tokio_task",
            kind: "resource_group",
            label: "RTMP outputs (1)",
            execution: "tokio_task",
            cpuPercent: 0,
            memory: {
              attributedBytes: 0,
              confidence: "derived",
            },
            threads: {},
          },
          {
            id: "group:stage:tokio_task",
            kind: "resource_group",
            label: "tokio_task stages (1)",
            execution: "tokio_task",
            cpuPercent: 0,
            memory: {
              attributedBytes: 0,
              confidence: "derived",
            },
            threads: {},
          },
          {
            id: "pipe-a:video:720p",
            kind: "stage",
            label: "video:720p",
            pipelineId: "pipe-a",
            execution: "child_process",
            cpuPercent: 12.5,
            memory: {
              attributedBytes: 64 * 1024 * 1024,
              confidence: "measured",
            },
            threads: { childProcess: 1 },
          },
          {
            id: "out-a",
            kind: "egress",
            label: "srt output",
            pipelineId: "pipe-a",
            execution: "os_thread",
            cpuPercent: 1,
            memory: {
              attributedBytes: 0,
              confidence: "derived",
            },
            threads: { appOwned: 1 },
          },
          {
            id: "pipe-a:source-ring",
            kind: "source_ring",
            label: "Source ring",
            pipelineId: "pipe-a",
            execution: "shared",
            cpuPercent: 0,
            memory: {
              attributedBytes: 4096,
              confidence: "derived",
            },
            threads: {},
          },
        ],
      }),
      { status: 200, headers: { "content-type": "application/json" } },
    );

  inspector.renderPipelineInspector();
  await flushAsyncWork();
  await inspector.refreshPipelineInspectorGraph();

  const graphHtml = document.getElementById(
    "inspect-graph-container",
  ).innerHTML;
  assert.match(graphHtml, /Whole Runtime/);
  assert.match(graphHtml, /Process, FFmpeg, thread, and shared-resource attribution/);
  assert.match(graphHtml, /Measured/);
  assert.match(graphHtml, /Derived/);
  assert.match(graphHtml, /Stage Breakdown/);
  assert.match(graphHtml, /processing stage/);
  assert.match(graphHtml, /output worker/);
  assert.match(graphHtml, /source ring/);
  assert.match(graphHtml, /video:720p/);
  assert.match(graphHtml, /Pipeline pipe-a/);
  assert.match(graphHtml, /FFmpeg workers/);
  assert.match(graphHtml, /12\.5%/);
  assert.doesNotMatch(graphHtml, /runtime-resource-graph/);
  assert.match(graphHtml, /<div class="space-y-3 p-3">/);
  assert.doesNotMatch(graphHtml, /Runtime Resource Overview/);
  assert.doesNotMatch(graphHtml, /Runtime Attribution/);
  assert.doesNotMatch(
    graphHtml,
    /xl:grid-cols-\[minmax\(0,1fr\)_minmax\(18rem,24rem\)\]/,
  );
  assert.match(graphHtml, /grid gap-2 border-base-content\/10 border-t pt-3 sm:grid-cols-2 xl:grid-cols-3/);
  assert.match(graphHtml, /<th>CPU<\/th>/);
  assert.match(graphHtml, /table table-xs/);
  assert.equal(
    graphHtml.match(/child_process stages \(1\)/g)?.length,
    1,
    "runtime overview should keep the child-process aggregate in the table only",
  );
  assert.doesNotMatch(
    graphHtml,
    />External FFmpeg</,
    "runtime overview should not duplicate the same child-process bucket",
  );
  assert.match(graphHtml, /SRT outputs \(2\)/);
  assert.match(graphHtml, /RTMP outputs \(1\)/);
  assert.equal(
    graphHtml.match(/tokio_task stages \(1\)/g)?.length,
    1,
    "runtime overview should keep stage aggregates in the table",
  );
  assert.match(graphHtml, /SRT Senders/);
  assert.doesNotMatch(graphHtml, /xl:col-span-2/);

  assert.match(graphHtml, /id="runtime-resource-table-scroll"/);
  assert.match(graphHtml, /data-scroll-preserve="runtime-resource-table"/);
});

test("inspector keeps processing graph for large-output pipelines", async () => {
  const { document, window } = installFakeDom();
  window.location.href =
    "http://localhost/?mode=pipeline&view=inspect&p=pipe-large";
  for (const [tag, id] of [
    ["select", "inspect-pipeline-select"],
    ["button", "inspect-open-pipeline-btn"],
    ["div", "inspect-pipeline-summary"],
    ["div", "inspect-diagnostics-summary"],
    ["div", "inspect-resource-details"],
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
    if (String(url).includes("/summary")) {
      return new Response(
        JSON.stringify({
          pipelineId: "pipe-large",
          outputs: { total: 51, running: 51 },
          graph: { hasGraph: true, nodes: 52, activeNodes: 52 },
          alerts: [
            {
              id: "pipe-large:out-7:blocked",
              severity: "warning",
              scope: "output",
              pipelineId: "pipe-large",
              outputId: "out-7",
              stageId: "pipe-large:video:720p",
              title: "Output 'out-7' is blocked by upstream stage",
              cause:
                "The output is waiting on stage 'pipe-large:video:720p' in phase 'firstOutput'.",
              evidence: ["blockedBy.stage = pipe-large:video:720p"],
              recommendedAction:
                "Inspect the upstream stage lifecycle and dependency chain for the blocked output.",
              generatedAt: "2026-07-15T00:00:00Z",
            },
            ...Array.from({ length: 3 }, (_, index) => ({
              id: `pipe-large:stage-${index}:lag`,
              severity: "warning",
              scope: "stage",
              pipelineId: "pipe-large",
              stageId: `pipe-large:stage-${index}`,
              title: `Stage 'pipe-large:stage-${index}' is lagging`,
              cause: `Stage ${index} is reading slower than the producer.`,
              evidence: [`lagSlots = ${300 + index}`],
              recommendedAction: "Check downstream throughput.",
              generatedAt: "2026-07-15T00:00:00Z",
            })),
          ],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (String(url).includes("/graph")) {
      return new Response(
        JSON.stringify({
          pipelineId: "pipe-large",
          nodes: [
            {
              id: "source",
              type: "ring_buffer",
              label: "Source Buffer",
              active: true,
            },
            ...Array.from({ length: 51 }, (_, index) => ({
              id: `egress-${index}`,
              type: "egress",
              label: `RTMP sender: Output ${index}`,
              active: true,
              details: {
                status: "running",
                phase: "send",
                totalSize: 1024,
                bitrateKbps: 64,
              },
            })),
          ],
          edges: Array.from({ length: 51 }, (_, index) => ({
            from: "source",
            to: `egress-${index}`,
            label: "RTMP publish",
          })),
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
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
        nodes: [
          {
            id: "pipe-large:video:720p",
            kind: "stage",
            label: "video:720p",
            pipelineId: "pipe-large",
            execution: "child_process",
            cpuPercent: 12.34,
            memory: {
              attributedBytes: 64 * 1024 * 1024,
              confidence: "measured",
            },
          },
          {
            id: "pipe-other:video:720p",
            kind: "stage",
            label: "other video:720p",
            pipelineId: "pipe-other",
            execution: "child_process",
            cpuPercent: 99.9,
            memory: {
              attributedBytes: 512 * 1024 * 1024,
              confidence: "measured",
            },
          },
          {
            id: "out-5",
            kind: "egress",
            label: "srt output",
            pipelineId: "pipe-large",
            execution: "os_thread",
          },
          {
            id: "out-6",
            kind: "egress",
            label: "srt output",
            pipelineId: "pipe-large",
            execution: "os_thread",
          },
          {
            id: "out-srt-other",
            kind: "egress",
            label: "srt output",
            pipelineId: "pipe-other",
            execution: "os_thread",
          },
        ],
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
    true,
    "large pipelines should still fetch the processing graph",
  );
  assert.equal(
    requests.some((url) =>
      url.includes(
        "/api/v1/engine/resource-map?pipeline_id=pipe-large&view=detail&top_n=50",
      ),
    ),
    true,
  );
  assert.match(
    document.getElementById("inspect-graph-status").textContent,
    /processing graph \/ 51 outputs/,
  );
  assert.match(
    document.getElementById("inspect-graph-container").innerHTML,
    /RTMP egress x51/,
  );
  assert.doesNotMatch(
    document.getElementById("inspect-graph-container").innerHTML,
    /id="inspect-processing-graph"/,
  );
  assert.doesNotMatch(
    document.getElementById("inspect-graph-container").innerHTML,
    /Processing Graph/,
  );
  assert.doesNotMatch(
    document.getElementById("inspect-graph-container").innerHTML,
    /SRT Senders/,
  );
  assert.doesNotMatch(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /SRT Senders/,
  );
  assert.match(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /Output 7/,
  );
  assert.match(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /Output 7 is blocked by upstream stage/,
  );
  assert.match(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /video:720p/,
  );
  assert.match(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /max-h-64 space-y-2 overflow-y-auto/,
  );
  assert.match(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /stage-2/,
  );
  assert.doesNotMatch(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /\+1 more alert/,
  );
  assert.doesNotMatch(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /out-7/,
  );
  assert.doesNotMatch(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /pipe-large:video:720p/,
  );
  assert.doesNotMatch(
    document.getElementById("inspect-pipeline-summary").innerHTML,
    /2 \/ 10 runtime \(max 512\)/,
  );
  assert.doesNotMatch(
    document.getElementById("inspect-graph-container").innerHTML,
    /FFmpeg workers/,
  );
  const resourceDetails = document.getElementById("inspect-resource-details");
  assert.match(
    resourceDetails.innerHTML,
    /Process Metrics/,
  );
  assert.match(
    resourceDetails.innerHTML,
    /Pipeline Attribution/,
  );
  assert.match(
    resourceDetails.innerHTML,
    /SRT Senders/,
  );
  assert.doesNotMatch(
    resourceDetails.innerHTML,
    /2 \/ 10 runtime/,
  );
  assert.match(
    resourceDetails.innerHTML,
    /2 \/ 10/,
  );
  assert.doesNotMatch(
    resourceDetails.innerHTML,
    /This pipeline \/ total active/,
  );
  assert.match(
    resourceDetails.innerHTML,
    /max 512/,
  );
  assert.match(
    resourceDetails.innerHTML,
    /FFmpeg workers/,
  );
  assert.match(
    resourceDetails.innerHTML,
    /table table-sm/,
  );
  assert.match(
    resourceDetails.innerHTML,
    /video:720p/,
  );
  assert.match(
    resourceDetails.innerHTML,
    /12\.3%/,
  );
  assert.match(
    resourceDetails.innerHTML,
    /64\.0 MiB/,
  );
  assert.doesNotMatch(
    resourceDetails.innerHTML,
    /other video:720p/,
  );
  assert.doesNotMatch(
    resourceDetails.innerHTML,
    /512\.0 MiB/,
  );
  assert.doesNotMatch(
    resourceDetails.innerHTML,
    /Accounted 1 stage worker for 2 measured/,
  );

  assert.match(
    document.getElementById("inspect-graph-container").innerHTML,
    /id="processing-graph-canvas"/,
  );
  assert.match(
    document.getElementById("inspect-graph-container").innerHTML,
    /data-scroll-preserve="processing-graph-canvas"/,
  );
});

test("inspector keeps the previous graph visible during background refresh", async () => {
  const { document, window } = installFakeDom();
  window.location.href =
    "http://localhost/?mode=pipeline&view=inspect&p=pipe-live";
  for (const [tag, id] of [
    ["select", "inspect-pipeline-select"],
    ["button", "inspect-open-pipeline-btn"],
    ["div", "inspect-pipeline-summary"],
    ["div", "inspect-diagnostics-summary"],
    ["div", "inspect-resource-details"],
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
      id: "pipe-live",
      name: "Live Pipeline",
      input: {
        status: "on",
        probeReady: true,
        probeStatus: "ready",
        readers: 0,
        audioTracks: [],
        publisher: { protocol: "srt" },
      },
      outs: [],
      stats: { inputBitrateKbps: 1, outputBitrateKbps: 1 },
      hlsPreview: { active: false, segments: 0 },
    },
  ];

  let holdSecondGraphRequest;
  let graphRequestCount = 0;
  globalThis.fetch = async (url) => {
    const href = String(url);
    if (href.includes("/graph")) {
      graphRequestCount += 1;
      if (graphRequestCount === 2) {
        await new Promise((resolve) => {
          holdSecondGraphRequest = resolve;
        });
      }
      return new Response(
        JSON.stringify({
          pipelineId: "pipe-live",
          nodes: [
            {
              id: "source",
              type: "ring_buffer",
              label: "Source Buffer",
              active: true,
            },
          ],
          edges: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (href.includes("/resource-map")) {
      return new Response(
        JSON.stringify({
          scope: { kind: "pipeline" },
          summary: {},
          nodes: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    return new Response("{}", { status: 200 });
  };

  await inspector.refreshPipelineInspectorGraph();
  const graphContainer = document.getElementById("inspect-graph-container");
  const graphStatus = document.getElementById("inspect-graph-status");
  assert.match(graphContainer.innerHTML, /Source Buffer/);
  assert.equal(
    graphStatus.textContent,
    "Live Pipeline / processing graph / 0 outputs / input live",
  );

  const refresh = inspector.refreshPipelineInspectorGraph();
  await Promise.resolve();
  assert.match(
    graphContainer.innerHTML,
    /Source Buffer/,
    "refresh should leave the previous graph mounted while the new graph loads",
  );
  assert.doesNotMatch(graphContainer.innerHTML, /Loading graph/);
  assert.equal(
    graphStatus.textContent,
    "Live Pipeline / processing graph / 0 outputs / input live",
    "background refresh should not replace the graph summary with loading text",
  );
  holdSecondGraphRequest();
  await refresh;
});

test("inspector runtime graph refresh ignores a stale resolution after the container is swapped mid-flight", async () => {
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&view=inspect";
  for (const [tag, id] of [
    ["select", "inspect-pipeline-select"],
    ["button", "inspect-open-pipeline-btn"],
    ["div", "inspect-pipeline-summary"],
    ["div", "inspect-diagnostics-summary"],
    ["div", "inspect-resource-details"],
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

  let resourceMapRequestCount = 0;
  let releaseResourceMapRequest;
  globalThis.fetch = async (url) => {
    const href = String(url);
    if (href.includes("/resource-map")) {
      resourceMapRequestCount += 1;
      if (resourceMapRequestCount === 1) {
        await new Promise((resolve) => {
          releaseResourceMapRequest = resolve;
        });
      }
      return new Response(
        JSON.stringify({
          scope: { kind: "runtime" },
          summary: {
            cpuPercent: 1,
            totalMemoryBytes: 1,
            processThreadCount: 1,
            srtSenderThreads: 0,
            srtSenderThreadLimit: 1,
            externalFfmpegCount: 0,
            retainedPayloadBytes: 0,
          },
          nodes: [
            {
              id: "runtime:restream",
              kind: "runtime_process",
              label: "restream",
              execution: "process",
              cpuPercent: 1,
              memory: { attributedBytes: 1, confidence: "measured" },
              threads: { process: 1 },
              hotspots: [],
            },
          ],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    return new Response("{}", { status: 200 });
  };

  const graphContainer = document.getElementById("inspect-graph-container");
  const graphStatus = document.getElementById("inspect-graph-status");

  const refresh = inspector.refreshPipelineInspectorGraph();
  assert.match(
    graphContainer.innerHTML,
    /Loading runtime resources/,
    "the loading placeholder should be painted synchronously before the fetch resolves",
  );

  // Simulate a dashboard-mode swap away from and back to the pipeline-inspect
  // view while the runtime resource-map fetch is still in flight. The `p`
  // query param stays absent in both states, so selectedPipeline() alone
  // cannot detect this swap -- only the RenderScope container-id check can.
  inspector.setPipelineInspectorContainerId("control-mode-content");
  releaseResourceMapRequest();
  await refresh;

  assert.doesNotMatch(
    graphContainer.innerHTML,
    /restream/,
    "a resolution that lands after the host container was swapped away must not paint into the stale container",
  );
  assert.match(
    graphStatus.textContent,
    /Loading runtime resources/,
    "a stale resolution must not overwrite the status line either",
  );

  // Swap back to the pipeline-inspect container and confirm a fresh refresh
  // still populates the graph -- the stale resolution must not have left the
  // module's cache state (graphRenderedStateKey/graphPipelineId) poisoned.
  inspector.setPipelineInspectorContainerId("inspect-mode-content");
  await inspector.refreshPipelineInspectorGraph();
  assert.match(
    graphContainer.innerHTML,
    /restream/,
    "a refresh once the container is current again must still populate the graph",
  );
});

test("processing graph collapses repeated egress leaves by count", async () => {
  const { document } = installFakeDom();
  const graph = await loadCompiledFrontendModule("features/graph.js");
  const container = appendRoot(document, "div", "graph-target");
  graph.renderGraphInto(container, {
    pipelineId: "pipe-1",
    nodes: [
      {
        id: "source",
        type: "ring_buffer",
        label: "Source Buffer",
        active: true,
      },
      ...Array.from({ length: 5 }, (_, index) => ({
        id: `rtmp-${index}`,
        type: "egress",
        label: `RTMP sender: Output ${index}`,
        active: true,
        details: {
          status: "running",
          phase: "send",
          totalSize: 1024,
          bitrateKbps: 64,
        },
        metrics: {
          packetsIn: 10,
          packetsOut: 10,
          bytesIn: 2048,
          bytesOut: 2048,
          processingUs: 100,
          avgUsPerPacket: 10,
          packetsPerSec: 30,
          uptimeSecs: 5,
        },
      })),
      {
        id: "srt-unique",
        type: "egress",
        label: "SRT sender: Backup",
        active: true,
        details: {
          status: "running",
          phase: "send",
          totalSize: 2048,
          bitrateKbps: 32,
        },
      },
    ],
    edges: [
      ...Array.from({ length: 5 }, (_, index) => ({
        from: "source",
        to: `rtmp-${index}`,
        label: "RTMP publish",
      })),
      { from: "source", to: "srt-unique", label: "SRT send" },
    ],
  });

  const html = container.innerHTML;
  assert.match(html, /Click a grouped node to inspect its members/);
  assert.doesNotMatch(html, /data-graph-copy-svg/);
  assert.doesNotMatch(html, /Copy SVG/);
  assert.match(html, /data-graph-aggregate-key/);
  assert.match(html, /click to expand/);
  assert.match(html, /RTMP egress x5/);
  assert.match(html, /branch starts: 5 leaves/);
  assert.match(html, /fan-out: RTMP egress x5/);
  assert.match(html, /5 RTMP outputs/);
  assert.match(html, /5\/5 running/);
  assert.match(html, /SRT sender: Backup/);
  assert.doesNotMatch(html, /RTMP sender: Output 0/);
});

test("processing graph keeps small graphs on the standard canvas scale", async () => {
  const { document } = installFakeDom();
  const graph = await loadCompiledFrontendModule("features/graph.js");
  const container = appendRoot(document, "div", "graph-target");
  graph.renderGraphInto(container, {
    pipelineId: "runtime",
    nodes: [
      {
        id: "runtime:restream",
        type: "packetizer",
        label: "restream",
        active: true,
      },
      {
        id: "runtime:external-ffmpeg",
        type: "transcoder",
        label: "External FFmpeg",
        active: true,
        details: {
          phase: "firstOutput",
          healthStatus: "warning",
          healthReason: "source reader overflowed 1 time(s)",
          overflowCount: 1,
        },
      },
    ],
    edges: [
      {
        from: "runtime:restream",
        to: "runtime:external-ffmpeg",
        label: "runtime",
      },
    ],
  });

  assert.match(
    container.innerHTML,
    /viewBox="0 0 1680 /,
    "small graphs should not zoom nodes larger than pipeline graphs",
  );
  assert.match(container.innerHTML, /Processing graph SVG/);
  assert.doesNotMatch(container.innerHTML, /data-graph-copy-svg/);
  assert.doesNotMatch(container.innerHTML, /Copy SVG/);
  assert.match(container.innerHTML, /stroke="#f59e0b"/);
  assert.match(container.innerHTML, /warn: source reader overflowed/);
});

test("processing graph collapses repeated non-egress leaf stages at the branch point", async () => {
  const { document } = installFakeDom();
  const graph = await loadCompiledFrontendModule("features/graph.js");
  const container = appendRoot(document, "div", "graph-target");
  graph.renderGraphInto(container, {
    pipelineId: "pipe-1",
    nodes: [
      {
        id: "demux",
        type: "demux",
        label: "Program demux",
        active: true,
      },
      ...Array.from({ length: 6 }, (_, index) => ({
        id: `audio-${index}`,
        type: "audio_filter",
        label: `Audio filter ${index}`,
        active: true,
        details: {
          phase: "active",
          backend: "ffmpeg",
        },
      })),
    ],
    edges: Array.from({ length: 6 }, (_, index) => ({
      from: "demux",
      to: `audio-${index}`,
      label: "audio track",
    })),
  });

  const html = container.innerHTML;
  assert.match(html, /Audio Filter x6/);
  assert.match(html, /6 audio filter stages/);
  assert.match(html, /branch starts: 6 leaves/);
  assert.match(html, /fan-out: Audio Filter x6/);
  assert.doesNotMatch(html, /Audio filter 0/);
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

test("YouTube monitor warning refresh ignores a stale response after the shell is reassigned", async () => {
  const { document } = installFakeDom();
  const controlRoom = await loadCompiledFrontendModule(
    "features/control-room.js",
  );

  const urlA = "https://www.youtube.com/watch?v=aaaaaaaaaaa";
  const urlB = "https://www.youtube.com/watch?v=bbbbbbbbbbb";
  const statusByUrl = {
    [urlA]: { live_now: false, live_content: true, upcoming: false },
    [urlB]: { live_now: true, live_content: false, upcoming: false },
  };
  const releasers = new Map();
  globalThis.fetch = async (url) => {
    const requestedUrl = decodeURIComponent(String(url).split("url=").at(-1));
    return new Promise((resolve) => {
      releasers.set(requestedUrl, () =>
        resolve(
          new Response(JSON.stringify(statusByUrl[requestedUrl]), {
            status: 200,
            headers: { "content-type": "application/json" },
          }),
        ),
      );
    });
  };

  const article = document.createElement("article");
  article.dataset.cardId = "output:out-b";
  const statusCluster = document.createElement("div");
  statusCluster.dataset.role = "control-room-card-status-cluster";
  article.appendChild(statusCluster);
  const shell = document.createElement("div");
  shell.dataset.role = "control-room-player-shell";
  article.appendChild(shell);
  document.body.appendChild(article);

  // Card A occupies the shell first; its status fetch starts but is held pending.
  shell.dataset.mediaKey = urlA;
  controlRoom.refreshYouTubeCardWarning(shell, urlA);

  // The shell is reused for card B before A's fetch resolves. This mirrors
  // syncCard's synchronous mediaKey write, which always happens-before any
  // fetch .then() callback can fire.
  shell.dataset.mediaKey = urlB;
  controlRoom.refreshYouTubeCardWarning(shell, urlB);

  // B resolves first: it is live now, so no warning should be applied.
  releasers.get(urlB)();
  await flushAsyncWork();
  assert.equal(
    statusCluster.querySelector('[data-role="control-room-card-warning"]'),
    null,
    "a live status must not add a warning badge",
  );

  // A resolves late (stale). Its "not live" warning must be dropped rather
  // than clobbering the shell that now belongs to card B.
  releasers.get(urlA)();
  await flushAsyncWork();
  assert.equal(
    statusCluster.querySelector('[data-role="control-room-card-warning"]'),
    null,
    "a stale response for a reassigned shell must not overwrite the current card's warning",
  );
});
