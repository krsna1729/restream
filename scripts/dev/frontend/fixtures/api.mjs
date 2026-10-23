// The synthetic engine's HTTP surface, independent of any transport: the
// Node fixture server and the static browser demo both answer with it.
//
//   const api = createFixtureApi({ preset: "plant", seed: 7 });
//   api.handle("GET", "/api/v1/state", new URLSearchParams(), null)
//     → { status, body } | null (no data for that route)
//   api.snapshot()   the state stream's first frame
//   api.tick(nowMs)  the next state delta, or null

import { createDeployment } from "./density.mjs";
import { createNotify } from "./notify.mjs";
import { createSchedules } from "./schedule.mjs";
import { createTeam } from "./team.mjs";

export function createFixtureApi({ preset, seed }) {
  const plant = createDeployment({ preset, seed });
  const schedules = createSchedules({ seed, pipelines: plant.state().pipelines, outputs: plant.state().outputs, count: preset === "small" ? 6 : 40 });
  const notify = createNotify({ seed, alerts: plant.alerts({}).alerts ?? [] });
  const team = createTeam({ seed, outputs: plant.state().outputs, members: preset === "small" ? 4 : preset === "plant" ? 24 : 120 });

  const reads = [
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
    [
      /^\/api\/v1\/pipelines\/([^/]+)\/flight-recorder$/,
      (m, q) =>
        plant.flightRecorder(decodeURIComponent(m[1]), {
          since: q.get("since") ? Date.parse(q.get("since")) : undefined,
          until: q.get("until") ? Date.parse(q.get("until")) : undefined,
          points: q.get("points") ? Number(q.get("points")) : undefined,
        }),
    ],
    [/^\/api\/v1\/pipelines\/([^/]+)\/probe$/, () => ({ audioTracks: [{ trackIndex: 0, codec: "aac", channels: 2, sampleRate: 48000 }] })],
  ];

  function handle(method, pathname, searchParams, body) {
    const answer = team.handle(method, pathname, searchParams, body) ?? notify.handle(method, pathname, body) ?? schedules.handle(method, pathname, body);
    if (answer) {
      if (method !== "GET") team.record(method, pathname, answer.status ?? 200);
      return { status: answer.status ?? 200, body: answer.body };
    }
    const start = pathname.match(/^\/api\/v1\/pipelines\/[^/]+\/outputs\/([^/]+)\/(start|stop)$/);
    if (start && method === "POST") {
      team.record("POST", pathname, 200);
      const ok = plant.setDesired(decodeURIComponent(start[1]), start[2] === "start" ? "running" : "stopped");
      return ok
        ? { status: 200, body: { message: "ok", desiredState: start[2] === "start" ? "running" : "stopped" } }
        : { status: 404, body: { error: "not found" } };
    }
    if (method === "GET") {
      for (const [pattern, read] of reads) {
        const match = pathname.match(pattern);
        if (match) return { status: 200, body: read(match, searchParams) };
      }
    }
    if (pathname.startsWith("/api/") && method !== "GET") {
      team.record(method, pathname, 200);
      return { status: 200, body: { message: "fixture: accepted, not applied" } };
    }
    return null;
  }

  return {
    handle,
    snapshot: () => plant.state(),
    tick: (nowMs) => plant.tick(nowMs),
    counts: () => plant.counts(),
  };
}
