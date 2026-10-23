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

import { createFixtureApi } from "./fixtures/api.mjs";
import { PRESETS } from "./fixtures/density.mjs";

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

const api = createFixtureApi({ preset, seed });
const subscribers = new Set();

setInterval(() => {
  const delta = api.tick(Date.now());
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

const server = createServer(async (req, res) => {
  const url = new URL(req.url ?? "/", "http://fixture");
  const { pathname, searchParams } = url;

  if (pathname === "/api/v1/state/stream") {
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-store", connection: "keep-alive" });
    res.write(`event: snapshot\ndata: ${JSON.stringify(api.snapshot())}\n\n`);
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

  // A media poster: a colour-bar still per file, its hue from the name, so
  // the library reads as pictures without real media.
  const poster = pathname.match(/^\/api\/v1\/media\/([^/]+)\/poster$/);
  if (poster) {
    res.writeHead(200, { "content-type": "image/svg+xml", "cache-control": "no-store" });
    return res.end(posterSvg(decodeURIComponent(poster[1])));
  }
  if (pathname.startsWith("/api/") || pathname === "/metrics/system" || pathname === "/healthz") {
    const method = req.method ?? "GET";
    const body = method === "GET" ? null : await readBody(req);
    const answer = api.handle(method, pathname, searchParams, body);
    if (answer) return sendJson(res, answer.body, answer.status);
    console.log(`fixture: no data for ${method} ${pathname}`);
    return sendJson(res, { error: "fixture has no data for this route" }, 404);
  }
  if (pathname.startsWith("/hls/")) return sendJson(res, { error: "no media in fixtures" }, 404);
  return serveStatic(res, pathname === "/" ? "/index.html" : pathname);
});

server.listen(port, "127.0.0.1", () => {
  const counts = api.counts();
  console.log(
    `fixture ${preset}: ${counts.pipelines} pipelines, ${counts.inputs} inputs, ${counts.outputs} outputs on http://127.0.0.1:${port}`,
  );
});

function posterSvg(name) {
  let hash = 0;
  for (const char of name) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  const hue = hash % 360;
  const bars = ["#c0c0c0", "#c0c000", "#00c0c0", "#00c000", "#c000c0", "#c00000", "#0000c0"]
    .map((color, index) => `<rect x="${index * 320 / 7}" width="${320 / 7 + 1}" height="120" fill="${color}"/>`)
    .join("");
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 180" width="320" height="180">${bars}<rect y="120" width="320" height="60" fill="hsl(${hue} 45% 22%)"/><text x="160" y="157" font-family="sans-serif" font-size="18" fill="#fff" text-anchor="middle">${name.replace(/[<&]/g, "").slice(0, 28)}</text></svg>`;
}
