import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { formatDuration, ingestReadiness, searchMedia, splitLibrary, storedForms } =
  await loadCompiledFrontendModule("control-room/media-model.js");
const {
  executionTopology,
  hostReadiness,
  kernelCapabilities,
  overallReadiness,
  ownerAttention,
} = await loadCompiledFrontendModule("control-room/system-model.js");

function file(name, overrides = {}) {
  return { name, size: 1, modifiedAt: "2026-10-12T10:00:00Z", ...overrides };
}

test("the library splits recordings from sources, the engine's kind first, newest first", () => {
  const { recordings, sources } = splitLibrary([
    file("old-recording.ts", { modifiedAt: "2026-10-12T09:00:00Z" }),
    file("new-recording.ts", { modifiedAt: "2026-10-12T11:00:00Z" }),
    // A file a pipeline ingests is a source, whatever its name says.
    file("recording-loop.mp4", { ingestCount: 1 }),
    // The engine's kind wins over the name.
    file("slate.mp4", { kind: "recording" }),
    file("sponsor.mp4"),
  ]);
  assert.deepEqual(
    recordings.map((f) => f.name),
    ["new-recording.ts", "slate.mp4", "old-recording.ts"],
  );
  assert.deepEqual(
    sources.map((f) => f.name),
    ["recording-loop.mp4", "sponsor.mp4"],
  );
});

test("search matches any stored name, ignoring case; a blank query keeps every file", () => {
  const files = [
    file("a.ts", { convertedName: "Temple-Main.mp4" }),
    file("b.mp4", { sourceName: "news.ts" }),
    file("c.mp4"),
  ];
  assert.deepEqual(
    searchMedia(files, "temple").map((f) => f.name),
    ["a.ts"],
  );
  assert.deepEqual(
    searchMedia(files, " NEWS ").map((f) => f.name),
    ["b.mp4"],
  );
  assert.equal(searchMedia(files, "  "), files);
});

test("a converted form equal to the original is not offered twice", () => {
  assert.deepEqual(storedForms(file("a.ts", { convertedName: "a.mp4" })), {
    original: "a.ts",
    converted: "a.mp4",
  });
  assert.deepEqual(storedForms(file("a.mp4", { convertedName: "a.mp4" })), {
    original: "a.mp4",
    converted: null,
  });
  assert.deepEqual(storedForms(file("x", { sourceName: "a.ts" })).original, "a.ts");
});

test("ingest readiness follows the engine's sparse-GOP judgement and needs video", () => {
  const base = {
    videoCodec: "h264",
    keyframeCount: 10,
    maxKeyframeIntervalSec: 2,
    sparseForLive: false,
    liveGopTargetSeconds: 2,
  };
  assert.equal(ingestReadiness(base).kind, "ready");
  const sparse = ingestReadiness({ ...base, maxKeyframeIntervalSec: 5, sparseForLive: true });
  assert.equal(sparse.kind, "needsReencode");
  assert.match(sparse.detail, /5\.0 s/);
  assert.equal(ingestReadiness({ ...base, videoCodec: null }).kind, "noVideo");
});

test("durations read as h:mm:ss, or m:ss under an hour", () => {
  assert.equal(formatDuration(3725), "1:02:05");
  assert.equal(formatDuration(65), "1:05");
  assert.equal(formatDuration(null), "--");
});

function setting(key, overrides = {}) {
  return { key, label: key, current: 1, status: "ok", ...overrides };
}

test("host checks list a failing one first and read like the engine measured them", () => {
  const rows = hostReadiness({
    hostSettings: [
      setting("nofile", { current: 65536, unit: "fds" }),
      setting("rmem", { current: 26214400, unit: "bytes", status: "warning" }),
      setting("cgroup", { current: "max 100000", unit: "quota" }),
      setting("odd", { status: "surprising" }),
    ],
  });
  assert.deepEqual(
    rows.map((row) => [row.key, row.status]),
    [
      ["rmem", "warning"],
      ["odd", "unknown"],
      ["nofile", "ok"],
      ["cgroup", "ok"],
    ],
  );
  assert.equal(rows[0].value, "25.0 MiB");
  assert.equal(rows[3].value, "max 100000");
  assert.equal(overallReadiness(rows), "warning");
  assert.equal(overallReadiness(rows.slice(1)), "unknown");
  assert.equal(overallReadiness(rows.slice(2)), "ok");
});

test("the topology shows each class's reported count, and unknown is not zero", () => {
  const health = {
    hostSettings: [setting("runtime.tokio.worker_threads", { current: 2 })],
    rtmpListener: { ingressOwners: 0 },
    srtListener: { ingressOwner: { owners: 3 } },
    egressFabricShards: [{}, {}],
  };
  const metrics = {
    mediaExecutor: { configuredWorkers: 4 },
    engine: { externalFfmpegCount: 1 },
  };
  assert.deepEqual(
    executionTopology(health, metrics).map((cls) => [cls.name, cls.count]),
    [
      ["CONTROL", 2],
      ["MEDIA", 4],
      ["RTMP ingress", 0],
      ["SRT ingress", 3],
      ["Egress fabric", 2],
      ["FFmpeg", 1],
    ],
  );
  assert.ok(executionTopology({}, null).every((cls) => cls.count === null));
});

test("kernel TLS is available when any cipher suite is, unknown when not probed", () => {
  const [, none] = kernelCapabilities(null);
  assert.equal(none.available, null);
  const [, off] = kernelCapabilities({ rtmps: { ktlsCapabilities: { a: false } } });
  assert.equal(off.available, false);
  const [uring, on] = kernelCapabilities({
    ioUring: { available: true, deploymentTier: "tier3" },
    rtmps: { ktlsCapabilities: { a: false, b: true } },
  });
  assert.equal(on.available, true);
  assert.equal(on.detail, "1 of 2 cipher suites");
  assert.equal(uring.available, true);
});

test("owner findings name each counter of lost or late work, and only those", () => {
  const clean = {
    faulted: false,
    rxRingDropped: 0,
    rxBufferExhaustions: 0,
    eventBridgeFullVisits: 0,
    telemetryDropped: 0,
    overloadDisconnects: 0,
    mediaSlowPasses20ms: 0,
    mediaSlowPasses5ms: 7,
    peers: 3,
  };
  assert.deepEqual(ownerAttention(clean), []);
  for (const [field, value] of [
    ["faulted", true],
    ["rxRingDropped", 2],
    ["rxBufferExhaustions", 1],
    ["eventBridgeFullVisits", 1],
    ["telemetryDropped", 1],
    ["overloadDisconnects", 1],
    ["mediaSlowPasses20ms", 1],
  ]) {
    assert.equal(ownerAttention({ ...clean, [field]: value }).length, 1, field);
  }
});
