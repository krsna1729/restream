import assert from "node:assert/strict";
import test from "node:test";

import { loadFrontendModule } from "../support/helpers/frontend-module-loader.mjs";

function makeStorage() {
  const data = new Map();
  return {
    getItem(key) {
      return data.has(key) ? data.get(key) : null;
    },
    setItem(key, value) {
      data.set(key, String(value));
    },
    removeItem(key) {
      data.delete(key);
    },
  };
}

function makeElement(tagName = "div") {
  return {
    tagName,
    value: "",
    innerText: "",
    textContent: "",
    dataset: {},
    style: {},
    classList: {
      add() {},
      remove() {},
      toggle() {},
    },
    appendChild() {},
    removeChild() {},
    setAttribute() {},
    removeAttribute() {},
    focus() {},
    select() {},
    click() {},
    getAttribute() {
      return null;
    },
  };
}

function installBrowserStubs() {
  const documentStub = {
    title: "",
    body: makeElement("body"),
    getElementById() {
      return null;
    },
    querySelector() {
      return null;
    },
    createElement(tagName) {
      return makeElement(tagName);
    },
    execCommand() {
      return true;
    },
  };

  const windowStub = {
    __RESTREAM_BASE_PATH__: "",
    location: {
      href: "http://localhost/",
    },
    history: {
      pushState() {},
    },
    localStorage: makeStorage(),
    sessionStorage: makeStorage(),
  };

  Object.defineProperty(globalThis, "document", {
    value: documentStub,
    configurable: true,
  });
  Object.defineProperty(globalThis, "window", {
    value: windowStub,
    configurable: true,
  });
  Object.defineProperty(globalThis, "navigator", {
    value: {
      clipboard: {
        async writeText() {},
      },
    },
    configurable: true,
  });
}

async function loadCompiledModule(relativePath) {
  installBrowserStubs();
  return loadFrontendModule(relativePath);
}

async function loadApiModule() {
  return loadCompiledModule("core/api.js");
}

test("frontend API helpers call the canonical v1 routes and methods", async () => {
  const requests = [];
  globalThis.fetch = async (url, options = {}) => {
    requests.push({
      url: String(url),
      method: options.method || "GET",
      body: options.body ? JSON.parse(options.body) : null,
    });
    return new Response(JSON.stringify({ ok: true, logs: [], files: [] }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };

  const api = await loadApiModule();

  await api.getConfig();
  await api.getConfig({ view: "dashboard" });
  await api.getConfig({ jobs: "latest" });
  await api.getSystemMetrics({ view: "summary" });
  await api.getPipelineInputs("pipe /1");
  await api.createPipelineInput("pipe /1", {
    label: "Encoder B",
    kind: "keyed",
    sourceUrl: null,
  });
  await api.updatePipelineInput("pipe /1", "input /1", {
    label: "Encoder C",
    enabled: false,
  });
  await api.updatePipelineInput("pipe /1", "input /1", {
    label: "Origin",
    kind: "hlsPull",
    sourceUrl: "https://origin.example/live/master.m3u8",
    variantRule: "lowestBandwidth",
    auth: { kind: "headers", headers: [{ name: "X-Token", value: "s3cret" }] },
  });
  await api.promotePipelineInput("pipe /1", "input /1");
  await api.deletePipelineInput("pipe /1", "input /1");
  await api.updatePipeline("pipe-1", {
    name: "Updated",
    fileIngest: {
      filename: "clip.ts",
      loopFlag: true,
      startTime: "00:00:01",
      liveOptimized: true,
      targetGopSeconds: 2,
    },
  });
  await api.updateOutput("pipe-1", "out-1", { name: "Output" });
  await api.getLogs(
    { pipelineId: "pipe-1", outputId: "out-1", eventClass: "lifecycle" },
    25,
  );
  await api.getLogs({ scope: "restream", level: "warn" }, 50);
  await api.listMediaFiles();
  await api.getAggregateAlerts();
  await api.getLifecycleEvents({ pipelineId: "pipe /1", limit: 40 });
  await api.getEngineTelemetry();
  await api.getStageTelemetry("pipe /1:video:720p");
  await api.getResourceMap("pipe /1", { view: "detail", topN: 40 });
  await api.getYoutubeMonitoringStatus("https://youtu.be/a?si=b&t=1");
  await api.logout();

  assert.deepEqual(
    requests.map((request) => [request.method, request.url]),
    [
      ["GET", "/api/v1/settings"],
      ["GET", "/api/v1/settings?view=dashboard"],
      ["GET", "/api/v1/settings?jobs=latest"],
      ["GET", "/metrics/system?view=summary"],
      ["GET", "/api/v1/pipelines/pipe%20%2F1/inputs"],
      ["POST", "/api/v1/pipelines/pipe%20%2F1/inputs"],
      ["PATCH", "/api/v1/pipelines/pipe%20%2F1/inputs/input%20%2F1"],
      ["PATCH", "/api/v1/pipelines/pipe%20%2F1/inputs/input%20%2F1"],
      [
        "POST",
        "/api/v1/pipelines/pipe%20%2F1/inputs/input%20%2F1/promote",
      ],
      ["DELETE", "/api/v1/pipelines/pipe%20%2F1/inputs/input%20%2F1"],
      ["PATCH", "/api/v1/pipelines/pipe-1"],
      ["PATCH", "/api/v1/pipelines/pipe-1/outputs/out-1"],
      [
        "GET",
        "/api/v1/logs?limit=25&pipeline_id=pipe-1&output_id=out-1&event_class=lifecycle",
      ],
      ["GET", "/api/v1/logs?limit=50&level=warn&scope=restream"],
      ["GET", "/api/v1/media"],
      ["GET", "/api/v1/alerts"],
      ["GET", "/api/v1/events?pipeline_id=pipe+%2F1&limit=40"],
      ["GET", "/api/v1/engine/telemetry"],
      ["GET", "/api/v1/stages/pipe%20%2F1%3Avideo%3A720p/telemetry"],
      [
        "GET",
        "/api/v1/engine/resource-map?pipeline_id=pipe+%2F1&view=detail&top_n=40",
      ],
      [
        "GET",
        "/api/v1/monitoring/youtube-status?url=https%3A%2F%2Fyoutu.be%2Fa%3Fsi%3Db%26t%3D1",
      ],
      ["POST", "/api/v1/auth/logout"],
    ],
  );
  assert.deepEqual(requests[5].body, {
    label: "Encoder B",
    kind: "keyed",
    sourceUrl: null,
  });
  assert.deepEqual(requests[6].body, {
    label: "Encoder C",
    enabled: false,
  });
  assert.deepEqual(requests[7].body, {
    label: "Origin",
    kind: "hlsPull",
    sourceUrl: "https://origin.example/live/master.m3u8",
    variantRule: "lowestBandwidth",
    auth: { kind: "headers", headers: [{ name: "X-Token", value: "s3cret" }] },
  });
  const pipelineUpdate = requests.find(
    (request) =>
      request.method === "PATCH" &&
      request.url === "/api/v1/pipelines/pipe-1",
  );
  assert.deepEqual(pipelineUpdate?.body, {
    name: "Updated",
    fileIngest: {
      filename: "clip.ts",
      loopFlag: true,
      startTime: "00:00:01",
      liveOptimized: true,
      targetGopSeconds: 2,
    },
  });
});

test("media upload uses the canonical multipart endpoint", async () => {
  let captured;
  globalThis.fetch = async (url, options = {}) => {
    captured = { url: String(url), options };
    return new Response(
      JSON.stringify({ uploaded: true, name: "source.mp4", size: 12 }),
      { status: 200, headers: { "content-type": "application/json" } },
    );
  };

  const api = await loadApiModule();
  const result = await api.uploadMediaFile(
    new File(["media bytes"], "source.mp4", { type: "video/mp4" }),
  );

  assert.equal(captured.url, "/api/v1/media/upload");
  assert.equal(captured.options.method, "POST");
  assert.ok(captured.options.body instanceof FormData);
  assert.equal(captured.options.body.get("file").name, "source.mp4");
  assert.deepEqual(result, { uploaded: true, name: "source.mp4", size: 12 });
});

test("stage telemetry treats an inactive-stage 404 as an expected null snapshot", async () => {
  const api = await loadApiModule();
  let errorAlerts = 0;
  const originalGetElementById = document.getElementById;
  document.getElementById = (id) =>
    id === "error-alert"
      ? {
          classList: {
            remove(className) {
              assert.equal(className, "hidden");
              errorAlerts += 1;
            },
          },
          querySelector() {
            return null;
          },
        }
      : originalGetElementById.call(document, id);
  globalThis.fetch = async () =>
    new Response("Stage not found", { status: 404 });
  const result = await api.getStageTelemetry("gone:stage");
  assert.equal(result, null);
  assert.equal(errorAlerts, 0);
});

test("frontend API helpers preserve response fields and run diagnostics centrally", async () => {
  const requests = [];
  globalThis.fetch = async (url, options = {}) => {
    requests.push({ url: String(url), options });
    if (String(url).startsWith("/api/v1/audio-caps")) {
      return new Response(
        JSON.stringify({
          caps: {
            "youtube:rtmp": { maxTracks: 2, maxChannels: 2, codecs: ["aac"] },
          },
          platformLabels: { youtube: "YouTube" },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (String(url).endsWith("/diagnostics/run")) {
      return new Response(
        JSON.stringify({ protocol: "srt", totalDurationMs: 4, checks: [] }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    return new Response(
      JSON.stringify({
        logs: [
          {
            id: 1,
            ts: "2026-06-29T00:00:00Z",
            message: "started",
            fields: '{"state":"running"}',
            eventType: "lifecycle.started",
          },
        ],
      }),
      { status: 200, headers: { "content-type": "application/json" } },
    );
  };

  const api = await loadApiModule();
  const caps = await api.getAudioCapsPayload();
  const logs = await api.getLogs({ pipelineId: "pipe-1", outputId: "out-1" }, 1);
  const controller = new AbortController();
  const report = await api.runPipelineDiagnostics("pipe 1", controller.signal);

  assert.equal(caps.caps["youtube:rtmp"].maxTracks, 2);
  assert.equal(logs.logs[0].fields, '{"state":"running"}');
  assert.equal(report.protocol, "srt");
  const diagnosticsRequest = requests.find(({ url }) =>
    url.endsWith("/diagnostics/run"),
  );
  assert.equal(
    diagnosticsRequest.url,
    "/api/v1/pipelines/pipe%201/diagnostics/run",
  );
  assert.equal(diagnosticsRequest.options.method, "POST");
  assert.equal(diagnosticsRequest.options.body, undefined);
  assert.equal(diagnosticsRequest.options.signal, controller.signal);
  assert.equal(
    api.buildLogsStreamUrl({
      scope: "restream",
      level: "warn",
      eventClass: "lifecycle",
      lastEventId: 12,
      prefixes: ["stderr", "exit"],
    }),
    "/api/v1/logs/stream?level=warn&scope=restream&event_class=lifecycle&last_event_id=12&prefix=stderr%2Cexit",
  );
  assert.equal(
    api.buildLogsStreamUrl({
      pipelineId: "pipe-1",
      eventClass: "lifecycle",
      includeRestream: true,
    }),
    "/api/v1/logs/stream?pipeline_id=pipe-1&include_restream=true&event_class=lifecycle",
  );
});
