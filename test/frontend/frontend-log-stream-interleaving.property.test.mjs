import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import { installFakeDom, loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

// The reconnect/backoff stream contracts (frontend-log-stream.test.mjs and
// friends) are all hand-picked scenarios. This is the frontend equivalent of
// a loom model: instead of one scripted race, replay many randomized
// interleavings of sync()/close() calls against events arriving from *any*
// EventSource instance, including ones the module has already superseded,
// and prove the module's `source !== openedSource` staleness guard holds
// under all of them — a stale source's event must never reach onLog.

class FakeEventSource {
  static instances = [];

  constructor(url) {
    this.url = String(url);
    this.readyState = 1; // OPEN
    this.listeners = new Map();
    this.onerror = null;
    FakeEventSource.instances.push(this);
  }

  addEventListener(type, handler) {
    const handlers = this.listeners.get(type) || [];
    handlers.push(handler);
    this.listeners.set(type, handlers);
  }

  close() {
    this.readyState = 2; // CLOSED
  }

  // Emits regardless of close()/staleness: a real EventSource can have an
  // event already queued in the task queue before JS calls close() on it,
  // so the fake must be able to reproduce that ordering too.
  emitLog(id) {
    for (const handler of this.listeners.get("log") || []) {
      handler({
        data: JSON.stringify({
          id,
          level: "INFO",
          target: "restream",
          message: `log ${id}`,
          ts: "2026-01-01T00:00:00.000Z",
          fields: null,
          pipelineId: null,
          outputId: null,
          eventType: null,
        }),
      });
    }
  }
}

function installFakeEventSource() {
  FakeEventSource.instances = [];
  Object.defineProperty(globalThis, "EventSource", {
    value: FakeEventSource,
    configurable: true,
  });
}

const filterArb = fc.constantFrom("pipe-a", "pipe-b", "pipe-c");
const actionArb = fc.oneof(
  filterArb.map((filter) => ({ kind: "sync", filter })),
  fc.constant({ kind: "unsync" }),
  fc
    .tuple(fc.nat({ max: 5 }), fc.integer({ min: 1, max: 500 }))
    .map(([sourceIndex, logId]) => ({ kind: "emit", sourceIndex, logId })),
);

test("managed log stream never delivers a superseded source's events, under any action interleaving", async () => {
  await fc.assert(
    fc.asyncProperty(
      fc.array(actionArb, { minLength: 1, maxLength: 40 }),
      async (actions) => {
        installFakeDom();
        installFakeEventSource();
        const { createManagedLogStream } =
          await loadCompiledFrontendModule("core/log-stream.js");
        const stream = createManagedLogStream();

        // liveSourceIndex is derived purely from observable driver actions
        // (which sync() calls caused a *new* EventSource to be constructed),
        // never from the module's internal generation/source variables.
        let liveSourceIndex = -1;
        let emittingSourceIndex = -1;
        const delivered = [];
        const onLog = () =>
          delivered.push({
            sourceIndexAtEmit: emittingSourceIndex,
            liveSourceIndexAtDelivery: liveSourceIndex,
          });

        for (const action of actions) {
          if (action.kind === "sync") {
            const before = FakeEventSource.instances.length;
            stream.sync({ filters: { pipelineId: action.filter }, onLog });
            if (FakeEventSource.instances.length > before) {
              liveSourceIndex = FakeEventSource.instances.length - 1;
            }
          } else if (action.kind === "unsync") {
            stream.sync(null);
            liveSourceIndex = -1;
          } else {
            const source = FakeEventSource.instances[action.sourceIndex];
            if (!source) continue;
            emittingSourceIndex = action.sourceIndex;
            source.emitLog(action.logId);
          }
        }

        for (const entry of delivered) {
          assert.equal(
            entry.sourceIndexAtEmit,
            entry.liveSourceIndexAtDelivery,
            "onLog fired for a source that was not the live source at delivery time",
          );
        }
      },
    ),
    { numRuns: 300 },
  );
});
