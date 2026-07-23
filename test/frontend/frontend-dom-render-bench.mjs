import { installFakeDom, loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

function makeOutput(index) {
  return {
    id: `out-${index + 1}`,
    pipe: "pipe-1",
    name: `Output ${index + 1}`,
    desiredState: "started",
    encoding: "source",
    url: `rtmp://example.com/live/${index + 1}`,
    monitoringUrl: index % 3 === 0 ? `https://example.com/monitor/${index + 1}` : null,
    status: "running",
    rawStatus: "running",
    phase: "sending",
    failurePhase: null,
    lastError: null,
    lastErrorAt: null,
    lastProgressAt: null,
    lastProgressAgeMs: null,
    retrying: false,
    retryAttempts: null,
    retryBackoffMs: null,
    nextRetryAt: null,
    retryRemainingMs: null,
    time: 10_000 + index * 100,
    job: null,
    totalSize: 2_000_000 + index * 25_000,
    bitrateKbps: 1_500 + (index % 10) * 50,
  };
}

function makePipeline(outputCount) {
  return {
    id: "pipe-1",
    name: "Hot Pipeline",
    key: "stream-key",
    inputSource: null,
    ingestUrls: { rtmp: null, srt: null },
    input: {
      status: "on",
      time: 30_000,
      probeReady: true,
      probeStatus: "ready",
      probePendingMs: null,
      video: null,
      audio: null,
      audioTracks: [],
      bytesReceived: 0,
      bytesSent: 0,
      readers: 0,
      bitrateKbps: 3200,
      publisher: null,
      unexpectedReadersCount: 0,
      lastSessionProtocol: null,
      lastDisconnectAt: null,
      lastDisconnectAgeMs: null,
      lastDisconnectReason: null,
      lastFailurePhase: null,
      recentDisconnectError: false,
      lastRemoteAddr: null,
      lastSessionBytesReceived: null,
    },
    outs: Array.from({ length: outputCount }, (_, index) => makeOutput(index)),
    stats: {
      inputBitrateKbps: 3200,
      outputBitrateKbps: 1800,
      readerCount: 0,
      outputCount,
      readerMismatch: false,
      unexpectedReadersCount: 0,
    },
    recording: { enabled: false, active: false },
    hlsPreview: {
      active: false,
      persistentConsumers: 0,
      lastAccessAgeMs: null,
      segments: 0,
      playlistBytes: 0,
    },
  };
}

function appendRoot(document, tagName, id) {
  const element = document.createElement(tagName);
  element.id = id;
  document.body.appendChild(element);
  return element;
}

function diffStats(after, before) {
  return {
    createElementCalls: after.createElementCalls - before.createElementCalls,
    innerHTMLWrites: after.innerHTMLWrites - before.innerHTMLWrites,
    textWrites: after.textWrites - before.textWrites,
    appendChildCalls: after.appendChildCalls - before.appendChildCalls,
    removeCalls: after.removeCalls - before.removeCalls,
    clearedChildren: after.clearedChildren - before.clearedChildren,
  };
}

function snapshotStats(stats) {
  return { ...stats };
}

async function runOptimizedBenchmark(outputCount, iterations, mutateTelemetry) {
  const { document } = installFakeDom();
  appendRoot(document, "div", "outs-col");

  const outputList = await loadCompiledFrontendModule("features/pipeline-output-list.js");
  const pipelineDeps = await loadCompiledFrontendModule(
    "features/pipeline-dependencies.js",
  );
  const { state } = await loadCompiledFrontendModule("core/state.js");
  const pipeline = makePipeline(outputCount);
  state.pipelines = [pipeline];
  let publicationCount = 0;
  let lastCardCount = 0;

  pipelineDeps.setPipelineViewDependencies({
    isOutputToggleBusy: () => false,
  });
  outputList.configurePipelineOutputOverviewPresentation({
    onPresentation: (model) => {
      publicationCount += 1;
      lastCardCount = model?.cards.length ?? 0;
    },
  });
  outputList.renderOutsColumn("pipe-1");
  const before = snapshotStats(document.stats);

  for (let iteration = 0; iteration < iterations; iteration += 1) {
    if (mutateTelemetry) {
      for (const output of pipeline.outs) {
        output.time += 5_000;
        output.totalSize += 100_000;
        output.bitrateKbps += 25;
      }
    }
    outputList.renderOutsColumn("pipe-1");
  }

  outputList.configurePipelineOutputOverviewPresentation({});
  return {
    publications: publicationCount,
    cards: lastCardCount,
    stats: diffStats(document.stats, before),
  };
}

async function runNaiveBenchmark(outputCount, iterations, mutateTelemetry) {
  const { buildPipelineOutputOverviewModel } = await loadCompiledFrontendModule(
    "features/pipeline-operate-view-model.js",
  );
  const pipeline = makePipeline(outputCount);
  let publications = 0;
  let cards = 0;

  for (let iteration = 0; iteration <= iterations; iteration += 1) {
    if (mutateTelemetry) {
      for (const output of pipeline.outs) {
        output.time += 5_000;
        output.totalSize += 100_000;
        output.bitrateKbps += 25;
      }
    }
    const model = buildPipelineOutputOverviewModel([pipeline], pipeline.id, [], true);
    publications += 1;
    cards = model?.cards.length ?? 0;
  }

  return { publications, cards };
}

async function main() {
  const outputCount = 125;
  const iterations = 100;

  console.log(
    "Frontend DOM churn benchmark (synthetic DOM-operation benchmark, not browser paint timing)",
  );

  for (const mutateTelemetry of [false, true]) {
    const label = mutateTelemetry ? "live telemetry" : "stable telemetry";
    const optimized = await runOptimizedBenchmark(
      outputCount,
      iterations,
      mutateTelemetry,
    );
    const naive = await runNaiveBenchmark(outputCount, iterations, mutateTelemetry);

    console.log(`\nScenario: ${label} / ${outputCount} outputs / ${iterations} refreshes`);
    console.log(
      `  optimized: publications=${optimized.publications}, createElement=${optimized.stats.createElementCalls}, appendChild=${optimized.stats.appendChildCalls}, remove=${optimized.stats.removeCalls}, clearedChildren=${optimized.stats.clearedChildren}, textWrites=${optimized.stats.textWrites}, innerHTMLWrites=${optimized.stats.innerHTMLWrites}, cards=${optimized.cards}`,
    );
    console.log(
      `  expanded:  publications=${naive.publications}, cards=${naive.cards}`,
    );
  }
}

await main();
process.exit(0);
