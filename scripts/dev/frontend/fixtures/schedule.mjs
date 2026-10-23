// Synthetic scheduled events for the fixture server: daily go-lives and
// stops across the plant's pipelines, a few one-offs, a paused one, and
// last results of every kind.

import { rng } from "./density.mjs";

const NAMES = ["Morning show", "Evening news", "Match day", "Night loop", "Sunday service", "Webinar", "Keynote", "Replay block"];

export function createSchedules({ seed = 7, pipelines = [], outputs = [], count = 30, nowMs = Date.now() } = {}) {
  const random = rng(seed * 13 + 11);
  const midnight = new Date(nowMs);
  midnight.setHours(0, 0, 0, 0);
  const schedules = [];
  for (let index = 0; index < Math.min(count, pipelines.length * 2); index += 1) {
    const pipeline = pipelines[index % pipelines.length];
    const repeat = index % 7 === 0 ? "once" : index % 5 === 0 ? "weekly" : "daily";
    const action = random.pick(["startOutputs", "startOutputs", "stopOutputs", "startFile", "stopFile"]);
    const atMs = midnight.getTime() + random.int(0, repeat === "once" ? 6 : 1) * 86_400_000 + random.int(6, 23) * 3_600_000 + random.pick([0, 15, 30, 45]) * 60_000;
    const mine = outputs.filter((output) => output.pipelineId === pipeline.id);
    const lastResult = random.pick([
      null,
      `ok: ${mine.length} of ${mine.length} destinations wanted running`,
      "ok: started intro.mp4, show.mp4",
      "failed: the pipeline has no file source",
      "skipped: the engine was not running at 2026-10-20 09:00 UTC",
    ]);
    schedules.push({
      id: `sch_${index.toString(16).padStart(4, "0")}`,
      pipelineId: pipeline.id,
      name: `${random.pick(NAMES)} · ${pipeline.name}`,
      action,
      outputIds: action.endsWith("Outputs") && random.chance(0.3) ? mine.slice(0, 2).map((output) => output.id) : [],
      atMs,
      repeat,
      enabled: index % 11 !== 3,
      lastRunMs: lastResult ? atMs - 86_400_000 : null,
      lastResult,
      createdAtMs: nowMs - 20 * 86_400_000,
    });
  }
  const period = { once: null, daily: 86_400_000, weekly: 7 * 86_400_000 };
  const withNext = (schedule) => {
    const now = Date.now();
    const step = period[schedule.repeat];
    let next = schedule.atMs > now ? schedule.atMs : step === null ? null : schedule.atMs + (Math.floor((now - schedule.atMs) / step) + 1) * step;
    if (!schedule.enabled) next = null;
    return { ...schedule, nextAtMs: next };
  };

  const links = new Map();
  function handle(method, path, body) {
    const share = path.match(/^\/api\/v1\/pipelines\/([^/]+)\/share-links$/);
    if (share) {
      const list = links.get(share[1]) ?? [
        { id: `sl_${share[1]}_1`, pipelineId: share[1], label: "Website embed", token: "f".repeat(48), createdAtMs: nowMs - 86_400_000, expiresAtMs: null, views: 1284 },
        { id: `sl_${share[1]}_2`, pipelineId: share[1], label: "Client preview", token: "e".repeat(48), createdAtMs: nowMs - 3_600_000, expiresAtMs: nowMs + 5 * 3_600_000, views: 17 },
      ];
      links.set(share[1], list);
      if (method === "GET") return { body: { links: list } };
      const link = { id: `sl_${Date.now().toString(16)}`, pipelineId: share[1], label: body.label, token: Date.now().toString(16).padEnd(48, "0"), createdAtMs: Date.now(), expiresAtMs: body.lifetimeHours ? Date.now() + body.lifetimeHours * 3_600_000 : null, views: 0 };
      list.push(link);
      return { status: 201, body: link };
    }
    const revoke = path.match(/^\/api\/v1\/share-links\/([^/]+)$/);
    if (revoke && method === "DELETE") {
      for (const list of links.values()) {
        const at = list.findIndex((link) => link.id === revoke[1]);
        if (at >= 0) list.splice(at, 1);
      }
      return { body: { deleted: true } };
    }
    if (path === "/api/v1/schedules" && method === "GET") return { body: { schedules: schedules.map(withNext) } };
    if (path === "/api/v1/schedules" && method === "POST") {
      const schedule = { ...body, id: `sch_${Date.now().toString(16)}`, lastRunMs: null, lastResult: null, createdAtMs: Date.now() };
      schedules.push(schedule);
      return { status: 201, body: withNext(schedule) };
    }
    const one = path.match(/^\/api\/v1\/schedules\/([^/]+)(\/run)?$/);
    if (one) {
      const index = schedules.findIndex((schedule) => schedule.id === one[1]);
      if (index < 0) return { status: 404, body: { error: "no such schedule" } };
      if (one[2]) return { body: { ok: true, result: "fixture: accepted, not applied" } };
      if (method === "PUT") {
        schedules[index] = { ...schedules[index], ...body };
        return { body: withNext(schedules[index]) };
      }
      if (method === "DELETE") {
        schedules.splice(index, 1);
        return { body: { deleted: true } };
      }
    }
    return undefined;
  }
  return { handle };
}
