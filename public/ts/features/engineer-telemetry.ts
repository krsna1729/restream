import {
  getEngineTelemetry,
  getPipelineTelemetry,
  getStageTelemetry,
} from "../core/api.js";
import { escapeHtml } from "../core/utils.js";
import type {
  EngineTelemetrySnapshot,
  PipelineTelemetrySnapshot,
  StageTelemetrySnapshot,
  TelemetryMetrics,
  TelemetryStage,
} from "../core/api.js";

export interface TelemetryPipelineOption {
  id: string;
  name: string;
}

interface TelemetryViewOptions {
  active: boolean;
  pipelines: TelemetryPipelineOption[];
}

const TELEMETRY_REFRESH_MS = 5_000;
let selectedPipelineId = "";
let selectedStageKey = "";
let lastFetchedAt = 0;
let requestSequence = 0;
let stageRequestSequence = 0;
const inFlightByPipeline = new Map<
  string,
  { sequence: number; promise: Promise<void> }
>();
let engineSnapshot: EngineTelemetrySnapshot | null = null;
let pipelineSnapshot: PipelineTelemetrySnapshot | null = null;
let stageSnapshot: StageTelemetrySnapshot | null = null;
let telemetryLoaded = false;
let telemetryUnavailable = false;
let stageUnavailable = false;
let viewOptions: TelemetryViewOptions | null = null;

function formatNumber(value: unknown): string {
  return typeof value === "number" && Number.isFinite(value)
    ? value.toLocaleString()
    : "—";
}

function formatBytes(value: unknown): string {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  const units = ["B", "KiB", "MiB", "GiB"];
  let amount = Math.max(0, value);
  let unit = 0;
  while (amount >= 1024 && unit < units.length - 1) {
    amount /= 1024;
    unit += 1;
  }
  return `${amount >= 10 || unit === 0 ? amount.toFixed(0) : amount.toFixed(1)} ${units[unit]}`;
}

function metricRows(metrics: TelemetryMetrics | undefined): string {
  const entries = Object.entries(metrics || {}).sort(([left], [right]) =>
    left.localeCompare(right),
  );
  if (!entries.length)
    return `<p class="text-base-content/60 text-sm">No counters reported.</p>`;
  return `<dl class="grid grid-cols-[minmax(0,1fr)_auto] gap-x-4 gap-y-1 text-xs">${entries
    .map(
      ([key, value]) =>
        `<dt class="truncate text-base-content/60">${escapeHtml(key)}</dt><dd class="font-mono">${escapeHtml(String(value ?? "—"))}</dd>`,
    )
    .join("")}</dl>`;
}

function stageKey(pipelineId: string, stage: TelemetryStage): string {
  return stage.stageKey || `${pipelineId}:${stage.kind}`;
}

function renderStage(pipelineId: string, stage: TelemetryStage): string {
  const key = stageKey(pipelineId, stage);
  return `<article class="border-base-content/10 bg-base-100 rounded-lg border p-3">
    <div class="flex items-center justify-between gap-2"><div><h3 class="text-sm font-semibold">${escapeHtml(stage.kind)}</h3><p class="text-base-content/50 text-xs">${stage.active === false ? "inactive" : "active"}</p></div><button class="btn btn-xs btn-outline" type="button" aria-label="View ${escapeHtml(stage.kind)} telemetry details" data-stage-telemetry-key="${escapeHtml(key)}">Details</button></div>
    <div class="mt-3">${metricRows(stage.metrics)}</div>
  </article>`;
}

export function renderEngineerTelemetryHtml(
  engine: EngineTelemetrySnapshot | null,
  pipeline: PipelineTelemetrySnapshot | null,
  stage: StageTelemetrySnapshot | null,
  pipelines: TelemetryPipelineOption[],
  pipelineId: string,
  status: {
    loaded?: boolean;
    unavailable?: boolean;
    stageUnavailable?: boolean;
  } = {},
): string {
  const options = pipelines
    .map(
      (item) =>
        `<option value="${escapeHtml(item.id)}"${item.id === pipelineId ? " selected" : ""}>${escapeHtml(item.name || item.id)}</option>`,
    )
    .join("");
  const ring = pipeline?.sourceRing;
  const readers = ring?.readers || [];
  const stages = pipeline?.stages || [];
  const egresses = pipeline?.egresses || [];
  const availability = !status.loaded
    ? `<div class="alert"><span>Loading telemetry snapshots…</span></div>`
    : status.unavailable
      ? `<div class="alert alert-warning"><span>Telemetry is temporarily unavailable. Last known counters remain visible.</span></div>`
      : "";
  return `<div class="mx-auto max-w-7xl space-y-4">
    <header class="flex flex-wrap items-end justify-between gap-3"><div><h1 class="text-lg font-semibold">Engineer telemetry</h1><p class="text-base-content/60 mt-1 text-sm">Point-in-time engine, ring, reader, stage, and egress counters.</p></div><div class="flex items-center gap-2"><select id="telemetry-pipeline-select" class="select select-sm" aria-label="Telemetry pipeline">${options || `<option value="">No pipelines</option>`}</select><button id="telemetry-refresh-btn" type="button" class="btn btn-sm btn-outline">Refresh</button></div></header>
    ${availability}
    <section class="grid gap-3 sm:grid-cols-2 lg:grid-cols-4" aria-label="Engine telemetry summary">
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Active ingests</div><div class="stat-value text-2xl">${engine?.ingests.length ?? "—"}</div></div>
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Stages</div><div class="stat-value text-2xl">${engine?.stages.length ?? "—"}</div></div>
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Egresses</div><div class="stat-value text-2xl">${engine?.egresses.length ?? "—"}</div></div>
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Transcoder buffers</div><div class="stat-value text-2xl">${engine?.activeTranscoderBuffers ?? "—"}</div></div>
    </section>
    ${
      pipelineId && pipeline
        ? `<div class="grid gap-4 xl:grid-cols-[minmax(20rem,.75fr)_minmax(0,1.25fr)]">
      <div class="space-y-4">
        <section class="border-base-content/10 bg-base-200 rounded-lg border p-4"><h2 class="font-semibold">Source ring</h2>${ring ? `<div class="mt-3 grid grid-cols-2 gap-3 text-sm"><div><span class="text-base-content/60">Fill</span><div class="font-mono">${formatNumber(ring.fill)} / ${formatNumber(ring.capacity)} (${formatNumber(ring.fillPercent)}%)</div></div><div><span class="text-base-content/60">Depth</span><div class="font-mono">${formatNumber(ring.bufferDepthSecs)} s</div></div></div>` : `<p class="text-base-content/60 mt-3 text-sm">No active source ring.</p>`}</section>
        <section class="border-base-content/10 bg-base-200 rounded-lg border p-4"><h2 class="font-semibold">Readers</h2><div class="mt-3 space-y-2">${readers.length ? readers.map((reader) => `<div class="bg-base-100 rounded-md p-3 text-sm"><div class="font-medium">${escapeHtml(reader.name)}</div><div class="text-base-content/60 mt-1 text-xs">Lag ${formatNumber(reader.lagSlots)} slots · ${formatNumber(reader.overflowCount)} overflows · packet age ${formatNumber(reader.packetAgeMs)} ms</div></div>`).join("") : `<p class="text-base-content/60 text-sm">No active readers.</p>`}</div></section>
        <section class="border-base-content/10 bg-base-200 rounded-lg border p-4"><h2 class="font-semibold">Egresses</h2><div class="mt-3 space-y-2">${egresses.length ? egresses.map((egress) => `<div class="bg-base-100 rounded-md p-3 text-sm"><div class="flex justify-between gap-2"><span class="font-medium">${escapeHtml(egress.outputId)}</span><span class="badge badge-sm">${escapeHtml(egress.status || egress.phase || "unknown")}</span></div><div class="text-base-content/60 mt-1 text-xs">${formatBytes(egress.bytesOut)} sent${egress.lastError ? ` · ${escapeHtml(egress.lastError)}` : ""}</div></div>`).join("") : `<p class="text-base-content/60 text-sm">No active egresses.</p>`}</div></section>
      </div>
      <div class="space-y-4"><section><h2 class="mb-3 font-semibold">Processing stages</h2><div class="grid gap-3 md:grid-cols-2">${stages.length ? stages.map((item) => renderStage(pipelineId, item)).join("") : `<div class="border-base-content/10 bg-base-200 rounded-lg border p-6 text-center text-sm">No active stages.</div>`}</div></section>
      <section id="stage-telemetry-detail" class="border-base-content/10 bg-base-200 rounded-lg border p-4"><h2 class="font-semibold">Stage detail</h2>${status.stageUnavailable ? `<p class="text-warning mt-2 text-sm">Fresh stage detail is unavailable; the stage may have stopped. Last known counters remain visible when available.</p>` : ""}${stage ? `<div class="mt-1 text-xs text-base-content/60">${escapeHtml(stage.stageKey)}</div><div class="mt-3 grid gap-4 md:grid-cols-2"><div><h3 class="mb-2 text-sm font-medium">Throughput</h3>${metricRows(stage.metrics)}</div><div><h3 class="mb-2 text-sm font-medium">Pipe</h3>${metricRows(stage.pipeMetrics)}</div></div>` : status.stageUnavailable ? "" : `<p class="text-base-content/60 mt-3 text-sm">Select a stage to fetch its current detail.</p>`}</section></div>
    </div>`
        : pipelineId
          ? `<div class="border-base-content/10 bg-base-200 rounded-lg border p-8 text-center text-sm">${status.loaded ? "Pipeline telemetry is unavailable." : "Loading pipeline telemetry…"}</div>`
          : `<div class="border-base-content/10 bg-base-200 rounded-lg border p-8 text-center text-sm">Select or create a pipeline to inspect telemetry.</div>`
    }
  </div>`;
}

function paintTelemetry(): void {
  const root = document.getElementById("telemetry-mode-content");
  if (!root || !viewOptions) return;
  root.innerHTML = renderEngineerTelemetryHtml(
    engineSnapshot,
    pipelineSnapshot,
    stageSnapshot,
    viewOptions.pipelines,
    selectedPipelineId,
    {
      loaded: telemetryLoaded,
      unavailable: telemetryUnavailable,
      stageUnavailable,
    },
  );
  const select = document.getElementById(
    "telemetry-pipeline-select",
  ) as HTMLSelectElement | null;
  select?.addEventListener("change", () => {
    selectTelemetryPipeline(select.value);
  });
  document
    .getElementById("telemetry-refresh-btn")
    ?.addEventListener("click", () => void refreshEngineerTelemetry(true));
  document
    .querySelectorAll<HTMLElement>("[data-stage-telemetry-key]")
    .forEach((button) => {
      button.addEventListener("click", () => {
        const key = button.dataset.stageTelemetryKey;
        if (key) void fetchStageDetail(key);
      });
    });
}

export async function fetchStageDetail(stageKeyValue: string): Promise<void> {
  if (!viewOptions?.active || document.hidden) return;
  const sequence = ++stageRequestSequence;
  const pipelineAtRequest = selectedPipelineId;
  selectedStageKey = stageKeyValue;
  stageUnavailable = false;
  const result = await getStageTelemetry(stageKeyValue);
  if (
    sequence !== stageRequestSequence ||
    pipelineAtRequest !== selectedPipelineId ||
    selectedStageKey !== stageKeyValue
  )
    return;
  // A stage may disappear between the pipeline snapshot and this detail fetch.
  if (result) stageSnapshot = result;
  stageUnavailable = result === null;
  paintTelemetry();
}

export async function refreshEngineerTelemetry(force = false): Promise<void> {
  if (!viewOptions?.active || document.hidden) return;
  if (!force && Date.now() - lastFetchedAt < TELEMETRY_REFRESH_MS) return;
  const scope = selectedPipelineId;
  const existing = inFlightByPipeline.get(scope);
  if (existing?.sequence === requestSequence) return existing.promise;
  const sequence = ++requestSequence;
  const pipelineAtRequest = selectedPipelineId;
  const stageAtRequest = selectedStageKey;
  const request = (async () => {
    const [engine, pipeline, stage] = await Promise.all([
      getEngineTelemetry(),
      pipelineAtRequest
        ? getPipelineTelemetry(pipelineAtRequest)
        : Promise.resolve(null),
      stageAtRequest
        ? getStageTelemetry(stageAtRequest)
        : Promise.resolve(undefined),
    ]);
    if (
      sequence !== requestSequence ||
      pipelineAtRequest !== selectedPipelineId
    )
      return;
    engineSnapshot = engine ?? engineSnapshot;
    if (pipelineAtRequest) pipelineSnapshot = pipeline ?? pipelineSnapshot;
    telemetryLoaded = true;
    telemetryUnavailable =
      engine === null || (Boolean(pipelineAtRequest) && pipeline === null);
    if (stageAtRequest && stageAtRequest === selectedStageKey) {
      if (stage) stageSnapshot = stage;
      stageUnavailable = stage === null;
    }
    lastFetchedAt = Date.now();
    paintTelemetry();
  })().finally(() => {
    if (inFlightByPipeline.get(scope)?.promise === request) {
      inFlightByPipeline.delete(scope);
    }
  });
  inFlightByPipeline.set(scope, { sequence, promise: request });
  return request;
}

export function selectTelemetryPipeline(pipelineId: string): void {
  if (pipelineId === selectedPipelineId) return;
  selectedPipelineId = pipelineId;
  selectedStageKey = "";
  stageSnapshot = null;
  stageUnavailable = false;
  pipelineSnapshot = null;
  telemetryLoaded = false;
  paintTelemetry();
  void refreshEngineerTelemetry(true);
}

export function renderEngineerTelemetryMode(
  options: TelemetryViewOptions,
): void {
  viewOptions = options;
  if (
    !options.pipelines.some((pipeline) => pipeline.id === selectedPipelineId)
  ) {
    selectTelemetryPipeline(options.pipelines[0]?.id || "");
  }
  if (!options.active) return;
  paintTelemetry();
  void refreshEngineerTelemetry();
}
