import assert from "node:assert/strict";
import test from "node:test";

import {
  appendRoot,
  flushAsyncWork,
  installFakeDom,
  loadCompiledFrontendModule,
  waitForCondition,
} from "./helpers.mjs";

test("overview activity SSE wakes the dashboard runtime without waiting for the next poll", async () => {
  const settingsUrl = "/api/v1/settings?view=dashboard";
  const summaryRuntimeWithFullMetricsUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=full";
  const summaryRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=summary";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=overview";
  appendRoot(document, "div", "overview-mode-panel");
  appendRoot(document, "div", "overview-mode-content");
  appendRoot(document, "div", "dashboard-grid");

  const requests = [];
  globalThis.fetch = async (url) => {
    const href = String(url);
    requests.push(href);

    if (href === "/api/v1/audio-caps") {
      return new Response(
        JSON.stringify({ caps: {}, platformLabels: {} }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (href === settingsUrl) {
      return new Response(
        JSON.stringify({
          serverName: "Restream",
          pipelines: [],
          outputs: [],
          jobs: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (href === summaryRuntimeWithFullMetricsUrl) {
      return new Response(
        JSON.stringify({
          health: { status: "ready", pipelines: {} },
          metrics: {
            generatedAt: "2026-06-30T00:00:00Z",
            cpu: { usagePercent: 12, cores: 4, load1: 0.5 },
            memory: { usedPercent: 20, totalBytes: 200, usedBytes: 40 },
            engine: { cpuPercent: 3, totalMemoryBytes: 1234, cpuSampleReady: true },
            disk: { usedPercent: 40, mountPoint: "/", root: "/" },
            network: { downloadKbps: 1, uploadKbps: 2 },
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (href === summaryRuntimeUrl) {
      return new Response(
        JSON.stringify({
          health: { status: "ready", pipelines: {} },
          metrics: {
            generatedAt: "2026-06-30T00:00:05Z",
            cpu: { usagePercent: 14 },
            memory: { usedPercent: 22 },
            disk: { usedPercent: 42 },
            network: { downloadKbps: 3, uploadKbps: 4 },
            engine: {
              cpuPercent: 5,
              totalMemoryBytes: 1236,
              cpuSampleReady: true,
            },
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (href === "/api/v1/logs?scope=restream&limit=24&order=desc") {
      return new Response(
        JSON.stringify({
          logs: [
            {
              id: 41,
              ts: "2026-06-30T00:00:00Z",
              level: "INFO",
              target: "restream::server",
              message: "dashboard api server listening",
              fields: "{}",
              pipelineId: null,
              outputId: null,
              eventType: "restream.http.ready",
            },
          ],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`Unexpected fetch: ${href}`);
  };

  const streams = [];
  class FakeEventSource {
    constructor(url) {
      this.url = String(url);
      this.handlers = new Map();
      this.onerror = null;
      this.closed = false;
      streams.push(this);
    }

    addEventListener(type, handler) {
      const handlers = this.handlers.get(type) || [];
      handlers.push(handler);
      this.handlers.set(type, handlers);
    }

    emit(type, payload) {
      const handlers = this.handlers.get(type) || [];
      for (const handler of handlers) {
        handler({ data: JSON.stringify(payload) });
      }
    }

    close() {
      this.closed = true;
    }
  }

  const originalEventSource = globalThis.EventSource;
  const originalSetTimeout = globalThis.setTimeout;
  const originalClearTimeout = globalThis.clearTimeout;
  const originalSetInterval = globalThis.setInterval;
  const originalClearInterval = globalThis.clearInterval;
  globalThis.setTimeout = (fn, _ms) => {
    fn();
    return 1;
  };
  globalThis.clearTimeout = () => {};
  globalThis.setInterval = () => 1;
  globalThis.clearInterval = () => {};
  Object.defineProperty(globalThis, "EventSource", {
    value: FakeEventSource,
    configurable: true,
  });

  try {
    const dashboard = await loadCompiledFrontendModule("features/dashboard.js");
    const modes = await loadCompiledFrontendModule("app/modes.js");

    await dashboard.refreshDashboardRuntime();
    modes.renderDashboardModes();
    await waitForCondition(() => streams.length === 1);

    assert.equal(streams.length, 1, "overview mode should open one restream activity SSE stream");
    assert.equal(
      streams[0].url,
      "/api/v1/logs/stream?scope=restream&last_event_id=41",
      "overview runtime should reuse the restream activity stream instead of a second lifecycle-only feed",
    );

    const initialSummaryHealthCount = requests.filter(
      (href) => href === summaryRuntimeUrl,
    ).length;
    streams[0].emit("log", {
      id: 88,
      ts: "2026-06-30T00:00:08Z",
      level: "INFO",
      target: "restream::pipeline",
      message: "publisher connected",
      fields: "{}",
      pipelineId: "pipe-1",
      outputId: null,
      eventType: "pipeline.publisher.connected",
    });
    await waitForCondition(
      () =>
        requests.filter((href) => href === summaryRuntimeUrl).length ===
        initialSummaryHealthCount + 1,
    );

    assert.equal(
      requests.filter((href) => href === summaryRuntimeUrl).length,
      initialSummaryHealthCount + 1,
      "a lifecycle event should trigger an immediate combined runtime refresh",
    );
    assert.equal(
      requests.some((href) => href.includes("/metrics/system")),
      false,
      "overview lifecycle wakeups should not fall back to standalone metrics fetches",
    );
  } finally {
    for (const stream of streams) {
      stream.close?.();
    }
    if (originalEventSource === undefined) {
      delete globalThis.EventSource;
    } else {
      Object.defineProperty(globalThis, "EventSource", {
        value: originalEventSource,
        configurable: true,
      });
    }
    globalThis.setTimeout = originalSetTimeout;
    globalThis.clearTimeout = originalClearTimeout;
    globalThis.setInterval = originalSetInterval;
    globalThis.clearInterval = originalClearInterval;
  }
});

test("dashboard non-runtime modes skip health polling until a runtime mode resumes", async () => {
  const settingsUrl = "/api/v1/settings?view=dashboard";
  const fullSettingsUrl = "/api/v1/settings";
  const summaryRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=summary";
  const summaryMetricsUrl = "/metrics/system?view=summary";
  const fullMetricsUrl = "/metrics/system";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=settings";
  appendRoot(document, "div", "overview-mode-panel");
  appendRoot(document, "div", "overview-mode-content");
  appendRoot(document, "div", "dashboard-grid");
  appendRoot(document, "div", "inspect-mode-panel");
  appendRoot(document, "div", "control-mode-panel");
  appendRoot(document, "div", "media-mode-panel");
  appendRoot(document, "div", "settings-mode-panel");
  appendRoot(document, "div", "settings-mode-content");
  appendRoot(document, "div", "status-mode-panel");
  appendRoot(document, "div", "restream-process-indicator");
  appendRoot(document, "span", "restream-process-dot");
  appendRoot(document, "span", "restream-process-text");

  const requests = [];
  const streams = [];
  globalThis.fetch = async (url) => {
    const href = String(url);
    requests.push(href);

    if (href === "/api/v1/audio-caps") {
      return new Response(
        JSON.stringify({ caps: {}, platformLabels: {} }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (href === settingsUrl || href === fullSettingsUrl) {
      return new Response(
        JSON.stringify({
          serverName: "Restream",
          ingestHost: "stream.example.com",
          ingestSecurity: {
            failureLimit: 10,
            failureWindowMs: 60000,
            banMs: 600000,
            trackedIpLimit: 10000,
          },
          recordingSettings: {
            retainSourceTs: false,
          },
          srtIngest: {
            mode: "plaintext",
            passphrase: null,
            pbkeylen: 16,
          },
          transcodeProfiles: {},
          pipelines: [],
          outputs: [],
          jobs: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (href === fullMetricsUrl || href === summaryMetricsUrl) {
      return new Response(
        JSON.stringify({
          generatedAt: "2026-06-30T00:00:00Z",
          cpu: { usagePercent: 10 },
          memory: { usedPercent: 20 },
          disk: { usedPercent: 30 },
          engine: { cpuPercent: 2, totalMemoryBytes: 1000, cpuSampleReady: true },
          network: { downloadKbps: 1, uploadKbps: 2 },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    throw new Error(`Unexpected fetch: ${href}`);
  };

  class FakeEventSource {
    constructor(url) {
      this.url = String(url);
      this.handlers = new Map();
      streams.push(this);
    }

    addEventListener(type, handler) {
      const handlers = this.handlers.get(type) || [];
      handlers.push(handler);
      this.handlers.set(type, handlers);
    }

    close() {
      this.closed = true;
    }
  }

  let pollCallback = null;
  const originalEventSource = globalThis.EventSource;
  const originalSetInterval = globalThis.setInterval;
  const originalClearInterval = globalThis.clearInterval;
  Object.defineProperty(globalThis, "EventSource", {
    value: FakeEventSource,
    configurable: true,
  });
  globalThis.setInterval = (fn, _ms) => {
    pollCallback = fn;
    return 1;
  };
  globalThis.clearInterval = () => {};

  try {
    const dashboard = await loadCompiledFrontendModule("features/dashboard.js");
    const modes = await loadCompiledFrontendModule("app/modes.js");
    const indicator = await loadCompiledFrontendModule(
      "features/restream-process-indicator.js",
    );
    window.history.pushState = (_state, _title, url) => {
      window.location.href = String(url);
    };

    dashboard.startDashboardRuntime();
    modes.renderDashboardModes();
    await flushAsyncWork();
    await flushAsyncWork();
    await flushAsyncWork();

    assert.equal(
      requests.filter((href) => href === summaryRuntimeUrl).length,
      0,
      "settings mode should skip boot-time health fetches",
    );
    assert.equal(
      requests.filter((href) => href === settingsUrl).length,
      0,
      "settings mode should skip dashboard config fetches",
    );
    assert.equal(
      requests.filter((href) => href === fullSettingsUrl).length,
      1,
      "settings mode should fetch its own full config once",
    );
    assert.equal(
      streams.length,
      1,
      "settings mode should keep a restream lifecycle stream open for process responsiveness",
    );
    assert.equal(
      String(streams[0]?.url).startsWith(
        "/api/v1/logs/stream?scope=restream&event_class=lifecycle",
      ),
      true,
      "settings mode should subscribe only to restream lifecycle events",
    );
    assert.equal(
      document.getElementById("restream-process-text")?.textContent,
      "Running",
      "settings mode should mark the Rust process as reachable after its metrics refresh",
    );

    indicator.updateRestreamProcessIndicatorFromLog({
      eventType: "restream.shutdown.completed",
    });
    assert.equal(
      document.getElementById("restream-process-text")?.textContent,
      "Stopped",
      "explicit lifecycle shutdown should still surface immediately",
    );

    requests.length = 0;
    await dashboard.refreshDashboardRuntime();
    await flushAsyncWork();

    assert.equal(
      requests.filter((href) => href === summaryRuntimeUrl).length,
      0,
      "settings mode steady-state polls should skip health fetches",
    );
    assert.equal(
      requests.filter((href) => href === summaryMetricsUrl).length,
      1,
      "settings mode should still refresh summary metrics",
    );
    assert.equal(
      requests.filter((href) => href === settingsUrl).length,
      0,
      "settings mode runtime refreshes should continue skipping dashboard config",
    );
    assert.equal(
      document.getElementById("restream-process-text")?.textContent,
      "Running",
      "metrics-only refreshes should revive the Rust process indicator after the API is reachable again",
    );

    requests.length = 0;
    modes.setDashboardMode("overview");
    await flushAsyncWork();
    await flushAsyncWork();
    await flushAsyncWork();

    assert.equal(
      requests.filter((href) => href === summaryRuntimeUrl).length,
      1,
      "returning to a runtime mode should trigger an immediate combined summary runtime refresh",
    );
    assert.equal(
      requests.filter((href) => href === settingsUrl).length,
      1,
      "returning to a runtime mode should also refresh dashboard config",
    );
    assert.equal(
      streams.some((stream) =>
        String(stream.url).startsWith("/api/v1/logs/stream?scope=restream"),
      ),
      true,
      "returning to overview should resume the restream activity stream",
    );
  } finally {
    for (const stream of streams) {
      stream.close?.();
    }
    if (originalEventSource === undefined) {
      delete globalThis.EventSource;
    } else {
      Object.defineProperty(globalThis, "EventSource", {
        value: originalEventSource,
        configurable: true,
      });
    }
    globalThis.setInterval = originalSetInterval;
    globalThis.clearInterval = originalClearInterval;
  }
});

test("status mode reuses its own restream log SSE without opening a second lifecycle stream", async () => {
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=status";
  appendRoot(document, "div", "dashboard-grid");
  appendRoot(document, "div", "overview-mode-panel");
  appendRoot(document, "div", "inspect-mode-panel");
  appendRoot(document, "div", "control-mode-panel");
  appendRoot(document, "div", "media-mode-panel");
  appendRoot(document, "div", "settings-mode-panel");
  appendRoot(document, "div", "status-mode-panel");
  appendRoot(document, "div", "status-mode-content");
  appendRoot(document, "div", "status-versions");
  appendRoot(document, "div", "workspace-mode-summary");
  appendRoot(document, "div", "restream-process-indicator");
  appendRoot(document, "span", "restream-process-dot");
  appendRoot(document, "span", "restream-process-text");

  const requests = [];
  globalThis.fetch = async (url) => {
    const href = String(url);
    requests.push(href);

    if (href === "/api/v1/audio-caps") {
      return new Response(
        JSON.stringify({ caps: {}, platformLabels: {} }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (href === "/metrics/system" || href === "/metrics/system?view=summary") {
      return new Response(
        JSON.stringify({
          generatedAt: "2026-06-30T00:00:00Z",
          cpu: { usagePercent: 12 },
          memory: { usedPercent: 20 },
          disk: { usedPercent: 40 },
          network: { downloadKbps: 1, uploadKbps: 2 },
          engine: {
            cpuPercent: 3,
            totalMemoryBytes: 1234,
            cpuSampleReady: true,
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (href === "/api/v1/engine") {
      return new Response(
        JSON.stringify({
          restream: { version: "0.1.0" },
          sbom: { endpoint: "/api/v1/engine/sbom" },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (href === "/api/v1/logs?scope=restream&limit=80&order=desc") {
      return new Response(
        JSON.stringify({
          logs: [
            {
              id: 91,
              ts: "2026-06-30T00:00:01Z",
              level: "INFO",
              target: "restream::api",
              message: "dashboard api server listening",
              fields: "{}",
              pipelineId: null,
              outputId: null,
              eventType: "restream.http.ready",
            },
          ],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`Unexpected fetch: ${href}`);
  };

  const streams = [];
  class FakeEventSource {
    constructor(url) {
      this.url = String(url);
      this.handlers = new Map();
      this.closed = false;
      streams.push(this);
    }

    addEventListener(type, handler) {
      const handlers = this.handlers.get(type) || [];
      handlers.push(handler);
      this.handlers.set(type, handlers);
    }

    close() {
      this.closed = true;
    }
  }

  const originalEventSource = globalThis.EventSource;
  const originalSetInterval = globalThis.setInterval;
  const originalClearInterval = globalThis.clearInterval;
  let setIntervalCalls = 0;
  Object.defineProperty(globalThis, "EventSource", {
    value: FakeEventSource,
    configurable: true,
  });
  globalThis.setInterval = () => {
    setIntervalCalls += 1;
    return 1;
  };
  globalThis.clearInterval = () => {};

  try {
    const dashboard = await loadCompiledFrontendModule("features/dashboard.js");
    const modes = await loadCompiledFrontendModule("app/modes.js");

    dashboard.startDashboardRuntime();
    modes.renderDashboardModes();
    await flushAsyncWork();
    await flushAsyncWork();
    await flushAsyncWork();

    assert.equal(
      requests.filter((href) => href === "/api/v1/engine").length,
      1,
      "status mode should fetch the engine status snapshot once",
    );
    assert.equal(
      requests.filter(
        (href) => href === "/metrics/system" || href === "/metrics/system?view=summary",
      ).length,
      0,
      "status mode should not keep the dashboard metrics poll alive under its dedicated status transport",
    );
    assert.equal(
      requests.filter(
        (href) => href === "/api/v1/logs?scope=restream&limit=80&order=desc",
      ).length,
      1,
      "status mode should fetch its log snapshot once",
    );
    assert.equal(
      streams.length,
      1,
      "status mode should keep only its restream log stream open",
    );
    assert.equal(
      streams[0].url,
      "/api/v1/logs/stream?scope=restream&last_event_id=91",
    );
    assert.equal(
      streams.some((stream) =>
        String(stream.url).includes("event_class=lifecycle"),
      ),
      false,
      "status mode should not open a second lifecycle-only SSE stream",
    );
    assert.equal(
      setIntervalCalls,
      0,
      "status mode should not register the dashboard runtime poller at all",
    );
  } finally {
    for (const stream of streams) {
      stream.close?.();
    }
    if (originalEventSource === undefined) {
      delete globalThis.EventSource;
    } else {
      Object.defineProperty(globalThis, "EventSource", {
        value: originalEventSource,
        configurable: true,
      });
    }
    globalThis.setInterval = originalSetInterval;
    globalThis.clearInterval = originalClearInterval;
  }
});

test("inspect mode refreshes graphs from dashboard runtime cadence without its own timer", async () => {
  const settingsUrl = "/api/v1/settings?view=dashboard";
  const firstRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=full&pipeline_id=pipe-1";
  const steadyRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=summary&pipeline_id=pipe-1";
  const graphUrl = "/api/v1/pipelines/pipe-1/graph";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=inspect&p=pipe-1";
  appendRoot(document, "div", "dashboard-grid");
  appendRoot(document, "section", "inspect-mode-panel");
  appendRoot(document, "select", "inspect-pipeline-select");
  appendRoot(document, "button", "inspect-open-pipeline-btn");
  appendRoot(document, "div", "inspect-pipeline-summary");
  appendRoot(document, "div", "inspect-diagnostics-summary");
  appendRoot(document, "button", "inspect-refresh-graph-btn");
  appendRoot(document, "button", "inspect-open-diagnostics-btn");
  appendRoot(document, "div", "inspect-graph-status");
  appendRoot(document, "div", "inspect-graph-container");

  const requests = [];
  globalThis.fetch = async (url) => {
    const href = String(url);
    requests.push(href);

    if (href === settingsUrl) {
      return new Response(
        JSON.stringify({
          serverName: "Restream",
          pipelines: [
            {
              id: "pipe-1",
              name: "Primary",
              streamKey: "primary",
              ingestUrls: { rtmp: null, srt: null },
            },
          ],
          outputs: [
            {
              id: "out-1",
              pipelineId: "pipe-1",
              name: "Primary Output",
              url: "rtmp://example.com/live/primary",
              desiredState: "started",
            },
          ],
          jobs: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === firstRuntimeUrl || href === steadyRuntimeUrl) {
      return new Response(
        JSON.stringify({
          health: {
            status: "ready",
            pipelines: {
              "pipe-1": {
                input: {
                  status: "on",
                  probeReady: true,
                  readers: 1,
                  publisher: { protocol: "srt" },
                  bytesReceived: 1024,
                  bytesSent: 2048,
                  bitrateKbps: 3200,
                },
                outputs: {
                  "out-1": {
                    status: "running",
                    rawStatus: "running",
                    bitrateKbps: 1200,
                    totalSize: 4096,
                  },
                },
                recording: { enabled: false, active: false },
              },
            },
          },
          metrics: {
            generatedAt:
              href === firstRuntimeUrl
                ? "2026-06-30T00:00:00Z"
                : "2026-06-30T00:00:05Z",
            cpu: { usagePercent: 12 },
            memory: { usedPercent: 20 },
            disk: { usedPercent: 40 },
            network: { downloadKbps: 1, uploadKbps: 2 },
            engine: {
              cpuPercent: 3,
              totalMemoryBytes: 1234,
              cpuSampleReady: true,
            },
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === graphUrl) {
      return new Response(
        JSON.stringify({
          pipelineId: "pipe-1",
          nodes: [
            {
              id: "pipe-1_ingest",
              type: "ingest",
              label: "Ingest",
              active: true,
              metrics: {
                packetsIn: 1,
                packetsOut: 1,
                bytesIn: 1024,
                bytesOut: 1024,
                processingUs: 10,
                avgUsPerPacket: 10,
                packetsPerSec: 1,
                uptimeSecs: 5,
              },
            },
          ],
          edges: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`Unexpected fetch: ${href}`);
  };

  let setIntervalCalls = 0;
  const originalSetInterval = globalThis.setInterval;
  const originalClearInterval = globalThis.clearInterval;
  globalThis.setInterval = (...args) => {
    setIntervalCalls += 1;
    return originalSetInterval(...args);
  };
  globalThis.clearInterval = (...args) => originalClearInterval(...args);

  try {
    const dashboard = await loadCompiledFrontendModule("features/dashboard.js");
    const modes = await loadCompiledFrontendModule("app/modes.js");

    dashboard.setDashboardHooks({
      afterRender: () => {
        modes.renderDashboardModes();
      },
    });

    await dashboard.refreshDashboardRuntime();
    await flushAsyncWork();

    await dashboard.refreshDashboardRuntime();
    await flushAsyncWork();

    assert.equal(
      requests.filter((href) => href === graphUrl).length,
      2,
      "inspect mode should refresh the graph on runtime refreshes",
    );
    assert.equal(
      setIntervalCalls,
      0,
      "inspect graph refresh should not allocate a second polling timer",
    );

    const autoRefreshButton = document.getElementById(
      "inspect-refresh-graph-btn",
    );
    autoRefreshButton.onclick();
    requests.length = 0;

    await dashboard.refreshDashboardRuntime();
    await flushAsyncWork();

    assert.equal(
      requests.includes(graphUrl),
      false,
      "disabling inspect auto refresh should stop graph fetches on runtime refresh",
    );
  } finally {
    globalThis.setInterval = originalSetInterval;
    globalThis.clearInterval = originalClearInterval;
  }
});

test("pipeline runtime mode uses summary health plus focused selected-pipeline detail", async () => {
  const settingsUrl = "/api/v1/settings?view=dashboard";
  const fullRuntimeWithFullMetricsUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=full&pipeline_id=pipe-1";
  const fullRuntimeWithSummaryMetricsUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=summary&pipeline_id=pipe-1";
  const unscopedSummaryRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=summary";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&p=pipe-1";
  appendRoot(document, "div", "dashboard-grid");

  const requests = [];
  globalThis.fetch = async (url) => {
    const href = String(url);
    requests.push(href);

    if (href === "/api/v1/audio-caps") {
      return new Response(
        JSON.stringify({ caps: {}, platformLabels: {} }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (href === settingsUrl) {
      return new Response(
        JSON.stringify({
          serverName: "Restream",
          pipelines: [
            {
              id: "pipe-1",
              name: "Primary",
              streamKey: "primary",
              ingestUrls: { rtmp: null, srt: null },
            },
          ],
          outputs: [],
          jobs: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    if (
      href === fullRuntimeWithFullMetricsUrl ||
      href === fullRuntimeWithSummaryMetricsUrl
    ) {
      return new Response(
        JSON.stringify({
          health: {
            status: "ready",
            pipelines: {
              "pipe-1": {
                input: {
                  status: "on",
                  probeReady: true,
                  video: null,
                  audioTracks: [],
                  publisher: { protocol: "srt", quality: { msRtt: 10 } },
                },
                outputs: {},
                recording: { enabled: false, active: false },
                hlsPreview: {
                  active: false,
                  persistentConsumers: 0,
                  segments: 0,
                  playlistBytes: 0,
                },
              },
            },
          },
          metrics: {
            generatedAt: "2026-06-30T00:00:00Z",
            cpu: { usagePercent: 12, cores: 4, load1: 0.5 },
            memory: { usedPercent: 20, totalBytes: 200, usedBytes: 40 },
            engine: { cpuPercent: 3, totalMemoryBytes: 1234, cpuSampleReady: true },
            disk: { usedPercent: 40, mountPoint: "/", root: "/" },
            network: { downloadKbps: 1, uploadKbps: 2 },
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`Unexpected fetch: ${href}`);
  };

  const originalSetInterval = globalThis.setInterval;
  const originalClearInterval = globalThis.clearInterval;
  globalThis.setInterval = () => 1;
  globalThis.clearInterval = () => {};

  try {
    const dashboard = await loadCompiledFrontendModule("features/dashboard.js");

    await dashboard.refreshDashboardRuntime();
    await flushAsyncWork();

    assert.equal(
      requests.some((href) =>
        href.startsWith(
          "/api/v1/dashboard/runtime?health_view=summary&metrics_view=",
        ) && href.includes("pipeline_id=pipe-1"),
      ),
      true,
      "pipeline mode should request a summary runtime snapshot that stays focused on the selected pipeline",
    );
    assert.equal(
      requests.includes(unscopedSummaryRuntimeUrl),
      false,
      "pipeline mode should keep the selected pipeline id on runtime refreshes",
    );
  } finally {
    globalThis.setInterval = originalSetInterval;
    globalThis.clearInterval = originalClearInterval;
  }
});

test("focused pipeline lifecycle filter ignores sibling events but keeps selected and restream wakes", async () => {
  const { window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&p=pipe-1";

  const dashboard = await loadCompiledFrontendModule("features/dashboard.js");

  assert.equal(
    dashboard.dashboardLifecycleEventShouldRefresh({
      pipelineId: "pipe-2",
      outputId: null,
      eventType: "pipeline.publisher.connected",
    }),
    false,
    "focused pipeline mode should ignore sibling lifecycle wakeups",
  );
  assert.equal(
    dashboard.dashboardLifecycleEventShouldRefresh({
      pipelineId: "pipe-1",
      outputId: null,
      eventType: "pipeline.publisher.connected",
    }),
    true,
    "focused pipeline mode should still react immediately to the selected pipeline",
  );
  assert.equal(
    dashboard.dashboardLifecycleEventShouldRefresh({
      pipelineId: null,
      outputId: null,
      eventType: "restream.http.ready",
    }),
    true,
    "restream-wide lifecycle events should still wake the focused pipeline view",
  );
  assert.equal(
    dashboard.dashboardLifecycleEventShouldRefresh({
      pipelineId: null,
      outputId: null,
      eventType: "restream.shutdown.completed",
    }),
    false,
    "terminal shutdown events should continue skipping runtime refreshes",
  );
});

test("focused pipeline runtime mode subscribes to selected-pipeline lifecycle events plus restream process events", async () => {
  const { window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&p=pipe-1";

  const streams = [];
  class FakeEventSource {
    constructor(url) {
      this.url = String(url);
      this.handlers = new Map();
      streams.push(this);
    }

    addEventListener(type, handler) {
      const handlers = this.handlers.get(type) || [];
      handlers.push(handler);
      this.handlers.set(type, handlers);
    }

    close() {
      this.closed = true;
    }
  }

  const originalEventSource = globalThis.EventSource;
  Object.defineProperty(globalThis, "EventSource", {
    value: FakeEventSource,
    configurable: true,
  });

  try {
    const dashboard = await loadCompiledFrontendModule("features/dashboard.js");
    dashboard.syncDashboardRuntimeStream();

    assert.equal(streams.length, 1);
    assert.equal(
      String(streams[0].url).startsWith(
        "/api/v1/logs/stream?pipeline_id=pipe-1&include_restream=true&event_class=lifecycle",
      ),
      true,
      "focused pipeline mode should narrow the lifecycle stream to the selected pipeline while keeping restream-wide process events",
    );
  } finally {
    for (const stream of streams) {
      stream.close?.();
    }
    if (originalEventSource === undefined) {
      delete globalThis.EventSource;
    } else {
      Object.defineProperty(globalThis, "EventSource", {
        value: originalEventSource,
        configurable: true,
      });
    }
  }
});

test("focused pipeline runtime refresh keeps sibling summaries while enriching the selected pipeline", async () => {
  const scopedRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=summary&pipeline_id=pipe-1";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&p=pipe-1";
  appendRoot(document, "div", "dashboard-grid");

  const requests = [];
  globalThis.fetch = async (url) => {
    const href = String(url);
    requests.push(href);

    if (href === scopedRuntimeUrl) {
      return new Response(
        JSON.stringify({
          health: {
            status: "ready",
            pipelines: {
              "pipe-1": {
                input: { status: "on", readers: 2, probeReady: true },
                outputs: {
                  "out-1": {
                    status: "running",
                    rawStatus: "running",
                    bitrateKbps: 1500,
                  },
                },
              },
              "pipe-2": {
                input: { status: "warning", readers: 0 },
                outputs: {
                  "out-2": {
                    status: "retrying",
                    rawStatus: "stopped",
                    retrying: true,
                  },
                },
              },
            },
          },
          metrics: {
            generatedAt: "2026-06-30T00:00:05Z",
            cpu: { usagePercent: 14 },
            memory: { usedPercent: 21 },
            disk: { usedPercent: 41 },
            network: { downloadKbps: 3, uploadKbps: 4 },
            engine: {
              cpuPercent: 4,
              totalMemoryBytes: 1236,
              cpuSampleReady: true,
            },
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`Unexpected fetch: ${href}`);
  };

  const originalSetInterval = globalThis.setInterval;
  const originalClearInterval = globalThis.clearInterval;
  globalThis.setInterval = () => 1;
  globalThis.clearInterval = () => {};

  try {
    const { state } = await loadCompiledFrontendModule("core/state.js");
    const dashboard = await loadCompiledFrontendModule("features/dashboard.js");

    state.config = {
      serverName: "Restream",
      pipelines: [
        {
          id: "pipe-1",
          name: "Primary",
          streamKey: "primary",
        },
        {
          id: "pipe-2",
          name: "Backup",
          streamKey: "backup",
        },
      ],
      outputs: [
        {
          id: "out-1",
          pipelineId: "pipe-1",
          name: "Primary Output",
          url: "rtmp://example.com/live/primary",
          desiredState: "started",
        },
        {
          id: "out-2",
          pipelineId: "pipe-2",
          name: "Backup Output",
          url: "rtmp://example.com/live/backup",
          desiredState: "started",
        },
      ],
      jobs: [],
    };
    state.health = {
      status: "ready",
      pipelines: {
        "pipe-1": {
          input: { status: "on", readers: 1 },
          outputs: {
            "out-1": {
              status: "running",
              rawStatus: "running",
            },
          },
        },
        "pipe-2": {
          input: { status: "warning", readers: 0 },
          outputs: {
            "out-2": {
              status: "retrying",
              rawStatus: "stopped",
              retrying: true,
            },
          },
        },
      },
    };
    state.metrics = {};
    state.pipelines = [];

    await dashboard.refreshDashboardRuntime();
    await flushAsyncWork();

    assert.deepEqual(requests, [scopedRuntimeUrl]);
    assert.equal(state.health.pipelines["pipe-1"].input.readers, 2);
    assert.equal(state.health.pipelines["pipe-1"].input.probeReady, true);
    assert.equal(
      state.health.pipelines["pipe-2"].outputs["out-2"].status,
      "retrying",
      "focused runtime refresh should keep sibling pipeline summaries in the same snapshot",
    );
  } finally {
    globalThis.setInterval = originalSetInterval;
    globalThis.clearInterval = originalClearInterval;
  }
});
