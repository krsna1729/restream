import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { destinationOutlier, inputTransport, mediaPath } = await loadCompiledFrontendModule(
  "control-room/monitor-path.js",
);

function output(id, overrides = {}) {
  return {
    id,
    pipelineId: "p",
    name: id,
    protocol: "rtmp",
    shard: null,
    stage: "p:source",
    desired: "running",
    actual: "sending",
    convergence: "converged",
    flow: "flowing",
    backpressure: null,
    retry: null,
    evidence: null,
    diagnosis: null,
    ...overrides,
  };
}

function egress(outputId, deliveryRatio, extra = {}) {
  return {
    outputId,
    quality: { deliveryRatio, deliveredBps: deliveryRatio * 1e6, offeredBps: 1e6 },
    feedLagUnits: 2,
    ...extra,
  };
}

function input(overrides = {}, quality = {}) {
  return {
    id: "in",
    label: "Main",
    selected: true,
    runtime: { connected: true, protocol: "srt", quality },
    ...overrides,
  };
}

test("input transport shows what the selected input reports and omits the rest", () => {
  const transport = inputTransport([
    input({ id: "backup", label: "Backup", selected: false }, { msRtt: 99 }),
    input(
      {},
      {
        msRtt: 21.4,
        mbpsReceiveRate: 8.1,
        srtRecvBufPackets: 14,
        srtRecvBufCapacityPackets: 100,
        packetsReceivedLossPerSec: 0,
        srtBonded: true,
        srtGroupConnectedMembers: 1,
        srtGroupMemberCount: 2,
      },
    ),
  ]);
  assert.equal(transport.label, "Main");
  assert.equal(transport.protocol, "SRT");
  assert.deepEqual(transport.rows.map(([label]) => label), [
    "RTT",
    "Receive rate",
    "Receive buffer",
    // A reported zero is a value; unreported drop and retrans rates are left out.
    "Loss / s",
    "Bonded links",
  ]);
  assert.deepEqual(transport.rows[0], ["RTT", "21 ms"]);
  assert.deepEqual(transport.rows[2], ["Receive buffer", "14%"]);
  assert.deepEqual(transport.rows[4], ["Bonded links", "1 / 2 connected"]);

  // RTMP reports TCP's RTT; an offline selected input has no transport.
  assert.deepEqual(inputTransport([input({}, { tcpRttMs: 3 })]).rows, [["RTT", "3 ms"]]);
  assert.equal(
    inputTransport([input({ runtime: { connected: false, quality: { msRtt: 1 } } })]),
    null,
  );
});

test("the media path counts each stage's wanted readers and every overflow once", () => {
  const graph = {
    pipelineId: "p",
    nodes: [
      {
        id: "rb",
        type: "ring_buffer",
        label: "Source ring",
        active: true,
        details: {
          fill: 37,
          capacity: 4096,
          readers: [
            { name: "p:video:720p", lagSlots: 3, overflowCount: 2 },
            { name: "p:source", lagSlots: 9, overflowCount: 1 },
          ],
        },
      },
      {
        id: "s720",
        type: "transcoder",
        label: "720p",
        active: true,
        stageKey: "p:video:720p",
        metrics: { packetsPerSec: 2790.4 },
        details: { overflowCount: 2 },
      },
      { id: "src", type: "source", label: "Source", active: true, stageKey: "p:source" },
      // An egress node names its output: it is not a stage of the path.
      { id: "e", type: "egress", label: "out", active: true, stageKey: "p:source", outputId: "a" },
    ],
    edges: [],
  };
  const path = mediaPath(graph, [
    output("a", { stage: "p:video:720p" }),
    output("b", { stage: "p:video:720p" }),
    output("c"),
    output("off", { desired: "stopped" }),
    output("other", { pipelineId: "q", stage: "p:source" }),
  ]);
  // A stage node's overflowCount repeats its ring reader's: counted once.
  assert.deepEqual(path.ring, { fill: 37, capacity: 4096, overflows: 3, worstLagSlots: 9 });
  assert.deepEqual(
    path.stages.map((stage) => [stage.label, stage.outputs, stage.packetsPerSec]),
    [
      ["720p", 2, 2790.4],
      ["Source", 1, null],
    ],
  );
  assert.equal(mediaPath({ pipelineId: "p", nodes: [], edges: [] }, []).ring, null);
});

test("the outlier is the wanted output that delivers least, under the floor", () => {
  const outputs = [
    output("a"),
    output("b"),
    output("c"),
    output("srt", { protocol: "srt" }),
    output("off", { desired: "stopped" }),
    output("q", { pipelineId: "q" }),
  ];
  const egresses = [
    egress("a", 0.99),
    egress("b", 0.83),
    egress("c", 1.01),
    egress("srt", 0.5),
    egress("off", 0.1),
    egress("q", 0.2),
  ];
  const outlier = destinationOutlier("p", outputs, egresses);
  assert.equal(outlier.outputId, "srt");
  // Its peers read the same stage over the same protocol: there are none.
  assert.equal(outlier.peersRatio, null);
  assert.equal(outlier.offeredBps, 1e6);

  const rtmp = destinationOutlier("p", outputs.slice(0, 3), egresses);
  assert.equal(rtmp.outputId, "b");
  // Two peers: the median is their mean.
  assert.equal(rtmp.peersRatio, 1);

  // Outputs with no running stage share nothing: they are not peers.
  const unplaced = destinationOutlier(
    "p",
    [output("x", { stage: null }), output("y", { stage: null })],
    [egress("x", 0.5), egress("y", 1)],
  );
  assert.equal(unplaced.outputId, "x");
  assert.equal(unplaced.peersRatio, null);

  // At the floor an output keeps up; an unrated output is not an outlier.
  assert.equal(destinationOutlier("p", [output("a")], [egress("a", 0.95)]), null);
  assert.equal(destinationOutlier("p", [output("a")], [{ outputId: "a", quality: {} }]), null);
});
