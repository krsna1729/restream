import assert from "node:assert/strict";
import test from "node:test";

import {
  installFakeDom,
  loadCompiledFrontendModule,
} from "../support/helpers/fake-dom.mjs";

class FakeEventSource {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSED = 2;
  static streams = [];

  constructor(url) {
    this.url = String(url);
    this.readyState = FakeEventSource.OPEN;
    this.closed = false;
    this.handlers = new Map();
    this.onerror = null;
    FakeEventSource.streams.push(this);
  }

  addEventListener(type, handler) {
    const handlers = this.handlers.get(type) || [];
    handlers.push(handler);
    this.handlers.set(type, handlers);
  }

  emitJson(type, payload) {
    for (const handler of this.handlers.get(type) || []) {
      handler({ data: JSON.stringify(payload) });
    }
  }

  emitRaw(type, data) {
    for (const handler of this.handlers.get(type) || []) handler({ data });
  }

  fail(readyState) {
    this.readyState = readyState;
    this.onerror?.(new Event("error"));
  }

  close() {
    this.closed = true;
    this.readyState = FakeEventSource.CLOSED;
  }
}

function installFakeEventSource() {
  FakeEventSource.streams = [];
  Object.defineProperty(globalThis, "EventSource", {
    value: FakeEventSource,
    configurable: true,
  });
}

test("managed log stream keeps one connection for one filter and suppresses replayed ids", async () => {
  installFakeDom();
  installFakeEventSource();
  const { createManagedLogStream } =
    await loadCompiledFrontendModule("core/log-stream.js");
  const stream = createManagedLogStream();
  const first = [];
  const updated = [];

  stream.sync({
    filters: { pipelineId: "pipe-1", eventClass: "lifecycle" },
    resumeAfterId: 10,
    onLog: (log) => first.push(log.id),
  });
  assert.equal(FakeEventSource.streams.length, 1);
  assert.equal(
    FakeEventSource.streams[0].url,
    "/api/v1/logs/stream?pipeline_id=pipe-1&event_class=lifecycle&last_event_id=10",
  );

  stream.sync({
    filters: { pipelineId: "pipe-1", eventClass: "lifecycle" },
    resumeAfterId: 10,
    onLog: (log) => updated.push(log.id),
  });
  assert.equal(FakeEventSource.streams.length, 1);

  FakeEventSource.streams[0].emitJson("log", { id: 10 });
  FakeEventSource.streams[0].emitJson("log", { id: 12 });
  FakeEventSource.streams[0].emitJson("log", { id: 11 });
  FakeEventSource.streams[0].emitRaw("log", "not json");

  assert.deepEqual(first, []);
  assert.deepEqual(updated, [12]);
  assert.equal(stream.getLastEventId(), 12);
});

test("managed log stream replaces changed scope and ignores the old generation", async () => {
  installFakeDom();
  installFakeEventSource();
  const { createManagedLogStream } =
    await loadCompiledFrontendModule("core/log-stream.js");
  const stream = createManagedLogStream();
  const received = [];

  stream.sync({
    filters: { pipelineId: "pipe-1" },
    resumeAfterId: 3,
    onLog: (log) => received.push(`one:${log.id}`),
  });
  const oldSource = FakeEventSource.streams[0];
  stream.sync({
    filters: { pipelineId: "pipe-2" },
    resumeAfterId: 20,
    onLog: (log) => received.push(`two:${log.id}`),
  });

  assert.equal(oldSource.closed, true);
  assert.equal(FakeEventSource.streams.length, 2);
  assert.equal(
    FakeEventSource.streams[1].url,
    "/api/v1/logs/stream?pipeline_id=pipe-2&last_event_id=20",
  );
  oldSource.emitJson("log", { id: 4 });
  FakeEventSource.streams[1].emitJson("log", { id: 21 });
  assert.deepEqual(received, ["two:21"]);
});

test("managed log stream leaves transient reconnect to EventSource and reports terminal closure", async () => {
  installFakeDom();
  installFakeEventSource();
  const { createManagedLogStream } =
    await loadCompiledFrontendModule("core/log-stream.js");
  const stream = createManagedLogStream();
  let unavailable = 0;

  stream.sync({
    filters: { scope: "restream" },
    onLog: () => {},
    onUnavailable: () => {
      unavailable += 1;
    },
  });
  const source = FakeEventSource.streams[0];
  source.fail(FakeEventSource.CONNECTING);
  assert.equal(source.closed, false);
  assert.equal(unavailable, 0);

  source.fail(FakeEventSource.CLOSED);
  assert.equal(source.closed, true);
  assert.equal(unavailable, 1);

  stream.sync({
    filters: { scope: "restream" },
    resumeAfterId: 7,
    onLog: () => {},
    onUnavailable: () => {
      unavailable += 1;
    },
  });
  assert.equal(FakeEventSource.streams.length, 2);
  assert.equal(
    FakeEventSource.streams[1].url,
    "/api/v1/logs/stream?scope=restream&last_event_id=7",
  );
});
