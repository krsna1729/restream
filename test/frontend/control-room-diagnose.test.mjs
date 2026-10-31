import assert from "node:assert/strict";
import test from "node:test";

import {
  LOG_ROWS_KEPT,
  diagnosticsFile,
  diagnosticsVerdict,
  logFieldsText,
  logFilters,
  withLiveRow,
} from "../../web/ts/control-room/diagnose-model.ts";

test("each scope asks the engine for its own rows, with one filter set for history and stream", () => {
  assert.deepEqual(logFilters({ kind: "all" }, "info", false), { level: "info" });
  assert.deepEqual(logFilters({ kind: "engine" }, "warn", false), { level: "warn", scope: "restream" });
  assert.deepEqual(logFilters({ kind: "pipeline", pipelineId: "p1", outputId: null }, "info", true), {
    level: "info",
    pipelineId: "p1",
    eventClass: "lifecycle",
  });
  assert.deepEqual(logFilters({ kind: "pipeline", pipelineId: "p1", outputId: "o1" }, "debug", false), {
    level: "debug",
    pipelineId: "p1",
    outputId: "o1",
  });
});

test("a streamed row goes on top once, and the page keeps a bounded number of rows", () => {
  const history = [{ id: 3 }, { id: 2 }, { id: 1 }];
  assert.deepEqual(
    withLiveRow(history, { id: 4 }).map((row) => row.id),
    [4, 3, 2, 1],
  );
  // The stream resumes at the newest history id; a repeat is not shown twice.
  assert.deepEqual(
    withLiveRow(history, { id: 3 }).map((row) => row.id),
    [3, 2, 1],
  );
  const full = Array.from({ length: LOG_ROWS_KEPT }, (_, index) => ({ id: LOG_ROWS_KEPT - index }));
  const next = withLiveRow(full, { id: LOG_ROWS_KEPT + 1 });
  assert.equal(next.length, LOG_ROWS_KEPT);
  assert.equal(next[0].id, LOG_ROWS_KEPT + 1);
  assert.equal(next.at(-1).id, 2);
});

test("structured fields read as indented JSON, raw text when they are not JSON", () => {
  assert.equal(logFieldsText({ fields: '{"a":1}' }), '{\n  "a": 1\n}');
  assert.equal(logFieldsText({ fields: { b: 2 } }), '{\n  "b": 2\n}');
  assert.equal(logFieldsText({ fields: "not json" }), "not json");
  assert.equal(logFieldsText({ fields: null }), null);
  assert.equal(logFieldsText({ fields: "" }), null);
  assert.equal(logFieldsText({}), null);
});

const report = {
  protocol: "srt",
  totalDurationMs: 4200,
  checks: [
    { index: 0, name: "probe", exitCode: 0, issues: [] },
    { index: 1, name: "gop", exitCode: 0, issues: ["keyframes 6 s apart"] },
    { index: 2, name: "ring", exitCode: 1, issues: ["read failed"] },
  ],
};

test("a run counts failed checks apart from checks that ran and found issues", () => {
  assert.deepEqual(diagnosticsVerdict(report), { failed: 1, withIssues: 1 });
  assert.deepEqual(diagnosticsVerdict({ ...report, checks: [report.checks[0]] }), { failed: 0, withIssues: 0 });
});

test("the downloaded report keeps the classic schema and a safe file name", () => {
  const file = diagnosticsFile({ id: "pipe/1", name: "Main feed" }, report, new Date("2026-10-13T07:08:09.123Z"));
  assert.equal(file.name, "diagnostic-Main_feed-pipe_1-2026-10-13_07-08-09.json");
  const json = JSON.parse(file.json);
  assert.equal(json.schema, "restream.diagnostics.v1");
  assert.equal(json.generatedAt, "2026-10-13T07:08:09.123Z");
  assert.deepEqual(json.pipeline, { id: "pipe/1", name: "Main feed" });
  assert.equal(json.probe, "srt");
  assert.equal(json.totalCommands, 3);
  assert.deepEqual(json.results, report.checks);
});
