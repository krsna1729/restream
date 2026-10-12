import assert from "node:assert/strict";
import test from "node:test";

import {
  installFakeDom,
  loadCompiledFrontendModule,
} from "../support/helpers/fake-dom.mjs";

function output(id, convergence) {
  return {
    id,
    pipelineId: "pipe",
    name: id,
    desired: "running",
    actual: null,
    convergence,
    flow: "idle",
    backpressure: null,
    retry: null,
    evidence: null,
  };
}

function delta(revision, outputs, removedOutputs = [], epoch = "e1") {
  return {
    epoch,
    revision,
    pipelines: [],
    outputs,
    removedPipelines: [],
    removedOutputs,
  };
}

class FakeSource {
  constructor(url) {
    this.url = url;
    this.closed = false;
    this.handlers = new Map();
    this.onerror = null;
  }
  addEventListener(type, handler) {
    this.handlers.set(type, handler);
  }
  emit(type, payload) {
    this.handlers.get(type)?.({ data: JSON.stringify(payload) });
  }
  close() {
    this.closed = true;
  }
}

async function loadStream() {
  installFakeDom();
  return loadCompiledFrontendModule("core/state-stream.js");
}

test("deltas apply in revision order; removed entities go", async () => {
  const { applyStateDelta } = await loadStream();
  const base = {
    epoch: "e1",
    revision: 3,
    pipelines: [],
    outputs: [output("a", "starting"), output("b", "starting")],
  };
  const next = applyStateDelta(
    base,
    delta(4, [output("a", "converged")], ["b"]),
  );
  assert.equal(next.revision, 4);
  assert.deepEqual(
    next.outputs.map((entry) => [entry.id, entry.convergence]),
    [["a", "converged"]],
  );
  assert.equal(applyStateDelta(next, delta(4, [])), next, "a replayed delta is ignored");
  assert.equal(applyStateDelta(next, delta(6, [])), null, "a gap asks for a resync");
  assert.equal(
    applyStateDelta(next, delta(5, [], [], "e2")),
    null,
    "a delta from another server run asks for a resync",
  );
});

test("a client that misses events resyncs from its last revision", async () => {
  const { openRuntimeStateStream } = await loadStream();
  const sources = [];
  const seen = [];
  const stream = openRuntimeStateStream({
    onState: (state) => seen.push(state.revision),
    createEventSource: (url) => {
      const source = new FakeSource(url);
      sources.push(source);
      return source;
    },
  });

  assert.match(sources[0].url, /\/api\/v1\/state\/stream$/);
  sources[0].emit("snapshot", {
    epoch: "e1",
    revision: 7,
    pipelines: [],
    outputs: [output("a", "starting")],
  });
  sources[0].emit("delta", delta(8, [output("a", "converged")]));
  // Revision 9 was lost in transit.
  sources[0].emit("delta", delta(10, [output("a", "retrying")]));

  assert.equal(sources[0].closed, true, "the gap closes the stream");
  assert.equal(sources.length, 2);
  assert.match(sources[1].url, /\/api\/v1\/state\/stream\?since=e1\.8$/);
  assert.equal(stream.current().revision, 8, "nothing past the gap is applied");

  // The server replays what was missed.
  sources[1].emit("delta", delta(9, [output("a", "starting")]));
  sources[1].emit("delta", delta(10, [output("a", "retrying")]));
  assert.deepEqual(seen, [7, 8, 9, 10]);
  assert.equal(stream.current().outputs[0].convergence, "retrying");
  stream.close();
});

test("a dropped connection resumes after a delay, from the last revision", async () => {
  const { openRuntimeStateStream } = await loadStream();
  const sources = [];
  const statuses = [];
  const stream = openRuntimeStateStream({
    onState: () => {},
    onStatus: (status) => statuses.push(status),
    reconnectDelayMs: 5,
    createEventSource: (url) => {
      const source = new FakeSource(url);
      sources.push(source);
      return source;
    },
  });
  sources[0].emit("open", {});
  sources[0].emit("snapshot", { epoch: "e1", revision: 2, pipelines: [], outputs: [] });
  sources[0].onerror();
  assert.equal(sources.length, 1, "no immediate reconnect loop");
  // An event from the closed connection does not mark the stream live.
  sources[0].emit("open", {});
  assert.deepEqual(statuses, ["live", "reconnecting"]);
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(sources.length, 2);
  assert.match(sources[1].url, /since=e1\.2$/);
  sources[1].emit("open", {});
  assert.deepEqual(statuses, ["live", "reconnecting", "live"]);
  stream.close();
});
