import assert from "node:assert/strict";
import test from "node:test";

import {
  appendRoot,
  flushAsyncWork,
  installFakeDom,
  loadCompiledFrontendModule,
  waitForCondition,
} from "./helpers.mjs";

test("output start and stop controls refresh runtime without invalidating dashboard settings", async () => {
  const settingsUrl = "/api/v1/settings?view=dashboard";
  const fullRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=full&pipeline_id=pipe-1";
  const steadyRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=summary&pipeline_id=pipe-1";
  const startUrl = "/api/v1/pipelines/pipe-1/outputs/out-1/start";
  const stopUrl = "/api/v1/pipelines/pipe-1/outputs/out-1/stop";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&p=pipe-1";
  appendRoot(document, "div", "dashboard-grid");

  const requests = [];
  globalThis.fetch = async (url, options = {}) => {
    const href = String(url);
    const method = String(options.method || "GET").toUpperCase();
    requests.push([method, href]);

    if (href === settingsUrl) {
      return new Response(
        JSON.stringify({
          serverName: "Restream",
          ingestHost: "stream.example.com",
          pipelines: [
            {
              id: "pipe-1",
              name: "Pipeline 1",
              streamKey: "stream-key",
              inputSource: null,
            },
          ],
          outputs: [
            {
              id: "out-1",
              pipelineId: "pipe-1",
              name: "Primary Output",
              url: "rtmp://example.com/live/secret",
              encoding: "source",
              desiredState: "started",
            },
          ],
          jobs: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === fullRuntimeUrl || href === steadyRuntimeUrl) {
      return new Response(
        JSON.stringify({
          health: {
            status: "ready",
            pipelines: {
              "pipe-1": {
                input: {
                  status: "on",
                  probeReady: true,
                  probeStatus: "ready",
                  bytesReceived: 0,
                  bytesSent: 0,
                  readers: 0,
                  bitrateKbps: 3200,
                },
                outputs: {
                  "out-1": {
                    status: "running",
                    rawStatus: "running",
                    phase: "sending",
                    bitrateKbps: 1500,
                    totalSize: 2048,
                  },
                },
              },
            },
          },
          metrics: {
            generatedAt:
              href === fullRuntimeUrl
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

    if (href === startUrl || href === stopUrl) {
      return new Response(
        JSON.stringify({ ok: true }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`Unexpected fetch: ${method} ${href}`);
  };

  const dashboard = await loadCompiledFrontendModule("features/dashboard.js");
  const editor = await loadCompiledFrontendModule("features/editor.js");

  await dashboard.refreshDashboard();
  requests.length = 0;

  await editor.startOutBtn("pipe-1", "out-1");
  await flushAsyncWork();

  assert.deepEqual(requests, [["POST", startUrl]]);

  requests.length = 0;

  await editor.stopOutBtn("pipe-1", "out-1");
  await flushAsyncWork();

  assert.deepEqual(requests, [
    ["POST", stopUrl],
    ["GET", steadyRuntimeUrl],
  ]);
  assert.equal(
    requests.some(([, href]) => href === settingsUrl),
    false,
    "output controls should not refetch dashboard settings after steady-state boot",
  );
});

test("output start and stop controls prefer lifecycle SSE convergence before fallback runtime refresh", async () => {
  const settingsUrl = "/api/v1/settings?view=dashboard";
  const fullRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=full&pipeline_id=pipe-1";
  const steadyRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=summary&pipeline_id=pipe-1";
  const startUrl = "/api/v1/pipelines/pipe-1/outputs/out-1/start";
  const stopUrl = "/api/v1/pipelines/pipe-1/outputs/out-1/stop";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&p=pipe-1";
  appendRoot(document, "div", "dashboard-grid");

  let desiredState = "stopped";
  const requests = [];
  globalThis.fetch = async (url, options = {}) => {
    const href = String(url);
    const method = String(options.method || "GET").toUpperCase();
    requests.push([method, href]);

    if (href === settingsUrl) {
      return new Response(
        JSON.stringify({
          serverName: "Restream",
          ingestHost: "stream.example.com",
          pipelines: [
            {
              id: "pipe-1",
              name: "Pipeline 1",
              streamKey: "stream-key",
              inputSource: null,
            },
          ],
          outputs: [
            {
              id: "out-1",
              pipelineId: "pipe-1",
              name: "Primary Output",
              url: "rtmp://example.com/live/secret",
              encoding: "source",
              desiredState,
            },
          ],
          jobs: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === fullRuntimeUrl || href === steadyRuntimeUrl) {
      const outputRunning = desiredState !== "stopped";
      return new Response(
        JSON.stringify({
          health: {
            status: "ready",
            pipelines: {
              "pipe-1": {
                input: {
                  status: "on",
                  probeReady: true,
                  probeStatus: "ready",
                  bytesReceived: 0,
                  bytesSent: 0,
                  readers: 0,
                  bitrateKbps: 3200,
                },
                outputs: {
                  "out-1": outputRunning
                    ? {
                        status: "running",
                        rawStatus: "running",
                        phase: "sending",
                        bitrateKbps: 1500,
                        totalSize: 2048,
                      }
                    : {
                        status: "off",
                        rawStatus: "stopped",
                        phase: null,
                        bitrateKbps: null,
                        totalSize: 0,
                      },
                },
              },
            },
          },
          metrics: {
            generatedAt:
              href === fullRuntimeUrl
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

    if (href === startUrl) {
      desiredState = "running";
      return new Response(
        JSON.stringify({
          message: "Output started",
          desiredState: "running",
          output: {
            id: "out-1",
            pipelineId: "pipe-1",
            name: "Primary Output",
            url: "rtmp://example.com/live/secret",
            encoding: "source",
            desiredState: "running",
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === stopUrl) {
      desiredState = "stopped";
      return new Response(
        JSON.stringify({
          message: "Output stopped",
          desiredState: "stopped",
          output: {
            id: "out-1",
            pipelineId: "pipe-1",
            name: "Primary Output",
            url: "rtmp://example.com/live/secret",
            encoding: "source",
            desiredState: "stopped",
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`Unexpected fetch: ${method} ${href}`);
  };

  const streams = [];
  const scheduledTimeouts = [];
  const flushMicrotasks = async () => {
    await Promise.resolve();
    await Promise.resolve();
  };

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

  const runTimeouts = (delayMs) => {
    const due = scheduledTimeouts.filter(
      (entry) => !entry.cleared && entry.delayMs === delayMs,
    );
    for (const entry of due) {
      entry.cleared = true;
      entry.fn();
    }
  };
  const originalEventSource = globalThis.EventSource;
  const originalSetTimeout = globalThis.setTimeout;
  const originalClearTimeout = globalThis.clearTimeout;
  Object.defineProperty(globalThis, "EventSource", {
    value: FakeEventSource,
    configurable: true,
  });
  globalThis.setTimeout = (fn, delayMs = 0) => {
    const token = {
      id: scheduledTimeouts.length + 1,
      fn,
      delayMs: Number(delayMs),
      cleared: false,
    };
    scheduledTimeouts.push(token);
    return token.id;
  };
  globalThis.clearTimeout = (timeoutId) => {
    const token = scheduledTimeouts.find((entry) => entry.id === timeoutId);
    if (token) token.cleared = true;
  };

  try {
    const dashboard = await loadCompiledFrontendModule("features/dashboard.js");
    const editor = await loadCompiledFrontendModule("features/editor.js");
    const { state } = await loadCompiledFrontendModule("core/state.js");

    await dashboard.refreshDashboard();
    dashboard.syncDashboardRuntimeStream();

    assert.equal(streams.length, 1);
    requests.length = 0;

    const startPromise = editor.startOutBtn("pipe-1", "out-1");
    await waitForCondition(
      () => state.config.outputs[0].desiredState === "running",
    );

    assert.deepEqual(requests, [["POST", startUrl]]);
    assert.equal(
      state.config.outputs[0].desiredState,
      "running",
      "the mutation response should patch local desired state before runtime convergence completes",
    );

    streams[0].emit("log", {
      id: 88,
      ts: "2026-06-30T00:00:08Z",
      level: "INFO",
      target: "restream::egress",
      message: "output started",
      fields: "{}",
      pipelineId: "pipe-1",
      outputId: "out-1",
      eventType: "egress.started",
    });
    runTimeouts(200);
    await startPromise;
    await flushMicrotasks();
    runTimeouts(1500);
    await flushMicrotasks();

    assert.deepEqual(requests, [
      ["POST", startUrl],
      ["GET", steadyRuntimeUrl],
    ]);

    requests.length = 0;
    const stopPromise = editor.stopOutBtn("pipe-1", "out-1");
    await waitForCondition(
      () => state.config.outputs[0].desiredState === "stopped",
    );

    assert.deepEqual(requests, [["POST", stopUrl]]);
    assert.equal(
      state.config.outputs[0].desiredState,
      "stopped",
      "stop should also patch local desired state before runtime convergence completes",
    );

    streams[0].emit("log", {
      id: 89,
      ts: "2026-06-30T00:00:10Z",
      level: "INFO",
      target: "restream::egress",
      message: "output stopped",
      fields: "{}",
      pipelineId: "pipe-1",
      outputId: "out-1",
      eventType: "egress.stopped",
    });
    runTimeouts(200);
    await stopPromise;
    await flushMicrotasks();
    runTimeouts(1500);
    await flushMicrotasks();

    assert.deepEqual(requests, [
      ["POST", stopUrl],
      ["GET", steadyRuntimeUrl],
    ]);
    assert.equal(
      requests.some(([, href]) => href === settingsUrl),
      false,
      "SSE-driven output convergence should still avoid refetching dashboard settings",
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
  }
});

test("output config mutations reuse returned output payloads instead of refetching dashboard settings", async () => {
  const settingsUrl = "/api/v1/settings?view=dashboard";
  const fullRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=full&pipeline_id=pipe-1";
  const updateUrl = "/api/v1/pipelines/pipe-1/outputs/out-1";
  const createUrl = "/api/v1/pipelines/pipe-1/outputs";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&p=pipe-1";
  appendRoot(document, "div", "dashboard-grid");

  const appendField = (tagName, id, value = "") => {
    const element = document.createElement(tagName);
    element.id = id;
    element.value = value;
    document.body.appendChild(element);
    return element;
  };

  appendField("dialog", "edit-out-modal").close = () => {};
  appendField("input", "out-mode-input", "edit");
  appendField("input", "out-pipe-id-input", "pipe-1");
  appendField("input", "out-id-input", "out-1");
  appendField("select", "out-protocol-input", "rtmp");
  appendField("select", "out-server-url-input", "");
  appendField(
    "input",
    "out-rtmp-key-input",
    "rtmp://example.com/live/updated-secret",
  );
  appendField("select", "out-encoding-input", "source");
  appendField("input", "out-name-input", "Renamed Output");
  appendField(
    "input",
    "out-monitoring-url-input",
    "https://example.com/live/out.m3u8",
  );
  appendRoot(document, "div", "out-rtmp-error").classList.add("hidden");
  appendRoot(document, "div", "out-monitoring-error").classList.add("hidden");

  const requests = [];
  globalThis.fetch = async (url, options = {}) => {
    const href = String(url);
    const method = String(options.method || "GET").toUpperCase();
    requests.push([method, href]);

    if (href === settingsUrl) {
      return new Response(
        JSON.stringify({
          serverName: "Restream",
          ingestHost: "stream.example.com",
          pipelines: [
            {
              id: "pipe-1",
              name: "Pipeline 1",
              streamKey: "stream-key",
              inputSource: null,
            },
          ],
          outputs: [
            {
              id: "out-1",
              pipelineId: "pipe-1",
              name: "Primary Output",
              url: "rtmp://example.com/live/original-secret",
              monitoringUrl: null,
              encoding: "source",
              desiredState: "started",
            },
          ],
          jobs: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === fullRuntimeUrl) {
      return new Response(
        JSON.stringify({
          health: {
            status: "ready",
            pipelines: {
              "pipe-1": {
                input: {
                  status: "on",
                  probeReady: true,
                  probeStatus: "ready",
                  bytesReceived: 0,
                  bytesSent: 0,
                  readers: 0,
                  bitrateKbps: 3200,
                },
                outputs: {
                  "out-1": {
                    status: "running",
                    rawStatus: "running",
                    phase: "sending",
                    bitrateKbps: 1500,
                    totalSize: 2048,
                  },
                },
              },
            },
          },
          metrics: {
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
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === updateUrl) {
      return new Response(
        JSON.stringify({
          message: "Output updated",
          output: {
            id: "out-1",
            pipelineId: "pipe-1",
            name: "Renamed Output",
            url: "rtmp://example.com/live/updated-secret",
            monitoringUrl: "https://example.com/live/out.m3u8",
            encoding: "source",
            desiredState: "started",
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === createUrl) {
      return new Response(
        JSON.stringify({
          message: "Output created",
          output: {
            id: "out-2",
            pipelineId: "pipe-1",
            name: "Backup Output",
            url: "rtmp://backup.example.com/live/backup-secret",
            monitoringUrl: null,
            encoding: "source",
            desiredState: "stopped",
          },
        }),
        { status: 201, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`Unexpected fetch: ${method} ${href}`);
  };

  const dashboard = await loadCompiledFrontendModule("features/dashboard.js");
  const editor = await loadCompiledFrontendModule("features/editor.js");
  const { state } = await loadCompiledFrontendModule("core/state.js");

  await dashboard.refreshDashboard();
  requests.length = 0;

  await editor.editOutFormBtn({ preventDefault() {} });
  await flushAsyncWork();

  assert.deepEqual(requests, [["PATCH", updateUrl]]);
  assert.equal(
    requests.some(([, href]) => href === settingsUrl),
    false,
    "editing an output should not refetch dashboard settings when the API returned the updated output",
  );
  assert.equal(state.config.outputs[0].name, "Renamed Output");
  assert.equal(
    state.pipelines[0].outs[0].monitoringUrl,
    "https://example.com/live/out.m3u8",
  );

  requests.length = 0;
  document.getElementById("out-mode-input").value = "create";
  document.getElementById("out-id-input").value = "";
  document.getElementById("out-name-input").value = "Backup Output";
  document.getElementById("out-rtmp-key-input").value =
    "rtmp://backup.example.com/live/backup-secret";
  document.getElementById("out-monitoring-url-input").value = "";

  await editor.editOutFormBtn({ preventDefault() {} });
  await flushAsyncWork();

  assert.deepEqual(requests, [["POST", createUrl]]);
  assert.equal(
    requests.some(([, href]) => href === settingsUrl),
    false,
    "creating an output should not refetch dashboard settings when the API returned the created output",
  );
  assert.equal(state.config.outputs.length, 2);
  assert.equal(
    state.config.outputs.some((output) => output.id === "out-2"),
    true,
  );
});

test("pipeline and output deletes patch dashboard state locally instead of refetching settings", async () => {
  const settingsUrl = "/api/v1/settings?view=dashboard";
  const fullRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=full&pipeline_id=pipe-1";
  const deleteOutputUrl = "/api/v1/pipelines/pipe-1/outputs/out-1";
  const deletePipelineUrl = "/api/v1/pipelines/pipe-1";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&p=pipe-1";
  appendRoot(document, "div", "dashboard-grid");

  const originalCreateElement = document.createElement.bind(document);
  document.createElement = (tagName) => {
    const element = originalCreateElement(tagName);
    if (String(tagName).toLowerCase() === "dialog") {
      element.showModal = () => {
        element.returnValue = "confirm";
        setTimeout(() => {
          element.dispatchEvent({ type: "close" });
        }, 0);
      };
      element.close = () => {
        element.dispatchEvent({ type: "close" });
      };
    }
    return element;
  };

  const requests = [];
  globalThis.fetch = async (url, options = {}) => {
    const href = String(url);
    const method = String(options.method || "GET").toUpperCase();
    requests.push([method, href]);

    if (href === settingsUrl) {
      return new Response(
        JSON.stringify({
          serverName: "Restream",
          ingestHost: "stream.example.com",
          pipelines: [
            {
              id: "pipe-1",
              name: "Pipeline 1",
              streamKey: "stream-key",
              inputSource: null,
            },
            {
              id: "pipe-2",
              name: "Pipeline 2",
              streamKey: "stream-key-2",
              inputSource: null,
            },
          ],
          outputs: [
            {
              id: "out-1",
              pipelineId: "pipe-1",
              name: "Primary Output",
              url: "rtmp://example.com/live/original-secret",
              monitoringUrl: null,
              encoding: "source",
              desiredState: "started",
            },
            {
              id: "out-2",
              pipelineId: "pipe-2",
              name: "Secondary Output",
              url: "rtmp://example.com/live/second-secret",
              monitoringUrl: null,
              encoding: "source",
              desiredState: "started",
            },
          ],
          jobs: [
            {
              pipelineId: "pipe-1",
              outputId: "out-1",
              startedAt: "2026-06-30T00:00:10Z",
            },
          ],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === fullRuntimeUrl) {
      return new Response(
        JSON.stringify({
          health: {
            status: "ready",
            pipelines: {
              "pipe-1": {
                input: {
                  status: "on",
                  probeReady: true,
                  probeStatus: "ready",
                  bytesReceived: 0,
                  bytesSent: 0,
                  readers: 0,
                  bitrateKbps: 3200,
                },
                outputs: {
                  "out-1": {
                    status: "running",
                    rawStatus: "running",
                    phase: "sending",
                    bitrateKbps: 1500,
                    totalSize: 2048,
                  },
                },
              },
              "pipe-2": {
                input: {
                  status: "off",
                  probeReady: false,
                  probeStatus: "off",
                  bytesReceived: 0,
                  bytesSent: 0,
                  readers: 0,
                  bitrateKbps: null,
                },
                outputs: {
                  "out-2": {
                    status: "stopped",
                    rawStatus: "stopped",
                    phase: null,
                    bitrateKbps: null,
                    totalSize: 0,
                  },
                },
              },
            },
          },
          metrics: {
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
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === deleteOutputUrl) {
      return new Response(
        JSON.stringify({ message: "Output deleted" }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === deletePipelineUrl) {
      return new Response(
        JSON.stringify({ message: "Pipeline deleted" }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`Unexpected fetch: ${method} ${href}`);
  };

  const dashboard = await loadCompiledFrontendModule("features/dashboard.js");
  const editor = await loadCompiledFrontendModule("features/editor.js");
  const { state } = await loadCompiledFrontendModule("core/state.js");

  await dashboard.refreshDashboard();
  requests.length = 0;

  await editor.deleteOutBtn("pipe-1", "out-1");
  await flushAsyncWork();
  await flushAsyncWork();

  assert.deepEqual(requests, [["DELETE", deleteOutputUrl]]);
  assert.equal(
    requests.some(([, href]) => href === settingsUrl),
    false,
    "deleting an output should not refetch dashboard settings",
  );
  assert.equal(
    state.config.outputs.some((output) => output.id === "out-1"),
    false,
  );
  assert.equal(
    state.config.jobs.some((job) => job.outputId === "out-1"),
    false,
  );

  requests.length = 0;
  await editor.deletePipeBtn();
  await flushAsyncWork();
  await flushAsyncWork();

  assert.deepEqual(requests, [["DELETE", deletePipelineUrl]]);
  assert.equal(
    requests.some(([, href]) => href === settingsUrl),
    false,
    "deleting a pipeline should not refetch dashboard settings",
  );
  assert.equal(
    state.config.pipelines.some((pipeline) => pipeline.id === "pipe-1"),
    false,
  );
  assert.equal(
    state.config.outputs.some((output) => output.pipelineId === "pipe-1"),
    false,
  );
  assert.equal(
    state.health.pipelines["pipe-1"],
    undefined,
  );
});

