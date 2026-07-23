import assert from "node:assert/strict";
import test from "node:test";

import {
  appendRoot,
  flushAsyncWork,
  installFakeDom,
  loadCompiledFrontendModule,
  waitForCondition,
} from "./helpers.mjs";

test("dashboard steady-state polling avoids repeated settings fetches", async () => {
  const settingsUrl = "/api/v1/settings?view=dashboard";
  const summaryRuntimeWithFullMetricsUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=full";
  const summaryRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=summary";
  const fullMetricsUrl = "/metrics/system";
  const summaryMetricsUrl = "/metrics/system?view=summary";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=overview";
  appendRoot(document, "div", "dashboard-v2-operate-panel");

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
            mediaDisk: {
              usedBytes: 100,
              totalBytes: 200,
              usedPercent: 50,
              mountPoint: "/media",
              mediaRoot: "/srv/media",
            },
            network: {
              downloadKbps: 1,
              uploadKbps: 2,
              interfaces: [{ name: "eth0" }],
              ignoredInterfaces: ["lo"],
            },
            disk: { usedPercent: 40, mountPoint: "/", root: "/" },
            cpu: { usagePercent: 12, cores: 4, load1: 0.5 },
            memory: { usedPercent: 20, totalBytes: 200, usedBytes: 40 },
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
        {
          status: 200,
          headers: { "content-type": "application/json" },
        },
      );
    }

    throw new Error(`Unexpected fetch: ${href}`);
  };

  let pollCallback = null;
  const originalSetInterval = globalThis.setInterval;
  const originalClearInterval = globalThis.clearInterval;
  globalThis.setInterval = (fn, _ms) => {
    pollCallback = fn;
    return 1;
  };
  globalThis.clearInterval = () => {};

  try {
    const { state } = await loadCompiledFrontendModule("core/state.js");
    const dashboard = await loadCompiledFrontendModule("features/dashboard.js");

    dashboard.startDashboardRuntime();
    await flushAsyncWork();

    assert.equal(
      requests.filter((href) => href === settingsUrl).length,
      1,
      "initial boot should fetch settings once",
    );
    assert.equal(
      requests.filter((href) => href === summaryRuntimeWithFullMetricsUrl).length,
      1,
      "initial boot should fetch the combined summary runtime snapshot once",
    );
    assert.equal(
      requests.filter((href) => href === fullMetricsUrl).length,
      0,
      "initial boot should no longer fetch metrics separately when runtime health is needed",
    );
    assert.equal(typeof pollCallback, "function");

    await pollCallback();
    await flushAsyncWork();

    assert.equal(
      requests.filter((href) => href === settingsUrl).length,
      1,
      "steady-state poll should reuse cached settings",
    );
    assert.equal(
      requests.filter((href) => href === summaryRuntimeUrl).length,
      1,
      "steady-state poll should use the combined summary runtime view",
    );
    assert.equal(
      requests.filter((href) => href === summaryMetricsUrl).length,
      0,
      "steady-state poll should no longer fetch summary metrics separately",
    );
    assert.equal(
      state.metrics.mediaDisk?.mountPoint,
      "/media",
      "summary refresh should preserve previously fetched media disk details",
    );
    assert.deepEqual(
      state.metrics.network?.interfaces,
      [{ name: "eth0" }],
      "summary refresh should preserve previously fetched network interface details",
    );

    await dashboard.refreshDashboard();

    assert.equal(
      requests.filter((href) => href === settingsUrl).length,
      2,
      "explicit dashboard refresh should invalidate settings",
    );
    assert.equal(
      requests.filter((href) => href === summaryRuntimeUrl).length,
      2,
      "explicit dashboard refresh should still refresh the combined summary runtime snapshot",
    );
    assert.equal(
      requests.filter((href) => href === summaryMetricsUrl).length,
      0,
      "explicit dashboard refresh should no longer fetch summary metrics separately",
    );

    await dashboard.refreshDashboardRuntime();

    assert.equal(
      requests.filter((href) => href === settingsUrl).length,
      2,
      "runtime-only refresh should not invalidate settings",
    );
    assert.equal(
      requests.filter((href) => href === summaryRuntimeUrl).length,
      3,
      "runtime-only refresh should refresh the combined summary runtime snapshot",
    );
    assert.equal(
      requests.filter((href) => href === summaryMetricsUrl).length,
      0,
      "runtime-only refresh should no longer fetch summary metrics separately",
    );
  } finally {
    globalThis.setInterval = originalSetInterval;
    globalThis.clearInterval = originalClearInterval;
  }
});

