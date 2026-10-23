import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { explainCause, explainError, outputTrouble, troubleTitle } = await loadCompiledFrontendModule("control-room/trouble-model.js");

test("known failure texts read as what went wrong and what to do", () => {
  assert.equal(explainError("[connect] Connection refused (os error 111)").summary, "The server refused the connection");
  assert.equal(explainError("NetStream.Publish.BadName: stream key in use").summary, "Another encoder is already streaming with this key");
  assert.equal(explainError("publish rejected: 403 Forbidden").summary, "The platform rejected the stream key");
  assert.equal(explainError("failed to lookup address information: Name or service not known").summary, "The server's name could not be found");
  assert.equal(explainError("operation timed out").summary, "The server did not answer in time");
  assert.match(explainError("Broken pipe (os error 32)").fix, /reconnects on its own/);
  assert.equal(explainError("something new"), null, "an unknown text is not guessed at");
});

test("a rejected cause keeps the engine's words for whoever needs them", () => {
  const known = explainCause({ code: "rejected", phase: "connect", error: "Connection refused (os error 111)" });
  assert.equal(known.summary, "The server refused the connection");
  assert.equal(known.raw, "connect: Connection refused (os error 111)");
  const unknown = explainCause({ code: "rejected", phase: null, error: "weird" });
  assert.deepEqual(unknown, { summary: "The server rejected the stream", fix: null, raw: "weird" });
  assert.equal(explainCause({ code: "stageNotProducing", stage: "p1:transcode:h264_720p", phase: "starting" }).summary, "Waiting on the h264_720p stage");
});

test("a destination's trouble: its diagnosis first, then its last failure, then back-pressure", () => {
  assert.equal(outputTrouble({ diagnosis: null, evidence: null, backpressure: null }), null);
  const failed = outputTrouble({ diagnosis: null, evidence: { error: "operation timed out", phase: "connect", atMs: 0 }, backpressure: null });
  assert.equal(failed.summary, "The server did not answer in time");
  assert.equal(failed.raw, "connect: operation timed out");
  assert.equal(
    outputTrouble({ diagnosis: { layer: "destination", cause: { code: "noPublisher" }, ruledOut: [] }, evidence: { error: "x", phase: null, atMs: 0 }, backpressure: null }).summary,
    "No publisher connected",
  );
  assert.equal(troubleTitle({ summary: "A", fix: "B", raw: "C" }), "A\nB\nEngine: C");
});
