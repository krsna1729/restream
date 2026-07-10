import assert from "node:assert/strict";
import test from "node:test";
import {
  installFakeDom,
  loadCompiledFrontendModule,
} from "./helpers/fake-dom.mjs";

test("incidents escape evidence, sort critical first, and distinguish loading/error/empty", async () => {
  installFakeDom();
  const { renderIncidentsHtml } = await loadCompiledFrontendModule(
    "features/incidents.js",
  );
  const base = {
    overview: null,
    alerts: null,
    events: null,
    loaded: false,
    unavailable: false,
  };
  assert.match(renderIncidentsHtml(base, [], ""), /Loading incident snapshots/);
  assert.match(renderIncidentsHtml(base, [], ""), /Critical[\s\S]*>—</);

  const html = renderIncidentsHtml(
    {
      ...base,
      loaded: true,
      unavailable: true,
      alerts: {
        generatedAt: "now",
        alerts: [
          {
            id: "warn",
            severity: "warning",
            scope: "pipeline",
            pipelineId: "p1",
            title: "Warning first",
            cause: "cause",
            evidence: [],
            recommendedAction: "wait",
            generatedAt: "now",
          },
          {
            id: "crit",
            severity: "critical",
            scope: "pipeline",
            pipelineId: "p1",
            title: "Critical <script>",
            cause: "bad <img>",
            evidence: ["<unsafe>"],
            recommendedAction: "restart & inspect",
            generatedAt: "now",
          },
        ],
      },
      events: {
        generatedAt: "now",
        count: 1,
        events: [
          {
            seq: 2,
            timestamp: "2026-01-01T00:00:00Z",
            kind: "egress.failed",
            pipelineId: "p1",
            error: "<boom>",
          },
        ],
      },
    },
    [{ id: "p1", name: "Pipe <one>" }],
    "p1",
  );
  assert.ok(
    html.indexOf("Critical &lt;script&gt;") < html.indexOf("Warning first"),
  );
  assert.doesNotMatch(html, /<script>|<unsafe>|<boom>|Pipe <one>/);
  assert.match(html, /temporarily unavailable/);
  assert.match(html, /fleet/);

  const empty = renderIncidentsHtml(
    {
      ...base,
      loaded: true,
      alerts: { generatedAt: "", alerts: [] },
      events: { generatedAt: "", count: 0, events: [] },
    },
    [],
    "",
  );
  assert.match(empty, /No active alerts/);
  assert.match(empty, /No recent lifecycle events/);
});

test("incident pipeline selection starts a fresh scoped request while the fleet request is pending", async () => {
  const { document } = installFakeDom();
  document.hidden = false;
  const root = document.createElement("div");
  root.id = "incidents-mode-content";
  document.body.appendChild(root);
  const pending = [];
  globalThis.fetch = (url) =>
    new Promise((resolve) =>
      pending.push({
        url: String(url),
        wave: Math.floor(pending.length / 3),
        resolve,
      }),
    );
  const incidents = await loadCompiledFrontendModule("features/incidents.js");
  incidents.renderIncidentsMode({
    active: true,
    pipelines: [{ id: "p1", name: "One" }],
    navigateToPipeline() {},
  });
  await new Promise((resolve) => setImmediate(resolve));
  incidents.selectIncidentPipeline("p1");
  assert.match(root.innerHTML, /Loading incident snapshots/);
  assert.doesNotMatch(root.innerHTML, /No recent lifecycle events/);
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(pending.some((request) => request.url.includes("pipeline_id=p1")));
  incidents.selectIncidentPipeline("");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(
    pending.filter(
      (request) =>
        request.url.includes("/events") && !request.url.includes("pipeline_id"),
    ).length,
    2,
    "A→B→A must not reuse the now-stale first A request",
  );
  const labels = ["Stale A", "Scope B", "Final A"];
  const resolveWave = (wave) => {
    for (const request of pending.filter((item) => item.wave === wave)) {
      const label = labels[wave];
      const data = request.url.includes("/events")
        ? {
            generatedAt: "",
            count: 1,
            events: [
              {
                seq: wave + 1,
                timestamp: "2026-01-01T00:00:00Z",
                kind: `event.${label}`,
                pipelineId: wave === 1 ? "p1" : "fleet",
              },
            ],
          }
        : request.url.includes("/alerts")
          ? {
              generatedAt: "",
              alerts: [
                {
                  id: `alert-${wave}`,
                  severity: "warning",
                  scope: "pipeline",
                  pipelineId: wave === 1 ? "p1" : "fleet",
                  title: label,
                  cause: label,
                  evidence: [],
                  recommendedAction: label,
                  generatedAt: "",
                },
              ],
            }
          : {
              generatedAt: "",
              totalPipelines: wave + 1,
              activePipelines: wave + 1,
              degradedPipelines: wave + 1,
              failedOutputs: wave + 1,
              alertCount: { critical: 0, warning: 1 },
              srtListener: null,
            };
      request.resolve(
        new Response(JSON.stringify(data), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      );
    }
  };

  resolveWave(2);
  await new Promise((resolve) => setImmediate(resolve));
  assert.match(root.innerHTML, /Final A/);
  resolveWave(1);
  resolveWave(0);
  await new Promise((resolve) => setImmediate(resolve));
  assert.match(root.innerHTML, /Final A/);
  assert.doesNotMatch(root.innerHTML, /Stale A|Scope B/);
});
