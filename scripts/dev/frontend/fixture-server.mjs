#!/usr/bin/env node
// Serves the built dashboard (`public/`) against a synthetic deployment
// (`fixtures/density.mjs`) so the UI can be exercised at any scale without
// an engine: hundreds of inputs and thousands of destinations, live deltas
// every 2 s, failures, retries and a scripted shard storm.
//
//   node scripts/dev/frontend/fixture-server.mjs --preset plant --port 3031
//
// Presets: small, plant, extreme. It is for development only: it has no
// auth (the session is the built-in admin, who can view as a lower role), accepts every mutation it understands, and labels itself synthetic.

import { createReadStream, existsSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { createDeployment, PRESETS } from "./fixtures/density.mjs";
import { createTeam } from "./fixtures/team.mjs";

const args = new Map();
for (let index = 2; index < process.argv.length; index += 1) {
  const arg = process.argv[index];
  if (arg.startsWith("--")) args.set(arg.slice(2), process.argv[index + 1]);
}
const preset = args.get("preset") ?? "plant";
if (!PRESETS[preset]) {
  console.error(`unknown preset ${preset}; one of ${Object.keys(PRESETS).join(", ")}`);
  process.exit(2);
}
const port = Number(args.get("port") ?? 3031);
const seed = Number(args.get("seed") ?? 7);
const tickMs = Number(args.get("tick-ms") ?? 2000);
const root = resolve(fileURLToPath(new URL("../../../public", import.meta.url)));

const plant = createDeployment({ preset, seed });
const subscribers = new Set();
const team = createTeam({ seed, outputs: plant.state().outputs, members: preset === "small" ? 4 : preset === "plant" ? 24 : 120 });

setInterval(() => {
  const delta = plant.tick(Date.now());
  if (!delta) return;
  const frame = `event: delta\ndata: ${JSON.stringify(delta)}\n\n`;
  for (const res of subscribers) res.write(frame);
}, tickMs);
setInterval(() => {
  for (const res of subscribers) res.write("event: heartbeat\ndata: {}\n\n");
}, 20_000);

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".json": "application/json",
  ".webmanifest": "application/manifest+json",
};

function sendJson(res, body, status = 200) {
  const text = JSON.stringify(body);
  res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" });
  res.end(text);
}

function serveStatic(res, pathname) {
  const target = normalize(join(root, pathname));
  if (!target.startsWith(root)) return sendJson(res, { error: "forbidden" }, 403);
  const file = existsSync(target) && statSync(target).isFile() ? target : join(root, "index.html");
  res.writeHead(200, { "content-type": TYPES[extname(file)] ?? "application/octet-stream", "cache-control": "no-store" });
  createReadStream(file).pipe(res);
}

async function readBody(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  const text = Buffer.concat(chunks).toString("utf8");
  try {
    return text ? JSON.parse(text) : {};
  } catch {
    return {};
  }
}

const routes = [
  [/^\/healthz$/, () => ({ ok: true })],
  [/^\/api\/v1\/state$/, () => plant.state()],
  [/^\/metrics\/system$/, () => plant.systemMetrics()],
  [/^\/api\/v1\/engine\/telemetry$/, () => plant.telemetry()],
  [/^\/api\/v1\/engine\/health$/, () => plant.health()],
  [/^\/api\/v1\/engine$/, () => plant.engine()],
  [/^\/api\/v1\/settings$/, () => plant.settings()],
  [/^\/api\/v1\/alerts$/, (_m, q) => plant.alerts({ since: q.get("since") ?? undefined })],
  [/^\/api\/v1\/events$/, (_m, q) => plant.lifecycleEvents({ pipelineId: q.get("pipeline_id") ?? q.get("pipelineId") ?? undefined })],
  [/^\/api\/v1\/fleet\/history$/, (_m, q) => plant.fleetHistory({ since: q.get("since") ?? undefined, until: q.get("until") ?? undefined })],
  [/^\/api\/v1\/engine\/resource-map$/, (_m, q) => plant.resourceMap({ pipelineId: q.get("pipeline_id") ?? undefined, topN: Number(q.get("top_n") ?? 200) })],
  [/^\/api\/v1\/capacity\/scenario$/, (_m, q) => plant.capacityScenario(q)],
  [/^\/api\/v1\/media$/, () => plant.mediaFiles()],
  [/^\/api\/v1\/logs$/, () => plant.logs()],
  [/^\/api\/v1\/security\/rate-limits$/, () => ({ attempts: [] })],
  [/^\/api\/v1\/audio-caps$/, () => ({ caps: {}, platformLabels: {} })],
  [/^\/api\/v1\/pipelines\/([^/]+)\/inputs$/, (m) => plant.pipelineInputs(decodeURIComponent(m[1]))],
  [/^\/api\/v1\/pipelines\/([^/]+)\/file-ingest$/, () => ({ configured: false, running: false })],
  [/^\/api\/v1\/pipelines\/([^/]+)\/summary$/, (m) => plant.pipelineSummary(decodeURIComponent(m[1]))],
  [/^\/api\/v1\/pipelines\/([^/]+)\/flight-recorder$/, (m) => plant.flightRecorder(decodeURIComponent(m[1]))],
  [/^\/api\/v1\/pipelines\/([^/]+)\/probe$/, () => ({ audioTracks: [{ trackIndex: 0, codec: "aac", channels: 2, sampleRate: 48000 }] })],
];

const server = createServer(async (req, res) => {
  const url = new URL(req.url ?? "/", "http://fixture");
  const { pathname, searchParams } = url;

  if (pathname === "/api/v1/state/stream") {
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-store", connection: "keep-alive" });
    res.write(`event: snapshot\ndata: ${JSON.stringify(plant.state())}\n\n`);
    subscribers.add(res);
    req.on("close", () => subscribers.delete(res));
    return;
  }
  if (pathname === "/api/v1/logs/stream") {
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-store" });
    const timer = setInterval(() => res.write(": keepalive\n\n"), 15_000);
    req.on("close", () => clearInterval(timer));
    return;
  }

  if (pathname.startsWith("/api/")) {
    const method = req.method ?? "GET";
    const body = method === "GET" ? null : await readBody(req);
    const answer = team.handle(method, pathname, searchParams, body);
    if (answer) {
      if (method !== "GET") team.record(method, pathname, answer.status ?? 200);
      return sendJson(res, answer.body, answer.status ?? 200);
    }
    req.fixtureBody = body;
  }

  const start = pathname.match(/^\/api\/v1\/pipelines\/[^/]+\/outputs\/([^/]+)\/(start|stop)$/);
  if (start && req.method === "POST") {
    team.record("POST", pathname, 200);
    const ok = plant.setDesired(decodeURIComponent(start[1]), start[2] === "start" ? "running" : "stopped");
    return sendJson(res, ok ? { message: "ok", desiredState: start[2] === "start" ? "running" : "stopped" } : { error: "not found" }, ok ? 200 : 404);
  }

  if (req.method === "GET") {
    for (const [pattern, handler] of routes) {
      const match = pathname.match(pattern);
      if (match) return sendJson(res, handler(match, searchParams));
    }
  }
  if (pathname.startsWith("/api/")) {
    if (req.method !== "GET") {
      team.record(req.method, pathname, 200);
      return sendJson(res, { message: "fixture: accepted, not applied" });
    }
    console.log(`fixture: no data for ${req.method} ${pathname}`);
    return sendJson(res, { error: "fixture has no data for this route" }, 404);
  }
  if (pathname.startsWith("/hls/")) return sendJson(res, { error: "no media in fixtures" }, 404);
  return serveStatic(res, pathname === "/" ? "/index.html" : pathname);
});

server.listen(port, "127.0.0.1", () => {
  const counts = plant.counts();
  console.log(
    `fixture ${preset}: ${counts.pipelines} pipelines, ${counts.inputs} inputs, ${counts.outputs} outputs on http://127.0.0.1:${port}`,
  );
});
