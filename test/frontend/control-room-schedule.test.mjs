import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { occurrences, agenda, nextUp, resultTone, toLocalInput, fromLocalInput, untilLabel } = await loadCompiledFrontendModule(
  "control-room/schedule-model.js",
);

const DAY = 86_400_000;
const T = Date.UTC(2026, 9, 20, 9, 0);

test("occurrences follow the engine's rule: the first time, then every period", () => {
  assert.deepEqual(occurrences(T, "once", T - 1, T + 1), [T]);
  assert.deepEqual(occurrences(T, "once", T + 1, T + DAY), []);
  assert.deepEqual(occurrences(T, "daily", T + DAY / 2, T + 3 * DAY), [T + DAY, T + 2 * DAY]);
  assert.deepEqual(occurrences(T, "weekly", T, T + 15 * DAY), [T, T + 7 * DAY, T + 14 * DAY]);
});

test("the agenda lays enabled occurrences out by local day, in time order", () => {
  const event = (id, atMs, repeat, enabled = true) => ({ id, name: id, atMs, repeat, enabled, nextAtMs: atMs });
  const days = agenda([event("b", T + 3_600_000, "daily"), event("a", T, "daily"), event("off", T, "daily", false)], T, 3);
  assert.equal(days.length, 3);
  assert.deepEqual(days[0].items.map((item) => item.schedule.id), ["a", "b"]);
  assert.ok(days.every((day) => !day.items.some((item) => item.schedule.id === "off")));
  assert.equal(nextUp([event("late", T + 9, "once"), event("soon", T + 5, "once"), { ...event("p", T + 1, "once"), enabled: false }]).id, "soon");
});

test("results read as tones, local inputs round-trip, and waits read as words", () => {
  assert.equal(resultTone("ok: 2 of 2"), "ok");
  assert.equal(resultTone("failed: no file"), "bad");
  assert.equal(resultTone("skipped: the engine was not running"), "warn");
  assert.equal(resultTone(null), "neutral");
  const local = new Date(2026, 9, 20, 7, 5).getTime();
  assert.equal(fromLocalInput(toLocalInput(local)), local);
  assert.ok(Number.isNaN(fromLocalInput("")));
  assert.equal(untilLabel(T + 125 * 60_000, T), "in 2 h 5 min");
  assert.equal(untilLabel(T + 30_000, T), "in 30 s");
  assert.equal(untilLabel(T - 1, T), "now");
});

test("the next event for a pipeline is its soonest enabled one", () => {
  const event = (id, pipelineId, nextAtMs, enabled = true) => ({ id, pipelineId, nextAtMs, enabled });
  const all = [event("a", "p1", 5_000), event("b", "p2", 3_000), event("c", "p1", 2_000, false), event("e", "p1", null)];
  assert.equal(nextUp(all).id, "b");
  assert.equal(nextUp(all, "p1").id, "a", "paused and finished events are not next");
  assert.equal(nextUp(all, "p3"), null);
});

test("recordings that started on a day are its events, oldest first; one still writing has no end", async () => {
  const { recordingsOn, localDay } = await loadCompiledFrontendModule("control-room/schedule-model.js");
  const day = localDay(Date.UTC(2026, 9, 23, 12));
  const at = (hours) => new Date(day + hours * 3_600_000).toISOString();
  const files = [
    { name: "late.mp4", kind: "recording", pipelineId: "p1", recordingStatus: "recording", recordingStartedAt: at(18) },
    { name: "early.mp4", kind: "recording", pipelineId: "p2", recordingStatus: "ready", recordingStartedAt: at(9), recordingEndedAt: at(10) },
    { name: "yesterday.mp4", kind: "recording", recordingStatus: "ready", recordingStartedAt: at(-3), recordingEndedAt: at(-2) },
    { name: "upload.mp4", kind: "source" },
  ];
  const events = recordingsOn(files, day);
  assert.deepEqual(events.map((event) => event.name), ["early.mp4", "late.mp4"]);
  assert.equal(events[0].endMs - events[0].startMs, 3_600_000);
  assert.equal(events[1].endMs, null);
});
