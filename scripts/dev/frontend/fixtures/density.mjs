// A seeded synthetic restream deployment for frontend scale and ergonomics
// work: runtime state, telemetry, host metrics, alerts, events and history
// shaped like the engine's payloads (web/ts/core/generated/runtime-state.ts,
// web/ts/core/api-types.ts, web/ts/types.ts). `tick()` advances it like a
// live plant: rates drift, outputs fail, retry and recover, inputs fail
// over, and a scripted storm saturates one egress shard. It never claims to
// be measured data; the fixture server labels every page as synthetic.

export const PRESETS = {
  empty: { pipelines: 0, outputs: 0, backupRatio: 0, shardsPerProtocol: 1, cores: 8 },
  small: { pipelines: 5, outputs: 24, backupRatio: 0.4, shardsPerProtocol: 2, cores: 8 },
  plant: { pipelines: 150, outputs: 3000, backupRatio: 0.9, shardsPerProtocol: 12, cores: 48 },
  extreme: { pipelines: 400, outputs: 10000, backupRatio: 1, shardsPerProtocol: 32, cores: 64 },
};

/** mulberry32: small, fast, deterministic. */
export function rng(seed) {
  let a = seed >>> 0;
  const next = () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  return {
    next,
    int: (lo, hi) => lo + Math.floor(next() * (hi - lo + 1)),
    pick: (list) => list[Math.floor(next() * list.length)],
    chance: (p) => next() < p,
    jitter: (value, spread) => value * (1 + (next() * 2 - 1) * spread),
  };
}

const PLATFORMS = [
  { id: "youtube", label: "YouTube", protocol: "rtmp", url: "rtmp://a.rtmp.youtube.com/live2", weight: 30 },
  { id: "twitch", label: "Twitch", protocol: "rtmp", url: "rtmp://live.twitch.tv/app", weight: 14 },
  { id: "facebook", label: "Facebook", protocol: "rtmp", url: "rtmps://live-api-s.facebook.com:443/rtmp", weight: 12 },
  { id: "kick", label: "Kick", protocol: "rtmp", url: "rtmps://fa723fc1b171.global-contribute.live-video.net/app", weight: 6 },
  { id: "x", label: "X", protocol: "rtmp", url: "rtmp://va.pscp.tv:80/x", weight: 5 },
  { id: "linkedin", label: "LinkedIn", protocol: "rtmp", url: "rtmps://live.linkedin.com/live", weight: 4 },
  { id: "cdn-srt", label: "CDN SRT", protocol: "srt", url: "srt://ingest.cdn.example:9000", weight: 16 },
  { id: "partner-hls", label: "Partner HLS", protocol: "hls", url: "https://push.partner.example/live", weight: 7 },
  { id: "moq-relay", label: "MoQ relay", protocol: "moq", url: "https://relay.moq.example/anon", weight: 3 },
  { id: "custom", label: "Custom RTMP", protocol: "rtmp", url: "rtmp://origin.example.net/live", weight: 3 },
];
const PLATFORM_TOTAL = PLATFORMS.reduce((sum, platform) => sum + platform.weight, 0);

const SHOW_WORDS = [
  "Temple", "Main", "Studio", "Arena", "Court", "Stage", "Newsroom", "Chapel", "Field", "Hall",
  "Plaza", "Cathedral", "Paddock", "Pitch", "Theatre", "Desk", "Booth", "Gate", "Garden", "Dome",
];
const RENDITIONS = ["source", "h264_1080p", "h264_720p", "h264_480p", "h265_1080p"];

function pickPlatform(random) {
  let roll = random.next() * PLATFORM_TOTAL;
  for (const platform of PLATFORMS) {
    roll -= platform.weight;
    if (roll <= 0) return platform;
  }
  return PLATFORMS[0];
}

function id(prefix, index) {
  return `${prefix}_${index.toString(36).padStart(4, "0")}`;
}

/**
 * Builds a deployment. `preset` names a size in PRESETS; `seed` makes it
 * reproducible. The returned object owns its clock-driven evolution.
 */
export function createDeployment({ preset = "plant", seed = 7, nowMs = Date.now() } = {}) {
  const size = PRESETS[preset] ?? PRESETS.plant;
  const random = rng(seed);
  const epoch = `fx${seed.toString(36)}${preset}`;
  let revision = 1;
  let tickCount = 0;
  const startedMs = nowMs;

  const pipelines = [];
  const inputs = [];
  const outputs = [];
  const sim = new Map(); // id -> mutable simulation details not in the state

  for (let index = 0; index < size.pipelines; index += 1) {
    const pid = id("pl", index);
    const name = `${random.pick(SHOW_WORDS)} ${random.pick(SHOW_WORDS)} ${index + 1}`;
    const ingestProtocol = random.pick(["rtmp", "rtmp", "srt", "srt", "hls", "moq"]);
    const bitrateBps = random.pick([3_500_000, 4_500_000, 6_000_000, 8_000_000, 12_000_000]);
    const height = bitrateBps >= 8_000_000 ? 1080 : bitrateBps >= 4_500_000 ? 720 : 540;
    const live = random.chance(0.96);
    const backups = random.chance(size.backupRatio) ? random.int(1, 2) : 0;
    const bonded = ingestProtocol === "srt" && random.chance(0.3);
    pipelines.push({
      id: pid,
      name,
      ingest: null,
      standbyInputs: 0,
      recording: random.chance(0.12),
    });
    sim.set(pid, {
      bitrateBps,
      fps: random.pick([25, 29.97, 30, 50, 59.94, 60]),
      gopSec: random.pick([1, 2, 2, 2, 4, 6]),
      ingestProtocol,
      height,
      live,
      bonded,
      bytesIn: 0,
      ringCapacity: 4096,
      ringFill: random.int(400, 2200),
      overflows: 0,
      stages: new Set(),
      ladder: random.chance(0.25),
    });
    for (let position = 0; position <= backups; position += 1) {
      const inputId = `${pid}_in${position}`;
      const kind = position > 0 && random.chance(0.2) ? "hlsPull" : "keyed";
      const streamKey = `sk-${pid}-${position}`;
      inputs.push({
        id: inputId,
        pipelineId: pid,
        position,
        label: position === 0 ? "Primary" : `Backup ${position}`,
        streamKey,
        role: position === 0 ? "primary" : "backup",
        enabled: true,
        selected: position === 0,
        kind,
        sourceUrl: kind === "hlsPull" ? `https://origin.example/${pid}/master.m3u8` : null,
        variantRule: "highestBandwidth",
        authKind: "none",
        authUsername: null,
        pushAuthKind: "none",
        pushAuthUsername: null,
        ingestUrls:
          kind === "keyed"
            ? {
                rtmp: `rtmp://ingest.restream.local/live/${streamKey}`,
                srt: `srt://ingest.restream.local:9710?streamid=${streamKey}`,
                hls: `https://ingest.restream.local/ingest/hls/${streamKey}/index.m3u8`,
                moq: `https://ingest.restream.local:4443/${streamKey}`,
              }
            : null,
        previewUrl: `/hls/inputs/${inputId}/master.m3u8`,
        runtime: {
          connected: false,
          forwardingState: null,
          protocol: null,
          remoteAddr: null,
          video: null,
          audio: null,
        },
      });
      sim.set(inputId, { bytesIn: 0, rttMs: random.int(8, 140), lossPerSec: 0, uptime: 0 });
    }
  }

  // Outputs: a heavy-tailed spread, so a few pipelines carry hundreds of
  // destinations and most carry a handful.
  const weights = pipelines.map(() => Math.pow(random.next(), 3) + 0.05);
  const weightTotal = weights.reduce((sum, weight) => sum + weight, 0);
  let outputIndex = 0;
  pipelines.forEach((pipeline, pipelineIndex) => {
    const share = Math.max(1, Math.round((weights[pipelineIndex] / weightTotal) * size.outputs));
    const state = sim.get(pipeline.id);
    for (let n = 0; n < share && outputIndex < size.outputs; n += 1) {
      const platform = pickPlatform(random);
      const rendition = state.ladder && random.chance(0.4) ? random.pick(RENDITIONS.slice(1, 4)) : "source";
      const stage = `${pipeline.id}:${rendition}`;
      state.stages.add(rendition);
      const oid = id("out", outputIndex);
      outputIndex += 1;
      const desired = random.chance(0.93) ? "running" : "stopped";
      outputs.push({
        id: oid,
        pipelineId: pipeline.id,
        name: `${platform.label} ${n + 1}`,
        protocol: platform.protocol,
        shard: null,
        stage: null,
        desired,
        actual: null,
        convergence: desired === "running" ? "starting" : "converged",
        flow: "idle",
        backpressure: null,
        retry: null,
        evidence: null,
        diagnosis: null,
      });
      sim.set(oid, {
        platform,
        rendition,
        stageKey: stage,
        shard: random.int(0, size.shardsPerProtocol - 1),
        bytesOut: 0,
        uptime: 0,
        health: "ok", // ok | rejected | slow | retrying | exhausted
        attempts: 0,
        nextRetryMs: 0,
        rttMs: random.int(15, 220),
        lastError: null,
        failurePhase: null,
      });
    }
  });

  const shards = [];
  for (const protocol of ["rtmp", "srt", "hls", "moq"]) {
    for (let shardIndex = 0; shardIndex < size.shardsPerProtocol; shardIndex += 1) {
      shards.push({
        protocol,
        feedId: `fabric-${protocol}`,
        shardIndex,
        state: "healthy",
        commandDepth: random.int(0, 3),
        commandCapacity: 256,
        progressAgeMs: random.int(1, 30),
      });
    }
  }

  const pipelineById = new Map(pipelines.map((pipeline) => [pipeline.id, pipeline]));
  const outputById = new Map(outputs.map((output) => [output.id, output]));
  const outputsByPipeline = new Map(pipelines.map((pipeline) => [pipeline.id, []]));
  for (const output of outputs) outputsByPipeline.get(output.pipelineId).push(output);
  const inputsByPipeline = new Map(pipelines.map((pipeline) => [pipeline.id, []]));
  for (const input of inputs) inputsByPipeline.get(input.pipelineId).push(input);
  const signature = (output) =>
    `${output.desired}|${output.actual}|${output.convergence}|${output.flow}|${output.backpressure}|${output.retry?.attempts}|${output.diagnosis?.cause.code}|${output.shard}`;

  const alertsOpen = new Map();
  const resolved = [];
  const events = [];
  let eventSeq = 1;
  const fleetSamples = [];
  const flight = new Map(); // pipelineId -> samples
  // Pipelines whose picture and sound checks are on: the first, so the
  // demo shows them; the switch adds and removes others.
  const qaOn = new Set(pipelines.slice(0, 1).map((pipeline) => pipeline.id));
  let storm = null; // { shard, protocol, untilTick }

  const emit = (event) => {
    events.push({ seq: eventSeq++, timestamp: new Date(event.atMs).toISOString(), ...event, atMs: undefined });
    if (events.length > 1000) events.shift();
  };

  function selectedInput(pipelineId) {
    return inputsByPipeline.get(pipelineId).find((input) => input.selected);
  }

  function setIngest(pipeline) {
    const input = selectedInput(pipeline.id);
    const state = sim.get(pipeline.id);
    pipeline.ingest =
      input && input.runtime.connected
        ? {
            inputId: input.id,
            protocol: input.runtime.protocol ?? state.ingestProtocol,
            videoCodec: "h264",
            bonded: state.bonded && input.runtime.protocol === "srt",
          }
        : null;
    pipeline.standbyInputs = inputsByPipeline
      .get(pipeline.id)
      .filter((candidate) => candidate.runtime.connected && !candidate.selected).length;
  }

  function connectInput(input, connected) {
    const state = sim.get(input.pipelineId);
    input.runtime = connected
      ? {
          connected: true,
          forwardingState: input.selected ? "active" : "standby",
          protocol: input.kind === "hlsPull" ? "hls" : state.ingestProtocol,
          remoteAddr: `203.0.113.${(input.position * 37 + input.pipelineId.length * 11) % 250}:${40000 + input.position}`,
          video: { codec: "h264", width: Math.round((state.height * 16) / 9), height: state.height },
          audio: { codec: "aac", sampleRate: 48000, channels: 2 },
        }
      : {
          connected: false,
          forwardingState: null,
          protocol: null,
          remoteAddr: null,
          video: null,
          audio: null,
        };
  }

  // Initial runtime: live pipelines have their inputs connected.
  for (const input of inputs) {
    const state = sim.get(input.pipelineId);
    connectInput(input, state.live && (input.selected || random.chance(0.8)));
  }
  pipelines.forEach(setIngest);

  function settleOutput(output, now) {
    const state = sim.get(output.id);
    const pipeline = pipelineById.get(output.pipelineId);
    const shardStalled =
      storm && storm.protocol === output.protocol && storm.shard === state.shard && output.desired === "running";
    output.retry = null;
    output.evidence = null;
    output.diagnosis = null;
    output.backpressure = null;
    if (output.desired === "stopped") {
      output.actual = null;
      output.convergence = "converged";
      output.flow = "idle";
      output.shard = null;
      output.stage = null;
      return;
    }
    if (!pipeline.ingest) {
      output.actual = "waitingUpstream";
      output.convergence = "waitingForIngest";
      output.flow = "idle";
      output.shard = null;
      output.stage = null;
      output.diagnosis = { layer: "ingest", cause: { code: "noPublisher" }, ruledOut: [] };
      return;
    }
    output.shard = state.shard;
    output.stage = state.stageKey;
    if (shardStalled) {
      output.actual = "sending";
      output.convergence = "converged";
      output.flow = "stalled";
      output.backpressure = "feedLag";
      output.diagnosis = {
        layer: "shard",
        cause: { code: "shardStalled", shard: state.shard },
        ruledOut: ["ingest", "feed"],
      };
      return;
    }
    switch (state.health) {
      case "retrying":
        output.actual = "retrying";
        output.convergence = "retrying";
        output.flow = "idle";
        output.retry = { attempts: state.attempts, nextAttemptAtMs: state.nextRetryMs };
        output.evidence = { error: state.lastError, phase: state.failurePhase, atMs: now - 4000 };
        output.diagnosis = {
          layer: "destination",
          cause:
            state.attempts >= 8
              ? { code: "retryBudgetNearlySpent", attempts: state.attempts, max: 10 }
              : { code: "rejected", phase: state.failurePhase, error: state.lastError },
          ruledOut: ["ingest", "feed", "shard"],
        };
        return;
      case "exhausted":
        output.actual = "failed";
        output.convergence = "retriesExhausted";
        output.flow = "idle";
        output.evidence = { error: state.lastError, phase: state.failurePhase, atMs: now - 60_000 };
        output.diagnosis = { layer: "destination", cause: { code: "retriesExhausted" }, ruledOut: ["ingest", "feed", "shard"] };
        return;
      case "slow":
        output.actual = output.protocol === "hls" ? "uploading" : "sending";
        output.convergence = "converged";
        output.flow = "flowing";
        output.backpressure = "socket";
        output.diagnosis = {
          layer: "destination",
          cause: { code: "backpressured", stalled: false },
          ruledOut: ["ingest", "feed", "shard"],
        };
        return;
      default:
        output.actual = output.protocol === "hls" ? "uploading" : "sending";
        output.convergence = "converged";
        output.flow = "flowing";
    }
  }

  const ERRORS = [
    ["connect", "Connection refused (os error 111)"],
    ["handshake", "RTMP handshake timed out after 10s"],
    ["publish", "NetStream.Publish.BadName: stream key rejected"],
    ["upload_segment", "PUT segment returned 503 Service Unavailable"],
    ["connect", "SRT connection rejected: peer timed out"],
  ];

  function failOutput(output, now) {
    const state = sim.get(output.id);
    const [phase, error] = random.pick(ERRORS);
    state.health = "retrying";
    state.attempts = 1;
    state.failurePhase = phase;
    state.lastError = error;
    state.nextRetryMs = now + 5000;
    state.uptime = 0;
    emit({ atMs: now, kind: "egress.failed", pipelineId: output.pipelineId, outputId: output.id, protocol: output.protocol, phase, error });
  }

  outputs.forEach((output) => {
    const state = sim.get(output.id);
    if (output.desired === "running" && random.chance(0.012)) failOutput(output, nowMs);
    else if (output.desired === "running" && random.chance(0.01)) state.health = "slow";
    else if (output.desired === "running" && random.chance(0.004)) {
      state.health = "exhausted";
      state.lastError = "Connection refused (os error 111)";
      state.failurePhase = "connect";
    }
    settleOutput(output, nowMs);
  });

  const graphs = pipelines.map((pipeline) => buildGraph(pipeline));
  const graphIndex = new Map(pipelines.map((pipeline, index) => [pipeline.id, index]));

  function buildGraph(pipeline) {
    const state = sim.get(pipeline.id);
    const nodes = [
      { id: "ingest", type: "ingest", label: `${state.ingestProtocol.toUpperCase()} ingest`, active: !!pipeline.ingest, stageKey: null, outputId: null, metricsStage: null },
      { id: "demux", type: "demux", label: "Demux", active: !!pipeline.ingest, stageKey: null, outputId: null, metricsStage: null },
      { id: "ring", type: "ring_buffer", label: "Source ring", active: !!pipeline.ingest, stageKey: null, outputId: null, metricsStage: null },
    ];
    const edges = [
      { from: "ingest", to: "demux", label: "packets" },
      { from: "demux", to: "ring", label: "packets" },
    ];
    for (const rendition of state.stages) {
      const key = `${pipeline.id}:${rendition}`;
      const type = rendition === "source" ? "passthrough" : "transcoder";
      nodes.push({ id: `stage:${rendition}`, type, label: rendition === "source" ? "Passthrough" : rendition, active: !!pipeline.ingest, stageKey: rendition, outputId: null, metricsStage: key });
      edges.push({ from: "ring", to: `stage:${rendition}`, label: "packets" });
    }
    for (const output of outputsByPipeline.get(pipeline.id)) {
      const rendition = sim.get(output.id).rendition;
      nodes.push({ id: `egress:${output.id}`, type: "egress", label: output.name, active: output.flow === "flowing", stageKey: null, outputId: output.id, metricsStage: null });
      edges.push({ from: `stage:${rendition}`, to: `egress:${output.id}`, label: output.protocol });
    }
    if (pipeline.recording) {
      nodes.push({ id: "recording", type: "recording", label: "Recording", active: true, stageKey: null, outputId: null, metricsStage: null });
      edges.push({ from: "ring", to: "recording", label: "packets" });
    }
    return { pipelineId: pipeline.id, nodes, edges };
  }

  /** As the engine stamps them: since when each output has been in its
   * state and each ingest connected. The first stamps spread over the last
   * few hours, so a fresh plant reads as one that has been running. */
  const sinceKeys = new Map();
  function stampSince(now) {
    for (const output of outputs) {
      const key = `${output.desired}|${output.convergence}|${output.flow}|${output.diagnosis ? 1 : 0}`;
      const held = sinceKeys.get(output.id);
      if (!held || held.key !== key) sinceKeys.set(output.id, { key, since: held ? now : now - random.int(60_000, 6 * 3_600_000) });
      output.stateSinceMs = sinceKeys.get(output.id).since;
    }
    for (const pipeline of pipelines) {
      if (!pipeline.ingest) continue;
      const key = `ingest:${pipeline.ingest.inputId}`;
      const held = sinceKeys.get(pipeline.id);
      if (!held || held.key !== key) sinceKeys.set(pipeline.id, { key, since: held ? now : now - random.int(600_000, 8 * 3_600_000) });
      pipeline.ingest.sinceMs = sinceKeys.get(pipeline.id).since;
    }
  }

  function state() {
    return {
      epoch,
      revision,
      pipelines: structuredClone(pipelines),
      outputs: structuredClone(outputs),
      inputs: structuredClone(inputs),
      graphs: structuredClone(graphs),
    };
  }

  const ALERT_CAUSES = {
    rejected: "Destination rejected the stream",
    retryBudgetNearlySpent: "Retry budget nearly spent",
    retriesExhausted: "Retries exhausted",
    shardStalled: "Egress shard stalled",
    noPublisher: "No publisher on a pipeline with wanted outputs",
    backpressured: "Destination is not keeping up",
  };

  function deriveAlerts(now) {
    const current = new Map();
    for (const output of outputs) {
      if (!output.diagnosis) continue;
      const code = output.diagnosis.cause.code;
      if (output.diagnosis.layer === "ingest") {
        const key = `pipeline:${output.pipelineId}:noPublisher`;
        if (!current.has(key)) {
          current.set(key, {
            id: key,
            severity: "warning",
            scope: "pipeline",
            pipelineId: output.pipelineId,
            title: "Pipeline has no live source",
            cause: ALERT_CAUSES.noPublisher,
            evidence: [{ signal: "no publisher connected on any enabled input", confidence: "measured" }],
            recommendedAction: "Start the encoder or promote a backup input.",
          });
        }
        continue;
      }
      if (output.diagnosis.layer === "shard") {
        const key = `stage:${output.protocol}-shard-${output.shard}:stalled`;
        if (!current.has(key)) {
          current.set(key, {
            id: key,
            severity: "critical",
            scope: "stage",
            pipelineId: output.pipelineId,
            stageId: `${output.protocol}-shard-${output.shard}`,
            title: `${output.protocol.toUpperCase()} shard ${output.shard} stalled`,
            cause: ALERT_CAUSES.shardStalled,
            evidence: [
              { signal: "shard progress age above 5 s", confidence: "measured" },
              { signal: "command queue near its bound", confidence: "measured" },
              { signal: "every output on the shard stalled together", confidence: "derived" },
            ],
            recommendedAction: "Move outputs off the shard or reduce load; check host CPU.",
          });
        }
        continue;
      }
      const key = `output:${output.id}:${code}`;
      current.set(key, {
        id: key,
        severity: code === "retriesExhausted" || code === "retryBudgetNearlySpent" ? "critical" : "warning",
        scope: "output",
        pipelineId: output.pipelineId,
        outputId: output.id,
        title: `${output.name}: ${ALERT_CAUSES[code] ?? code}`,
        cause: ALERT_CAUSES[code] ?? code,
        evidence: [
          { signal: output.evidence?.error ?? "delivery below the 0.95 floor", confidence: "measured" },
          { signal: "ingest, feed and shard healthy", confidence: "derived" },
        ],
        recommendedAction: code === "backpressured" ? "Check the destination's ingest bandwidth." : "Check the stream key and destination status.",
        diagnosis: output.diagnosis,
      });
    }
    for (const [key, alert] of current) {
      const existing = alertsOpen.get(key);
      alertsOpen.set(key, {
        ...alert,
        generatedAt: new Date(now).toISOString(),
        firstSeen: existing?.firstSeen ?? new Date(now).toISOString(),
        lastSeen: new Date(now).toISOString(),
      });
    }
    for (const [key, alert] of alertsOpen) {
      if (!current.has(key)) {
        alertsOpen.delete(key);
        resolved.unshift({ alert, resolvedAt: new Date(now).toISOString() });
        if (resolved.length > 500) resolved.pop();
      }
    }
  }

  function fleetSample(now) {
    const wanted = outputs.filter((output) => output.desired === "running");
    const delivering = wanted.filter((output) => output.flow === "flowing" && !output.diagnosis).length;
    const degraded = { ingest: 0, feed: 0, shard: 0, destination: 0 };
    for (const output of wanted) if (output.diagnosis) degraded[output.diagnosis.layer] += 1;
    const capacity = capacitySnapshot();
    return {
      atMs: now,
      headroomPercent: Math.max(0, 100 - capacity.projectedUtilization * 100),
      deliveringPercent: wanted.length ? (100 * delivering) / wanted.length : null,
      degraded,
      // About 4.5 Mb/s per delivering output over the 2 s between samples.
      sentBytes: Math.round((delivering * 4.5e6 * 2) / 8),
    };
  }

  function capacitySnapshot() {
    const running = outputs.filter((output) => output.flow === "flowing");
    const ingressPps = pipelines.filter((pipeline) => pipeline.ingest).length * 900;
    const egressPps = running.length * 450;
    const shardLimit = 250_000 * Math.max(1, size.shardsPerProtocol);
    const hottestShardUtil = Math.min(1.4, storm ? 1.12 : (egressPps / shardLimit) * random.jitter(1, 0.05));
    const nicUtil = Math.min(1, running.reduce((sum, output) => sum + sim.get(sim.get(output.id).stageKey.split(":")[0])?.bitrateBps ?? 0, 0) / (25e9 * Math.max(1, size.cores / 16)));
    const ffmpegStages = [...sim.values()].filter((value) => value.stages).reduce((sum, value) => sum + [...value.stages].filter((stage) => stage !== "source").length, 0);
    const ffmpegUtil = Math.min(1.2, ffmpegStages / (size.cores * 2.5));
    const centers = { egress: hottestShardUtil, nic: nicUtil, ffmpeg: ffmpegUtil, ingress: ingressPps / (100_000 * size.cores), memory: 0.34, disk: 0.08, cpu: ffmpegUtil * 0.8 };
    const hottest = Object.entries(centers).sort((a, b) => b[1] - a[1])[0];
    const utilization = hottest[1];
    const status = utilization >= 1 ? "overloaded" : utilization >= 0.8 ? "saturated" : "healthy";
    return {
      ingressPps,
      mediaBps: running.length * 5_000_000,
      egressPps,
      hottestShardUtil,
      nicUtil,
      memoryUtil: 0.34,
      ffmpegUtil,
      diskUtil: 0.08,
      activeLeaves: running.length,
      uniqueStages: [...sim.values()].reduce((sum, value) => sum + (value.stages ? value.stages.size : 0), 0),
      hottestCenter: hottest[0],
      projectedUtilization: utilization,
      flow: {
        center: hottest[0],
        utilization,
        queue: storm ? 230 : random.int(0, 6),
        backlogSlope: storm ? 14.2 : 0,
        deadlineSlackMs: storm ? -420 : 1800,
        delayMs: storm ? 2400 : random.int(2, 20),
        errors: outputs.filter((output) => output.convergence === "retrying").length,
        amplification: storm ? 3.4 : 1 + outputs.filter((output) => output.convergence === "retrying").length / Math.max(1, running.length),
        status,
      },
      flowMeasured: true,
      observeOnly: true,
    };
  }

  function record(now) {
    for (const [order, pipeline] of pipelines.entries()) {
      const state = sim.get(pipeline.id);
      const mine = outputsByPipeline.get(pipeline.id);
      const wanted = mine.filter((output) => output.desired === "running");
      const input = selectedInput(pipeline.id);
      const inputSim = input ? sim.get(input.id) : null;
      // A rare publisher wobble: bitrate and frame rate dip together.
      const wobble = pipeline.ingest && random.chance(0.03) ? random.jitter(0.45, 0.2) : 1;
      const ingest = pipeline.ingest && input
        ? {
            inputId: input.id,
            protocol: pipeline.ingest.protocol,
            bitrateBps: state.bitrateBps * wobble * random.jitter(1, 0.06),
            packetsPerSec: (state.bitrateBps / 8 / 1300) * wobble,
            videoCodec: "h264",
            width: Math.round((state.height * 16) / 9),
            height: state.height,
            fps: Math.max(1, state.fps * (wobble < 1 ? 0.6 : 1)),
            gopSec: state.gopSec * random.jitter(1, 0.03),
            maxGopSec: state.gopSec * (random.chance(0.05) ? 2 : 1.02),
            rttMs: inputSim ? random.jitter(inputSim.rttMs, 0.15) : null,
            lossPerSec: pipeline.ingest.protocol === "srt" ? (random.chance(0.1) ? random.next() * 6 : 0) : null,
            retransPerSec: pipeline.ingest.protocol === "srt" ? random.next() * 3 : null,
            receiveBuffer: pipeline.ingest.protocol === "srt" ? random.jitter(0.08, 0.5) : null,
            ...(qaOn.has(pipeline.id) && {
              qa: {
                pictureCheckedMs: now,
                blackSinceMs: null,
                stillSinceMs: null,
                soundCheckedMs: now,
                silentSinceMs: null,
                loudnessLufs: -16 + (random.next() - 0.5) * 4 + (wobble < 1 ? -6 : 0),
                // The fixture probe reports two tracks: program and a quieter
                // commentary track.
                tracks: [
                  { silentSinceMs: null, loudnessLufs: -16 + (random.next() - 0.5) * 4 },
                  { silentSinceMs: null, loudnessLufs: -27 + (random.next() - 0.5) * 6 },
                ],
              },
            }),
            // The stream inspector's minute: every fifth pipeline's encoder
            // has audio drifting off and an occasional clock leap.
            timeline: {
              windowSec: 60,
              unread: 0,
              tracks: [
                { media: "video", track: 0, packetsPerSec: state.fps, backwards: 0, jumps: order % 5 === 2 && random.chance(0.5) ? 1 : 0, ptsBeforeDts: 0, ...(order % 5 === 2 && { largestJumpMs: 4_960 }), ...(order % 4 === 1 && { captions: { cea608: true, cea708: order % 8 === 1, lastSeenSec: order % 8 === 5 ? 42 : 0, lastTextSec: 3 } }) },
                { media: "audio", track: 0, packetsPerSec: 46.9, backwards: 0, jumps: 0, ptsBeforeDts: 0, avOffsetMs: Math.round(random.jitter(45, 0.2)), clockDriftMsPerMin: order % 5 === 2 ? Math.round(random.jitter(320, 0.1)) : Math.round((random.next() - 0.5) * 30) },
                { media: "audio", track: 1, packetsPerSec: 46.9, backwards: 0, jumps: 0, ptsBeforeDts: 0, avOffsetMs: Math.round(random.jitter(60, 0.2)), clockDriftMsPerMin: Math.round((random.next() - 0.5) * 30) },
              ],
            },
          }
        : null;
      let offered = 0;
      let delivered = 0;
      for (const output of mine) {
        if (output.actual === null) continue;
        offered += state.bitrateBps;
        delivered += output.flow === "flowing" ? state.bitrateBps * (sim.get(output.id).health === "slow" ? 0.7 : 0.995) : 0;
      }
      const samples = flight.get(pipeline.id) ?? [];
      // Every third pipeline encodes on the host; every ninth struggles.
      const decoders =
        pipeline.ingest && order % 3 === 0
          ? [{ stage: "transcode:h264_720p", lagSlots: order % 9 === 0 ? random.int(200, 400) : random.int(0, 20), lagSec: null, inputBlocked: order % 9 === 0 ? random.jitter(0.35, 0.2) : random.jitter(0.02, 0.5) }].map((decoder) => ({ ...decoder, lagSec: decoder.lagSlots / 120 }))
          : [];
      samples.push({
        atMs: now,
        ingest,
        ...(decoders.length > 0 && { decoders }),
        ring: pipeline.ingest ? { fill: state.ringFill, capacity: state.ringCapacity, worstLagSlots: random.int(0, 60), overflows: state.overflows } : null,
        outputs: {
          wanted: wanted.length,
          delivering: wanted.filter((output) => output.flow === "flowing" && !output.diagnosis).length,
          degraded: wanted.filter((output) => output.diagnosis).length,
          retrying: wanted.filter((output) => output.convergence === "retrying" || output.convergence === "retriesExhausted").length,
          stalled: wanted.filter((output) => output.flow === "stalled").length,
          retryAttempts: wanted.reduce((sum, output) => sum + (output.retry?.attempts ?? 0), 0),
          offeredBps: offered,
          deliveredBps: delivered,
          resyncs: 0,
          worstFeedLag: wanted.some((output) => output.flow === "stalled") ? 3800 : random.int(0, 40),
        },
      });
      if (samples.length > 300) samples.shift();
      flight.set(pipeline.id, samples);
    }
  }

  /** Advances the plant by one sample; returns the state delta. */
  function tick(now = Date.now()) {
    tickCount += 1;
    const changed = { pipelines: new Set(), outputs: new Set(), inputs: new Set(), graphs: new Set() };

    // A scripted storm every ~90 ticks: one shard stalls for 20 ticks.
    if (!storm && tickCount % 90 === 30) {
      storm = { protocol: "rtmp", shard: random.int(0, size.shardsPerProtocol - 1), untilTick: tickCount + 20 };
      emit({ atMs: now, kind: "stage.stalled", pipelineId: "", protocol: "rtmp", phase: `shard ${storm.shard}` });
    } else if (storm && tickCount >= storm.untilTick) {
      emit({ atMs: now, kind: "stage.recovered", pipelineId: "", protocol: storm.protocol, phase: `shard ${storm.shard}` });
      storm = null;
    }
    for (const shard of shards) {
      const stormed = storm && storm.protocol === shard.protocol && storm.shard === shard.shardIndex;
      shard.state = stormed ? "stalled" : "healthy";
      shard.commandDepth = stormed ? Math.min(shard.commandCapacity, 180 + random.int(0, 70)) : random.int(0, 4);
      shard.progressAgeMs = stormed ? 5000 + (tickCount - storm.untilTick + 20) * 2000 : random.int(1, 40);
    }

    // Inputs: rare publisher drops (failover to a backup) and returns.
    for (const pipeline of pipelines) {
      const state = sim.get(pipeline.id);
      const selected = selectedInput(pipeline.id);
      if (state.live && selected?.runtime.connected && random.chance(0.002)) {
        connectInput(selected, false);
        changed.inputs.add(selected.id);
        const backup = inputsByPipeline.get(pipeline.id).find((input) => !input.selected && input.runtime.connected);
        if (backup) {
          selected.selected = false;
          backup.selected = true;
          backup.runtime.forwardingState = "active";
          changed.inputs.add(backup.id);
          emit({ atMs: now, kind: "ingest.failover", pipelineId: pipeline.id, protocol: backup.runtime.protocol });
        } else {
          emit({ atMs: now, kind: "ingest.disconnected", pipelineId: pipeline.id, protocol: state.ingestProtocol });
        }
      } else if (state.live && !selected?.runtime.connected && random.chance(0.08)) {
        connectInput(selected, true);
        changed.inputs.add(selected.id);
        emit({ atMs: now, kind: "ingest.connected", pipelineId: pipeline.id, protocol: state.ingestProtocol });
      }
      const before = `${pipeline.ingest?.inputId}|${pipeline.standbyInputs}`;
      setIngest(pipeline);
      if (`${pipeline.ingest?.inputId}|${pipeline.standbyInputs}` !== before) changed.pipelines.add(pipeline.id);
      state.ringFill = Math.max(0, Math.min(state.ringCapacity, Math.round(random.jitter(pipeline.ingest ? 1400 : 0, 0.25))));
      state.bytesIn += pipeline.ingest ? (state.bitrateBps / 8) * 2 * random.jitter(1, 0.12) : 0;
    }

    // Outputs: rare failures; retries succeed or count up; recoveries.
    for (const output of outputs) {
      const state = sim.get(output.id);
      const before = signature(output);
      if (output.desired === "running") {
        if (state.health === "ok" && random.chance(0.0004)) failOutput(output, now);
        else if (state.health === "retrying" && now >= state.nextRetryMs) {
          if (random.chance(0.45)) {
            state.health = "ok";
            state.attempts = 0;
            emit({ atMs: now, kind: "egress.started", pipelineId: output.pipelineId, outputId: output.id, protocol: output.protocol });
          } else if (state.attempts >= 10) {
            state.health = "exhausted";
          } else {
            state.attempts += 1;
            state.nextRetryMs = now + Math.min(300_000, 5000 * 2 ** Math.min(state.attempts, 16)) * random.jitter(0.75, 0.25);
          }
        } else if (state.health === "slow" && random.chance(0.05)) state.health = "ok";
        else if (state.health === "ok" && random.chance(0.0004)) state.health = "slow";
      }
      settleOutput(output, now);
      const delivering = output.flow === "flowing";
      const bitrate = sim.get(output.pipelineId).bitrateBps;
      state.bytesOut += delivering ? (bitrate / 8) * 2 * (state.health === "slow" ? 0.7 : 1) * random.jitter(1, 0.06) : 0;
      state.uptime = delivering ? state.uptime + 2 : 0;
      if (signature(output) !== before) changed.outputs.add(output.id);
    }

    for (const pipelineId of new Set([...changed.pipelines, ...[...changed.outputs].map((oid) => outputById.get(oid).pipelineId)])) {
      const index = graphIndex.get(pipelineId);
      graphs[index] = buildGraph(pipelines[index]);
      changed.graphs.add(pipelineId);
    }

    stampSince(now);
    deriveAlerts(now);
    fleetSamples.push(fleetSample(now));
    if (fleetSamples.length > 17_280) fleetSamples.shift();
    record(now);

    const any = changed.pipelines.size + changed.outputs.size + changed.inputs.size + changed.graphs.size;
    if (!any) return null;
    revision += 1;
    return {
      epoch,
      revision,
      pipelines: pipelines.filter((pipeline) => changed.pipelines.has(pipeline.id)),
      outputs: outputs.filter((output) => changed.outputs.has(output.id)),
      inputs: inputs.filter((input) => changed.inputs.has(input.id)),
      graphs: graphs.filter((graph) => changed.graphs.has(graph.pipelineId)),
      removedPipelines: [],
      removedOutputs: [],
      removedInputs: [],
      removedGraphs: [],
    };
  }

  function telemetry(now = Date.now()) {
    const ingests = [];
    const telemetryInputs = [];
    for (const pipeline of pipelines) {
      const state = sim.get(pipeline.id);
      if (pipeline.ingest) {
        const input = selectedInput(pipeline.id);
        ingests.push({
          pipelineId: pipeline.id,
          protocol: pipeline.ingest.protocol,
          streamKey: input.streamKey,
          uptimeSecs: Math.round((now - startedMs) / 1000),
          bytesReceived: Math.round(state.bytesIn),
          video: { codec: "h264", width: Math.round((state.height * 16) / 9), height: state.height, fps: state.fps },
          audio: { codec: "aac", channels: 2, sample_rate: 48000 },
          metrics: { packetsIn: Math.round(state.bytesIn / 1300), packetsPerSec: Math.round(state.bitrateBps / 8 / 1300) },
        });
      }
      for (const input of inputs) {
        if (input.pipelineId !== pipeline.id || !input.runtime.connected) continue;
        const inputSim = sim.get(input.id);
        inputSim.bytesIn += (state.bitrateBps / 8) * 2;
        const srt = input.runtime.protocol === "srt";
        telemetryInputs.push({
          inputId: input.id,
          pipelineId: pipeline.id,
          uptimeSecs: Math.round((now - startedMs) / 1000),
          bytesReceived: Math.round(inputSim.bytesIn),
          quality: srt
            ? {
                msRtt: random.jitter(inputSim.rttMs, 0.1),
                mbpsReceiveRate: (state.bitrateBps / 1e6) * random.jitter(1, 0.08),
                packetsReceivedLossPerSec: random.chance(0.1) ? random.next() * 4 : 0,
                packetsReceivedDropPerSec: 0,
                packetsReceivedRetransPerSec: random.next() * 2,
                srtRecvBufPackets: random.int(40, 600),
                srtRecvBufCapacityPackets: 8192,
                mbpsLinkCapacity: random.jitter(80, 0.2),
                srtBonded: state.bonded,
                srtGroupMemberCount: state.bonded ? 2 : null,
                srtGroupConnectedMembers: state.bonded ? random.pick([1, 2, 2, 2]) : null,
              }
            : {
                tcpRttMs: random.jitter(inputSim.rttMs, 0.1),
                tcpReceiveRateMbps: (state.bitrateBps / 1e6) * random.jitter(1, 0.08),
              },
        });
      }
    }
    const stages = [];
    for (const pipeline of pipelines) {
      for (const rendition of sim.get(pipeline.id).stages) {
        stages.push({
          stageKey: `${pipeline.id}:${rendition}`,
          pipelineId: pipeline.id,
          kind: rendition === "source" ? "passthrough" : "transcoder",
          active: !!pipeline.ingest,
          metrics: {
            packetsPerSec: pipeline.ingest ? Math.round(random.jitter(sim.get(pipeline.id).bitrateBps / 8 / 1300, 0.05)) : 0,
            avgUsPerPacket: rendition === "source" ? random.jitter(1.2, 0.2) : random.jitter(38, 0.3),
          },
        });
      }
    }
    const egresses = outputs
      .filter((output) => output.actual !== null)
      .map((output) => {
        const state = sim.get(output.id);
        const offeredBps = sim.get(output.pipelineId).bitrateBps;
        const deliveredBps = output.flow === "flowing" ? offeredBps * (state.health === "slow" ? 0.7 : random.jitter(0.995, 0.004)) : 0;
        return {
          outputId: output.id,
          pipelineId: output.pipelineId,
          protocol: output.protocol,
          targetUrl: `${state.platform.url}/••••`,
          targetAddr: `198.51.100.${(state.shard * 7) % 250}:1935`,
          status: output.actual,
          phase: output.actual,
          uptimeSecs: state.uptime,
          bytesOut: Math.round(state.bytesOut),
          lastProgressAgeMs: output.flow === "flowing" ? random.int(5, 60) : output.flow === "stalled" ? 8000 : null,
          lastError: state.health === "ok" ? null : state.lastError,
          failurePhase: state.health === "ok" ? null : state.failurePhase,
          quality: {
            offeredBps,
            deliveredBps,
            deliveryRatio: offeredBps ? deliveredBps / offeredBps : null,
            ...(output.protocol === "srt" ? { msRtt: random.jitter(state.rttMs, 0.1), srtSendBufPackets: random.int(0, 900) } : {}),
          },
          metrics: { packetsOut: Math.round(state.bytesOut / 1300) },
          resyncCount: state.health === "slow" ? random.int(1, 9) : 0,
          feedLagUnits: output.flow === "stalled" ? 3800 : random.int(0, 40),
          fabricFeed: `fabric-${output.protocol}`,
          shardId: output.shard,
        };
      });
    const sourceRings = pipelines
      .filter((pipeline) => pipeline.ingest)
      .map((pipeline) => {
        const state = sim.get(pipeline.id);
        return {
          pipelineId: pipeline.id,
          fill: state.ringFill,
          capacity: state.ringCapacity,
          payloadStats: { retainedBytes: state.ringFill * 1300 },
          readers: [...state.stages].map((rendition) => ({ name: rendition, lagSlots: random.int(0, 60), overflowCount: state.overflows })),
        };
      });
    return {
      generatedAt: new Date(now).toISOString(),
      ingests,
      inputs: telemetryInputs,
      stages,
      egresses,
      activeTranscoderBuffers: stages.filter((stage) => stage.kind === "transcoder").length * 3,
      memoryAccounting: { sourceRings },
    };
  }

  function systemMetrics(now = Date.now()) {
    const cpu = Math.min(98, 22 + (storm ? 30 : 0) + random.jitter(18, 0.3));
    const up = outputs.filter((output) => output.flow === "flowing").reduce((sum, output) => sum + sim.get(output.pipelineId).bitrateBps, 0) / 1000;
    const down = pipelines.filter((pipeline) => pipeline.ingest).reduce((sum, pipeline) => sum + sim.get(pipeline.id).bitrateBps, 0) / 1000;
    const totalMem = size.cores * 4 * 2 ** 30;
    return {
      generatedAt: new Date(now).toISOString(),
      cpu: { usagePercent: cpu, cores: size.cores, load1: (cpu / 100) * size.cores },
      memory: { usedBytes: totalMem * 0.41, totalBytes: totalMem, usedPercent: 41 },
      engine: {
        cpuPercent: cpu * 0.9,
        cpuSampleReady: true,
        restreamCpuPercent: cpu * 0.55,
        externalFfmpegCpuPercent: cpu * 0.35,
        memoryBytes: totalMem * 0.3,
        restreamMemoryBytes: totalMem * 0.12,
        totalMemoryBytes: totalMem,
        externalFfmpegCount: [...sim.values()].reduce((sum, value) => sum + (value.stages ? [...value.stages].filter((stage) => stage !== "source").length : 0), 0),
        externalFfmpegMemoryBytes: totalMem * 0.18,
      },
      disk: { usedPercent: 37, totalBytes: 2e12, usedBytes: 7.4e11, freeBytes: 1.26e12, scope: "data", mountPoint: "/", root: "/var/lib/restream" },
      mediaDisk: { usedPercent: 62, totalBytes: 4e12, usedBytes: 2.48e12, freeBytes: 1.52e12, scope: "media", mountPoint: "/media", mediaDir: "/media/restream" },
      network: {
        scope: "external",
        downloadKbps: down,
        uploadKbps: up,
        interfaces: [
          { name: "bond0", downloadKbps: down * 0.98, uploadKbps: up * 0.97 },
          { name: "eth2", downloadKbps: down * 0.02, uploadKbps: up * 0.03 },
        ],
        ignoredInterfaces: ["lo", "docker0"],
        sampleMs: 2000,
      },
      capacity: capacitySnapshot(),
      mediaExecutor: { configuredWorkers: Math.max(2, size.cores / 4), workers: [] },
      ioUring: { available: true, deploymentTier: "full" },
      rtmps: { ktlsCapabilities: { tx: true, rx: false } },
      egressShards: structuredClone(shards),
    };
  }

  function alerts({ since } = {}) {
    const now = Date.now();
    const list = [...alertsOpen.values()].sort((a, b) =>
      a.severity === b.severity ? a.firstSeen.localeCompare(b.firstSeen) : a.severity === "critical" ? -1 : 1,
    );
    return {
      generatedAt: new Date(now).toISOString(),
      alerts: list,
      ...(since
        ? { resolved: resolved.filter((item) => item.resolvedAt >= since), resolvedCompleteFrom: new Date(startedMs).toISOString() }
        : {}),
    };
  }

  function lifecycleEvents({ pipelineId, limit = 200 } = {}) {
    const list = events.filter((event) => !pipelineId || event.pipelineId === pipelineId).slice(-limit).reverse();
    return { generatedAt: new Date().toISOString(), count: list.length, events: list };
  }

  function fleetHistory({ since, until } = {}) {
    const from = since ? Date.parse(since) : 0;
    const to = until ? Date.parse(until) : Infinity;
    const samples = fleetSamples.filter((sample) => sample.atMs >= from && sample.atMs <= to);
    return { samples, completeFromMs: startedMs, intervalMs: 5000, newestMs: fleetSamples.at(-1)?.atMs ?? null };
  }

  function health() {
    return {
      status: "ok",
      hostSettings: [
        { key: "nofile", label: "Open files", current: 1_048_576, required: 65_536, status: "ok", detail: "soft limit" },
        { key: "rmem_max", label: "UDP receive buffer", current: 26_214_400, required: 26_214_400, unit: "bytes", status: "ok" },
        { key: "wmem_max", label: "UDP send buffer", current: 212_992, required: 26_214_400, unit: "bytes", status: "warning", detail: "SRT egress may drop under burst" },
      ],
      rtmpListener: { acceptErrors: 0, fdExhaustionErrors: 0, restarts: 0, ingressOwners: Math.max(1, size.cores / 8) },
      srtListener: {
        bondingAvailable: true,
        restarts: 0,
        ingressOwner: {
          faulted: false, owners: Math.max(1, size.cores / 8), peers: inputs.filter((input) => input.runtime.protocol === "srt").length,
          rxRingDepth: 12, rxRingDropped: 0, rxBufferExhaustions: 0, txInFlight: 40, txCapacity: 4096, budgetExhausted: 0,
          eventBridgeFullVisits: 0, telemetryDropped: 0, overloadDisconnects: 0, mediaSlowPasses5ms: 3, mediaSlowPasses20ms: 0, mediaPassMaxUs: 4100,
        },
      },
      egressFabricShards: structuredClone(shards),
      containedPanics: 0,
    };
  }

  function engine() {
    return {
      restream: { version: "0.42.0-fixture", commit: "fixture", buildTimestamp: new Date(startedMs).toISOString() },
      toolchain: { rustc: "1.93.0", target: "x86_64-unknown-linux-gnu" },
      nativeLibraries: { srt: { version: "srt-rs", license: "Apache-2.0" }, sqlite: { version: "3.50.4", license: "blessing" } },
      externalTools: {
        ffmpeg: { path: "/usr/local/bin/ffmpeg", version: "n8.1.3-9-g29e619e767-20260930", error: null },
        ffprobe: { path: "/usr/local/bin/ffprobe", version: "n8.1.3-9-g29e619e767-20260930", error: null },
        features: ["transcoding", "fileIngest", "recordingRemux", "posters", "pictureAndSoundChecks", "mediaAnalysis"].map((feature) => ({ feature, available: true, missing: [] })),
      },
      sbom: { endpoint: "/api/v1/engine/sbom", componentCount: 612, licensesIncluded: true },
      os: { hostname: `restream-${preset}`, kernelVersion: "6.18.44", cpu: { modelName: "Synthetic EPYC", logicalCpus: size.cores } },
    };
  }

  function settings() {
    return {
      serverName: `Restream ${preset}`,
      ingestHost: "ingest.restream.local",
      dashboardPasswordChangeRecommended: false,
      ingestSecurity: { failureLimit: 10, failureWindowMs: 60_000, banMs: 600_000, trackedIpLimit: 10_000 },
      recordingSettings: { retainSourceTs: false },
      srtIngest: { mode: "plaintext", pbkeylen: 16, latencyMs: 120 },
      transcodeProfiles: {
        h264_1080p: { preset: "veryfast", tune: "zerolatency", crf: 0, gop: 60, bframes: 0, bitrate: 6000, maxBitrate: 6500, width: 1920, height: 1080 },
        h264_720p: { preset: "veryfast", tune: "zerolatency", crf: 0, gop: 60, bframes: 0, bitrate: 3500, maxBitrate: 4000, width: 1280, height: 720 },
        h264_480p: { preset: "veryfast", tune: "zerolatency", crf: 0, gop: 60, bframes: 0, bitrate: 1500, maxBitrate: 1800, width: 854, height: 480 },
      },
      pipelines: pipelines.map((pipeline) => ({
        id: pipeline.id,
        name: pipeline.name,
        streamKey: `sk-${pipeline.id}-0`,
        inputSource: null,
        srtIngestPolicy: null,
        fileIngest: { configured: false, running: false },
      })),
      outputs: outputs.map((output) => ({
        id: output.id,
        pipelineId: output.pipelineId,
        name: output.name,
        url: `${sim.get(output.id).platform.url}/live-key-${output.id}`,
        monitoringUrl: null,
        config: { video: { mode: "source" }, audio: { mode: "all" } },
        desiredState: output.desired,
      })),
      jobs: [],
    };
  }

  function pipelineInputs(pipelineId) {
    const list = inputs.filter((input) => input.pipelineId === pipelineId);
    const tel = telemetry();
    return {
      inputs: list.map((input) => {
        const t = tel.inputs.find((candidate) => candidate.inputId === input.id);
        return {
          ...structuredClone(input),
          runtime: {
            ...input.runtime,
            uptimeSeconds: t?.uptimeSecs ?? null,
            bytesReceived: t?.bytesReceived ?? 0,
            video: input.runtime.video ? { ...input.runtime.video, fps: sim.get(pipelineId).fps } : null,
            audio: input.runtime.audio ? { codec: "aac", channels: 2, sample_rate: 48000 } : null,
            quality: t?.quality ?? null,
          },
        };
      }),
      selectedInputId: list.find((input) => input.selected)?.id ?? null,
    };
  }

  function resourceMap({ pipelineId, topN = 200 } = {}) {
    const scoped = pipelineId ? pipelines.filter((pipeline) => pipeline.id === pipelineId) : pipelines;
    const nodes = [];
    for (const pipeline of scoped) {
      const state = sim.get(pipeline.id);
      nodes.push({ id: `${pipeline.id}:ring`, kind: "source_ring", label: `${pipeline.name} ring`, pipelineId: pipeline.id, execution: "media", memory: { attributedBytes: state.ringFill * 1300, confidence: "measured" }, status: pipeline.ingest ? "active" : "idle" });
      for (const rendition of state.stages) {
        const transcode = rendition !== "source";
        nodes.push({
          id: `${pipeline.id}:${rendition}`,
          kind: transcode ? "transcoder" : "passthrough",
          label: `${pipeline.name} · ${rendition}`,
          pipelineId: pipeline.id,
          execution: transcode ? "ffmpeg" : "media",
          cpuPercent: transcode ? random.jitter(140, 0.3) : random.jitter(2, 0.5),
          memory: { attributedBytes: transcode ? 180e6 : 4e6, confidence: transcode ? "measured" : "derived" },
          backendPid: transcode ? 10_000 + nodes.length : undefined,
          metrics: { packetsPerSec: Math.round(state.bitrateBps / 8 / 1300), avgUsPerPacket: transcode ? 38 : 1.2 },
          judgement: transcode ? { verdict: random.chance(0.1) ? "pressured" : "clear", center: "ffmpeg", utilization: random.next(), measured: false, reason: "share of the FFmpeg stage budget" } : undefined,
        });
      }
    }
    for (const shard of shards) {
      nodes.push({
        id: `shard:${shard.protocol}:${shard.shardIndex}`,
        kind: "egress_shard",
        label: `${shard.protocol.toUpperCase()} shard ${shard.shardIndex}`,
        execution: "egress",
        state: shard.state,
        protocol: shard.protocol,
        shardIndex: shard.shardIndex,
        feedId: shard.feedId,
        cpuPercent: shard.state === "stalled" ? 99 : random.jitter(30, 0.4),
        metrics: { commandDepth: shard.commandDepth, commandCapacity: shard.commandCapacity, progressAgeMs: shard.progressAgeMs },
        judgement: { verdict: shard.state === "stalled" ? "limiting" : "clear", center: "egress", utilization: shard.commandDepth / shard.commandCapacity, measured: true, reason: "command queue against its bound" },
      });
    }
    const sorted = nodes.sort((a, b) => (b.judgement?.utilization ?? 0) - (a.judgement?.utilization ?? 0));
    const returned = sorted.slice(0, topN);
    return {
      generatedAt: new Date().toISOString(),
      scope: pipelineId ? { kind: "pipeline", pipelineId } : { kind: "runtime" },
      view: "detail",
      limits: { topN, totalNodeCount: nodes.length, returnedNodeCount: returned.length, truncatedNodeCount: nodes.length - returned.length, maxTopN: 500 },
      summary: { pipelines: scoped.length, nodes: nodes.length },
      nodes: returned,
      edges: [],
      attribution: { measured: ["cpuPercent"], derived: ["memory"] },
    };
  }

  function capacityScenario(query) {
    const baseline = capacitySnapshot();
    const add = Number(query.get("addRtmp") ?? 0) + Number(query.get("addRtmps") ?? 0) + 2 * Number(query.get("addSrt") ?? 0);
    const projected = { ...baseline, activeLeaves: baseline.activeLeaves + add, hottestShardUtil: baseline.hottestShardUtil * (1 + add / Math.max(1, baseline.activeLeaves)) };
    projected.projectedUtilization = Math.max(projected.hottestShardUtil, baseline.projectedUtilization);
    return {
      addOutputs: { rtmp: Number(query.get("addRtmp") ?? 0), rtmps: Number(query.get("addRtmps") ?? 0), srt: Number(query.get("addSrt") ?? 0) },
      addStages: Number(query.get("addStages") ?? 0),
      ratedBy: query.get("pipelineId") ?? pipelines[0]?.id ?? null,
      ingest: { bps: 6_000_000, pps: 900 },
      calibration: {
        modelVersion: 5,
        limits: { ingressPps: 100_000 * size.cores, egressPps: 250_000 * size.shardsPerProtocol, nicBps: 10e9, memoryBytes: 1e9 * size.cores, ffmpegStages: size.cores, diskBps: 5e8 },
        sources: { ingressPps: "prior", egressPps: "prior", nicBps: "configured", memoryBytes: "prior", ffmpegStages: "prior", diskBps: "prior" },
        priorCpus: size.cores,
        safeUtilization: 0.8,
      },
      retryPolicy: { maxRetries: 10, baseMs: 5000, maxMs: 300000, jitter: "equal" },
      baseline,
      projected,
    };
  }

  function mediaFiles() {
    const files = [];
    for (let index = 0; index < Math.min(400, size.pipelines * 2); index += 1) {
      const recording = index % 3 === 0;
      const pipeline = pipelines[index % pipelines.length];
      files.push({
        name: recording ? `rec-${pipeline.id}-${index}.mp4` : `${random.pick(SHOW_WORDS).toLowerCase()}-${index}.mp4`,
        size: random.int(50, 9000) * 1e6,
        modifiedAt: new Date(startedMs - index * 3_600_000).toISOString(),
        kind: recording ? "recording" : "source",
        recordingId: recording ? `rec_${index}` : null,
        pipelineId: recording ? pipeline.id : null,
        recordingStatus: recording ? "ready" : null,
        conversionStatus: recording ? "ready" : null,
      });
    }
    return { files };
  }

  function logs() {
    return {
      logs: events.slice(-200).reverse().map((event) => ({
        id: event.seq,
        ts: event.timestamp,
        level: event.kind.endsWith("failed") || event.kind.endsWith("stalled") ? "warn" : "info",
        target: "restream::events",
        message: `${event.kind}${event.error ? `: ${event.error}` : ""}`,
        pipelineId: event.pipelineId || null,
        outputId: event.outputId ?? null,
        eventType: event.kind,
        eventClass: "lifecycle",
      })),
      hasMore: false,
    };
  }

  function pipelineSummary(pipelineId) {
    const pipeline = pipelineById.get(pipelineId);
    if (!pipeline) return null;
    const mine = outputsByPipeline.get(pipelineId);
    return {
      generatedAt: new Date().toISOString(),
      pipelineId,
      source: { status: pipeline.ingest ? "live" : "offline", bitrateKbps: sim.get(pipelineId).bitrateBps / 1000, protocol: pipeline.ingest?.protocol ?? null, readers: sim.get(pipelineId).stages.size },
      outputs: { total: mine.length, running: mine.filter((output) => output.flow === "flowing").length },
      recording: { enabled: pipeline.recording, active: pipeline.recording && !!pipeline.ingest },
      graph: { nodes: graphs[graphIndex.get(pipelineId)].nodes.length, hasGraph: true },
      alerts: [...alertsOpen.values()].filter((alert) => alert.pipelineId === pipelineId),
    };
  }

  /** A stable 0..1 number for a pipeline and a time bucket. */
  function noise(pipelineId, bucket) {
    let hash = 2166136261;
    for (const char of `${pipelineId}:${bucket}`) hash = Math.imul(hash ^ char.charCodeAt(0), 16777619);
    return ((hash >>> 0) % 10_000) / 10_000;
  }

  /** The persisted history before the live samples: one sample per 10 s for
   * a day, shaped like the newest live one, with an outage in about one
   * ten-minute stretch in twelve (a dip, retries, then recovery). */
  function persistedHistory(pipelineId, fromMs, toMs) {
    const live = flight.get(pipelineId) ?? [];
    const shape = live.at(-1);
    if (!shape) return [];
    const out = [];
    const start = Math.max(fromMs, startedMs - 24 * 3_600_000);
    for (let at = Math.ceil(start / 10_000) * 10_000; at < toMs; at += 10_000) {
      const bucket = Math.floor(at / 600_000);
      const hit = noise(pipelineId, bucket) < 0.08;
      const phase = (at % 600_000) / 600_000;
      const depth = hit ? Math.max(0, Math.sin(Math.PI * Math.min(1, phase * 2.5))) * (0.3 + noise(pipelineId, bucket + 7) * 0.7) : 0;
      const wanted = shape.outputs.wanted;
      const down = Math.round(wanted * depth);
      out.push({
        ...shape,
        atMs: at,
        outputs: {
          ...shape.outputs,
          delivering: Math.max(0, wanted - down),
          degraded: down,
          retrying: Math.round(down * 0.7),
          stalled: Math.round(down * 0.2),
          retryAttempts: Math.round(down * (1 + depth * 6)),
          deliveredBps: shape.outputs.offeredBps * (1 - depth),
        },
      });
    }
    return out;
  }

  /** At most `max` samples, evenly spread, the ends kept. */
  function thin(samples, max) {
    if (samples.length <= max || max < 2) return samples;
    return Array.from({ length: max }, (_, i) => samples[Math.round((i * (samples.length - 1)) / (max - 1))]);
  }

  function flightRecorder(pipelineId, { since = 0, until = Number.MAX_SAFE_INTEGER, points = 2000 } = {}) {
    const live = flight.get(pipelineId) ?? [];
    const detailedFromMs = live[0]?.atMs ?? startedMs;
    const history = since < detailedFromMs ? persistedHistory(pipelineId, since, Math.min(until, detailedFromMs)) : [];
    const recent = live.filter((sample) => sample.atMs >= since && sample.atMs < until);
    const samples = thin([...history, ...recent], Math.max(2, Math.min(points, 2000)));
    return {
      schema: "restream.trace.v1",
      pipelineId,
      intervalMs: 2000,
      completeFromMs: live.length ? startedMs - 24 * 3_600_000 : startedMs,
      detailedFromMs,
      persistedIntervalMs: 10_000,
      samples,
    };
  }

  /** Applies an operator mutation the fixture understands. */
  function setDesired(outputId, desired) {
    const output = outputById.get(outputId);
    if (!output) return false;
    output.desired = desired;
    const state = sim.get(output.id);
    if (desired === "running") {
      state.health = "ok";
      state.attempts = 0;
    }
    return true;
  }

  // Warm history so charts start populated: 15 minutes at 2 s.
  for (let back = 450; back > 0; back -= 1) tick(nowMs - back * 2000);

  stampSince(startedMs);

  return {
    preset,
    size,
    state,
    tick,
    telemetry,
    systemMetrics,
    alerts,
    lifecycleEvents,
    fleetHistory,
    health,
    engine,
    settings,
    pipelineInputs,
    resourceMap,
    capacityScenario,
    mediaFiles,
    logs,
    flightRecorder,
    pipelineSummary,
    streamQa(pipelineId) {
      const enabled = qaOn.has(pipelineId);
      const pipeline = pipelines.find((candidate) => candidate.id === pipelineId);
      const newest = (flight.get(pipelineId) ?? []).at(-1);
      return { enabled, active: enabled && Boolean(pipeline?.ingest), findings: enabled ? (newest?.ingest?.qa ?? null) : null };
    },
    setStreamQa(pipelineId, enabled) {
      if (enabled) qaOn.add(pipelineId);
      else qaOn.delete(pipelineId);
    },
    setDesired,
    counts: () => ({ pipelines: pipelines.length, inputs: inputs.length, outputs: outputs.length }),
  };
}
