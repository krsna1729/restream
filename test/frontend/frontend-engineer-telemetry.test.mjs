import assert from "node:assert/strict";
import test from "node:test";
import {
  installFakeDom,
  loadCompiledFrontendModule,
} from "../support/helpers/fake-dom.mjs";

test("telemetry renders zero/null ring values, escapes labels, and distinguishes loading", async () => {
  installFakeDom();
  const { renderEngineerTelemetryHtml } = await loadCompiledFrontendModule(
    "features/engineer-telemetry.js",
  );
  const loading = renderEngineerTelemetryHtml(
    null,
    null,
    null,
    null,
    [{ id: "p1", name: "One" }],
    "p1",
  );
  assert.match(loading, /Loading telemetry snapshots/);
  assert.match(loading, /Loading pipeline telemetry/);
  assert.doesNotMatch(
    loading,
    /No active (source ring|readers|egresses|stages)/,
  );
  const html = renderEngineerTelemetryHtml(
    {
      generatedAt: "",
      ingests: [],
      stages: [],
      egresses: [],
      activeTranscoderBuffers: 0,
    },
    {
      generatedAt: "",
      pipelineId: "p1",
      ingest: null,
      sourceRing: {
        fill: 0,
        capacity: 0,
        fillPercent: 0,
        estimatedPktRatePerSec: 0,
        bufferDepthSecs: 0,
        payloadStats: {},
        readers: [
          {
            name: "reader <x>",
            lagSlots: 0,
            overflowCount: 0,
            packetAgeMs: null,
          },
        ],
      },
      stages: [{ kind: "video <bad>", metrics: { packetsIn: 0 } }],
      egresses: [],
    },
    null,
    {
      status: "ready",
      hostSettings: [
        {
          key: "net.core.rmem_max",
          label: "Kernel receive buffer ceiling",
          current: 26214400,
          required: 26214400,
          unit: "bytes",
          status: "ok",
        },
      ],
    },
    [{ id: "p1", name: "Pipe <bad>" }],
    "p1",
    { loaded: true },
  );
  assert.match(html, /0 \/ 0 \(0%\)/);
  assert.match(html, /packet age — ms/);
  assert.match(html, /Transcoder buffers[\s\S]*>0</);
  assert.doesNotMatch(html, /reader <x>|video <bad>|Pipe <bad>/);
  assert.match(html, /View video &lt;bad&gt; telemetry details/);
  assert.match(html, /Kernel receive buffer ceiling/);
  assert.match(html, /25 MiB/);
  assert.match(
    html,
    /Telemetry loaded · 0 ingests · 1 stage · 0 egresses · 1 reader · Pipe &lt;bad&gt;/,
  );

  const retained = renderEngineerTelemetryHtml(
    {
      generatedAt: "",
      ingests: [],
      stages: [],
      egresses: [],
      activeTranscoderBuffers: 0,
    },
    {
      generatedAt: "",
      pipelineId: "p1",
      ingest: null,
      sourceRing: null,
      stages: [],
      egresses: [],
    },
    {
      generatedAt: "",
      stageKey: "p1:video:720p",
      pipelineId: "p1",
      kind: "video:720p",
      metrics: { packetsIn: 12 },
    },
    null,
    [{ id: "p1", name: "One" }],
    "p1",
    { loaded: true, stageUnavailable: true },
  );
  assert.match(retained, /Fresh stage detail is unavailable/);
  assert.match(retained, /packetsIn[\s\S]*12/);
});

test("telemetry starts a new pipeline request and ignores the stale selection", async () => {
  const { document } = installFakeDom();
  document.hidden = false;
  const root = document.createElement("div");
  root.id = "telemetry-mode-content";
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
  const telemetry = await loadCompiledFrontendModule(
    "features/engineer-telemetry.js",
  );
  telemetry.renderEngineerTelemetryMode({
    active: true,
    pipelines: [
      { id: "p1", name: "One" },
      { id: "p2", name: "Two" },
    ],
  });
  await new Promise((resolve) => setImmediate(resolve));
  telemetry.selectTelemetryPipeline("p2");
  await new Promise((resolve) => setImmediate(resolve));
  const pipelinePath = (id) => ["", "pipelines", id, "telemetry"].join("/");
  assert.ok(
    pending.some((request) => request.url.includes(pipelinePath("p1"))),
  );
  assert.ok(
    pending.some((request) => request.url.includes(pipelinePath("p2"))),
  );
  telemetry.selectTelemetryPipeline("p1");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(
    pending.filter((request) => request.url.includes(pipelinePath("p1")))
      .length,
    2,
    "A→B→A must issue a current A request",
  );
  const labels = ["stale-a-reader", "scope-b-reader", "final-a-reader"];
  const resolveWave = (wave) => {
    for (const request of pending.filter((item) => item.wave === wave)) {
      const pipelineId = wave === 1 ? "p2" : "p1";
      const data = request.url.includes("/engine/health")
        ? {
            status: "ready",
            hostSettings: [
              {
                key: "runtime.nofile",
                label: "Open file descriptors",
                current: 65536,
                required: 65536,
                unit: "fds",
                status: "ok",
              },
            ],
          }
        : request.url.includes("/engine/")
        ? {
            generatedAt: "",
            ingests: Array.from({ length: wave + 1 }, () => ({
              protocol: "rtmp",
              uptimeSecs: 1,
              bytesReceived: 1,
              metrics: {},
            })),
            stages: [],
            egresses: [],
            activeTranscoderBuffers: wave,
          }
        : {
            generatedAt: "",
            pipelineId,
            ingest: null,
            sourceRing: {
              fill: wave,
              capacity: 10,
              fillPercent: wave * 10,
              estimatedPktRatePerSec: 1,
              bufferDepthSecs: 1,
              payloadStats: {},
              readers: [
                {
                  name: labels[wave],
                  lagSlots: wave,
                  overflowCount: 0,
                  packetAgeMs: 1,
                },
              ],
            },
            stages: [],
            egresses: [],
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
  assert.match(root.innerHTML, /value="p1" selected/);
  assert.match(root.innerHTML, /final-a-reader/);
  resolveWave(1);
  resolveWave(0);
  await new Promise((resolve) => setImmediate(resolve));
  assert.match(root.innerHTML, /final-a-reader/);
  assert.doesNotMatch(root.innerHTML, /stale-a-reader|scope-b-reader/);
});
