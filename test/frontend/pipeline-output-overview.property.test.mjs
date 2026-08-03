import assert from "node:assert/strict";
import test from "node:test";
import fc from "fast-check";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

// Property test (this repo's fast-check equivalent of backend proptest):
// generate randomized output fleets instead of hand-picked scenario fixtures,
// and assert structural invariants buildPipelineOutputOverviewModel must
// hold for any fleet shape, not just the cases the hand-written scenario
// suites happened to pick.

const statusArb = fc.constantFrom(
  "on",
  "running",
  "warning",
  "retrying",
  "failed",
  "error",
  "stalled",
  "off",
  "idle",
);

const outputArb = fc.record({
  name: fc.string({ minLength: 1, maxLength: 12 }),
  url: fc.constant("rtmp://example/live"),
  desiredState: fc.constantFrom("started", "stopped"),
  status: statusArb,
  phase: fc.option(fc.constantFrom("sending", "connecting", "idle"), {
    nil: null,
  }),
  failurePhase: fc.option(fc.string({ minLength: 1, maxLength: 8 }), {
    nil: null,
  }),
  lastError: fc.option(fc.string({ minLength: 1, maxLength: 12 }), {
    nil: null,
  }),
  lastProgressAgeMs: fc.option(fc.nat({ max: 60_000 }), { nil: null }),
  retrying: fc.boolean(),
  retryAttempts: fc.option(fc.nat({ max: 5 }), { nil: null }),
  retryRemainingMs: fc.option(fc.nat({ max: 30_000 }), { nil: null }),
  flapping: fc.boolean(),
  recentFailureCount: fc.nat({ max: 10 }),
  bitrateKbps: fc.nat({ max: 20_000 }),
  time: fc.nat({ max: 999_999 }),
  monitoringUrl: fc.option(fc.constant("http://monitor.example"), {
    nil: null,
  }),
});

const outputsArb = fc
  .array(outputArb, { minLength: 0, maxLength: 20 })
  .map((outputs) =>
    outputs.map((output, index) => ({ ...output, id: `out-${index}` })),
  );

function pipelineFrom(outputs) {
  return {
    id: "pipe-1",
    name: "Pipeline",
    key: "pipe-1-key",
    ingestUrls: { rtmp: null, srt: null },
    input: {
      status: "on",
      probeReady: true,
      flapping: false,
      disconnectGraceActive: false,
      recentDisconnectError: false,
      lastDisconnectReason: null,
      probePendingMs: null,
      time: null,
      video: null,
      audioTracks: [],
      unexpectedReadersCount: 0,
      recentDisconnectCount: 0,
      publisher: null,
    },
    outs: outputs,
    stats: {
      inputBitrateKbps: 0,
      outputBitrateKbps: 0,
      readerCount: 0,
      outputCount: outputs.length,
    },
    recording: { enabled: false, active: false },
    inputSource: null,
    fileIngest: null,
    hlsPreview: {
      active: false,
      persistentConsumers: 0,
      lastAccessAgeMs: null,
      segments: 0,
      playlistBytes: 0,
    },
  };
}

test("pipeline output overview model holds structural invariants for any output fleet", async () => {
  const { buildPipelineOutputOverviewModel } = await loadCompiledFrontendModule(
    "features/pipeline-operate-view-model.js",
  );
  const { isOutputRunning } = await loadCompiledFrontendModule(
    "core/output-status.js",
  );

  fc.assert(
    fc.property(outputsArb, fc.boolean(), (outputs, expanded) => {
      const pipeline = pipelineFrom(outputs);
      const model = buildPipelineOutputOverviewModel(
        [pipeline],
        pipeline.id,
        [],
        expanded,
      );

      // Every output is counted in exactly one status bucket: no output is
      // dropped or double-counted across the count breakdown.
      const countedTotal = model.counts.reduce(
        (sum, entry) => sum + entry.count,
        0,
      );
      assert.equal(countedTotal, outputs.length);

      // "N/total active" always agrees with the exported isOutputRunning
      // predicate, independent of the internal status-bucket classification.
      const expectedActive = outputs.filter(isOutputRunning).length;
      assert.equal(model.activeLabel, `${expectedActive}/${outputs.length} active`);

      // Attention is capped at 5 and only surfaces outputs whose status
      // tone is warning or error (score > 0 in the production classifier
      // never carries a success/neutral tone).
      assert.ok(model.attention.length <= 5);
      for (const item of model.attention) {
        assert.ok(
          item.status.tone === "warning" || item.status.tone === "error",
          `attention item "${item.id}" unexpectedly has tone "${item.status.tone}"`,
        );
      }
      const attentionIds = model.attention.map((item) => item.id);
      assert.equal(new Set(attentionIds).size, attentionIds.length);
      for (const id of attentionIds) {
        assert.ok(outputs.some((output) => output.id === id));
      }

      // Card pagination: capped at 8 unless expanded, and always a
      // same-order prefix of the underlying output list.
      const expectedCardCount = expanded
        ? outputs.length
        : Math.min(outputs.length, 8);
      assert.equal(model.cards.length, expectedCardCount);
      assert.deepEqual(
        model.cards.map((card) => card.id),
        outputs.slice(0, expectedCardCount).map((output) => output.id),
      );

      // canExpand and listCaption presence agree with the >8 threshold.
      const expectedCanExpand = outputs.length > 8;
      assert.equal(model.canExpand, expectedCanExpand);
      assert.equal(model.listCaption === null, !expectedCanExpand);
    }),
    { numRuns: 200 },
  );
});
