import {
  getEngineTelemetry,
  getHealth,
  getPipelineTelemetry,
  getStageTelemetry,
} from "../core/api.js";
import { escapeHtml } from "../core/utils.js";
import { RenderScope } from "../core/render-scope.js";
import type { RenderScopeToken } from "../core/render-scope.js";
import type { HealthData, HostSettingRow } from "../types.js";
import type {
  EngineTelemetrySnapshot,
  PipelineTelemetrySnapshot,
  StageTelemetrySnapshot,
  TelemetryMetrics,
  TelemetryStage,
} from "../core/api.js";
import type { TelemetryCheckpointModel } from "./telemetry-view-model.js";

export interface TelemetryPipelineOption {
  id: string;
  name: string;
}

interface TelemetryViewOptions {
  active: boolean;
  containerId?: string;
  pipelines: TelemetryPipelineOption[];
}

const TELEMETRY_REFRESH_MS = 5_000;
const TELEMETRY_EGRESS_CARD_LIMIT = 8;
const TELEMETRY_STAGE_CARD_LIMIT = 8;
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
let healthSnapshot: HealthData | null = null;
let telemetryLoaded = false;
let telemetryUnavailable = false;
let stageUnavailable = false;
let viewOptions: TelemetryViewOptions | null = null;
const telemetryScope = new RenderScope("telemetry-mode-content");
let telemetrySearchQuery = "";
let telemetryEgressExpanded = false;
let telemetryStagesExpanded = false;
let telemetryHostSettingsExpanded = false;
let telemetryCheckpointCallback:
  | ((model: TelemetryCheckpointModel | null) => void)
  | null = null;
let telemetryV2PresentationActive = false;

export function configureTelemetryCheckpointPresentation(options: {
  readonly onPresentation?: (model: TelemetryCheckpointModel | null) => void;
  readonly v2Active?: boolean;
}): void {
  telemetryV2PresentationActive = options.v2Active === true;
  telemetryCheckpointCallback = options.onPresentation ?? null;
  if (!telemetryCheckpointCallback || !viewOptions?.active) {
    telemetryCheckpointCallback?.(null);
    return;
  }
  telemetryCheckpointCallback(buildTelemetryCheckpointModel());
}

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

function formatHostSettingValue(value: unknown, unit: unknown): string {
  if (typeof value === "string") return value;
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  if (unit === "bytes") return formatBytes(value);
  return value.toLocaleString();
}

function hostSettingTone(status: unknown): string {
  if (status === "ok") return "badge-success";
  if (status === "warning") return "badge-warning";
  return "badge-ghost";
}

function renderHostSettings(settings: HostSettingRow[] | undefined): string {
  const rows = settings || [];
  if (!rows.length) {
    return `<p class="text-base-content/60 mt-3 text-sm">No host settings reported.</p>`;
  }
  return `<div class="mt-3 overflow-x-auto" role="region" aria-label="Host settings table" tabindex="0">
    <table class="table table-sm">
      <thead><tr><th>Setting</th><th>Current</th><th>Required</th><th>Status</th><th>Info</th></tr></thead>
      <tbody>${rows
        .map(
          (row) => `<tr>
            <td><div class="font-medium">${escapeHtml(row.label || row.key)}</div><div class="text-base-content/50 text-xs">${escapeHtml(row.key)}</div></td>
            <td class="font-mono">${escapeHtml(formatHostSettingValue(row.current, row.unit))}</td>
            <td class="font-mono">${escapeHtml(formatHostSettingValue(row.required, row.unit))}</td>
            <td><span class="badge badge-sm ${hostSettingTone(row.status)}">${escapeHtml(row.status || "unknown")}</span></td>
            <td class="max-w-sm text-sm text-base-content/70">${escapeHtml(row.detail || "—")}</td>
          </tr>`,
        )
        .join("")}</tbody>
    </table>
  </div>`;
}

function telemetryV2Active(): boolean {
  return telemetryV2PresentationActive;
}

function renderHostSettingsSection(health: HealthData | null): string {
  const rows = health?.hostSettings || [];
  const healthLabel = health?.status
    ? `health ${escapeHtml(health.status)}`
    : "health unavailable";
  const rowLabel = pluralize(rows.length, "host setting");
  const expanded = !telemetryV2Active() || telemetryHostSettingsExpanded;
  if (expanded) {
    return `<section class="border-base-content/10 bg-base-200 rounded-lg border p-4" aria-label="Host settings">
      <div class="flex flex-wrap items-center justify-between gap-2">
        <div>
          <h2 class="font-semibold">Host settings</h2>
          <p class="text-base-content/60 mt-1 text-xs">${escapeHtml(rowLabel)} · ${healthLabel}</p>
        </div>
        ${
          telemetryV2Active()
            ? `<button id="telemetry-host-settings-toggle" type="button" class="btn btn-xs btn-outline" aria-label="Hide telemetry host settings" aria-expanded="true">Hide host settings</button>`
            : `<span class="text-base-content/50 text-xs">${healthLabel}</span>`
        }
      </div>
      ${renderHostSettings(health?.hostSettings)}
    </section>`;
  }
  return `<section class="border-base-content/10 bg-base-200 rounded-lg border p-4" aria-label="Host settings">
      <div class="flex flex-wrap items-center justify-between gap-2">
        <div>
          <h2 class="font-semibold">Host settings</h2>
          <p class="text-base-content/60 mt-1 text-xs">${escapeHtml(rowLabel)} · ${healthLabel}</p>
        </div>
        <button id="telemetry-host-settings-toggle" type="button" class="btn btn-xs btn-outline" aria-label="Show telemetry host settings" aria-expanded="false">Show host settings</button>
      </div>
      <p class="text-base-content/60 mt-3 text-sm">Kernel/runtime prerequisites are available when diagnosing host-level capacity or networking issues.</p>
    </section>`;
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
  const counterCount = Object.keys(stage.metrics || {}).length;
  const state = stage.active === false ? "inactive" : "active";
  return `<article class="border-base-content/10 bg-base-100 rounded-lg border p-3">
    <div class="flex items-center justify-between gap-2"><div><div class="text-sm font-semibold">${escapeHtml(stage.kind)}</div><p class="text-base-content/50 text-xs">${escapeHtml(state)}</p></div><button class="btn btn-xs btn-outline" type="button" aria-label="View ${escapeHtml(stage.kind)} telemetry details" data-stage-telemetry-key="${escapeHtml(key)}">Details</button></div>
    <p class="text-base-content/60 mt-3 text-xs">${escapeHtml(pluralize(counterCount, "counter"))} · raw values in Stage detail</p>
  </article>`;
}

function normalizeTelemetrySearch(value: string): string {
  return value.trim().toLowerCase();
}

function telemetryNoResultText(kind: string, query: string): string {
  const trimmed = query.trim();
  return `No ${kind} match "${trimmed}". Clear search to return to the full telemetry set.`;
}

function metricKeys(metrics: TelemetryMetrics | undefined): string {
  return Object.keys(metrics || {}).join(" ");
}

function readerSearchText(reader: { name: string }): string {
  return reader.name.toLowerCase();
}

function egressSearchText(egress: {
  outputId: string;
  status?: string;
  phase?: string;
  protocol?: string;
  targetUrl?: string;
  targetAddr?: string | null;
  lastError?: string | null;
  failurePhase?: string | null;
}): string {
  return [
    egress.outputId,
    egress.status,
    egress.phase,
    egress.protocol,
    egress.targetUrl,
    egress.targetAddr,
    egress.lastError,
    egress.failurePhase,
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
}

function stageSearchText(pipelineId: string, stage: TelemetryStage): string {
  return [
    stage.kind,
    stageKey(pipelineId, stage),
    stage.active === false ? "inactive" : "active",
    metricKeys(stage.metrics),
    metricKeys(stage.pipeMetrics),
    metricKeys(stage.payloadStats),
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
}

function telemetrySearchSummaryText(
  filteredReaders: number,
  totalReaders: number,
  filteredStages: number,
  totalStages: number,
  filteredEgresses: number,
  totalEgresses: number,
  search: string,
): string {
  const visible = `${pluralize(filteredReaders, "reader")} · ${pluralize(filteredStages, "stage")} · ${pluralize(filteredEgresses, "egress", "egresses")} visible`;
  if (!search) return visible;
  const total = totalReaders + totalStages + totalEgresses;
  const shown = filteredReaders + filteredStages + filteredEgresses;
  return `${shown}/${total} telemetry items match "${search}" · ${pluralize(filteredReaders, "reader")} · ${pluralize(filteredStages, "stage")} · ${pluralize(filteredEgresses, "egress", "egresses")}`;
}

function pluralize(
  count: number,
  singular: string,
  plural = `${singular}s`,
): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

function telemetryScopeLabel(
  pipelines: TelemetryPipelineOption[],
  pipelineId: string,
): string {
  if (!pipelineId) return "fleet";
  return (
    pipelines.find((pipeline) => pipeline.id === pipelineId)?.name ||
    pipelineId
  );
}

function telemetrySummaryText(
  engine: EngineTelemetrySnapshot | null,
  pipeline: PipelineTelemetrySnapshot | null,
  pipelines: TelemetryPipelineOption[],
  pipelineId: string,
  status: {
    loaded?: boolean;
    unavailable?: boolean;
    stageUnavailable?: boolean;
  },
): string {
  const scope = telemetryScopeLabel(pipelines, pipelineId);
  if (!status.loaded) return `Loading telemetry snapshots · ${scope}`;
  const ingests = engine?.ingests.length ?? 0;
  const stages = pipeline?.stages.length ?? engine?.stages.length ?? 0;
  const egresses = pipeline?.egresses.length ?? engine?.egresses.length ?? 0;
  const readers = pipeline?.sourceRing?.readers.length ?? 0;
  const state = status.unavailable ? "stale" : "loaded";
  return `Telemetry ${state} · ${pluralize(ingests, "ingest")} · ${pluralize(stages, "stage")} · ${pluralize(egresses, "egress", "egresses")} · ${pluralize(readers, "reader")} · ${scope}`;
}

function telemetryStatusTone(): TelemetryCheckpointModel["statusTone"] {
  if (!telemetryLoaded) return "neutral";
  if (telemetryUnavailable || stageUnavailable) return "warning";
  return "success";
}

function buildTelemetryCheckpointModel(): TelemetryCheckpointModel {
  const pipelines = viewOptions?.pipelines || [];
  const scope = telemetryScopeLabel(pipelines, selectedPipelineId);
  const normalizedSearch = normalizeTelemetrySearch(telemetrySearchQuery);
  const ring = pipelineSnapshot?.sourceRing;
  const readers = ring?.readers || [];
  const stages = pipelineSnapshot?.stages || [];
  const egresses = pipelineSnapshot?.egresses || [];
  const filteredReaders = readers.filter(
    (reader) =>
      !normalizedSearch || readerSearchText(reader).includes(normalizedSearch),
  );
  const filteredStages = stages.filter(
    (item) =>
      !normalizedSearch ||
      stageSearchText(selectedPipelineId, item).includes(normalizedSearch),
  );
  const filteredEgresses = egresses.filter(
    (egress) =>
      !normalizedSearch || egressSearchText(egress).includes(normalizedSearch),
  );
  const searchLabel = telemetryLoaded
    ? telemetrySearchSummaryText(
        filteredReaders.length,
        readers.length,
        filteredStages.length,
        stages.length,
        filteredEgresses.length,
        egresses.length,
        telemetrySearchQuery.trim(),
      )
    : "Loading matches";
  const summary = telemetrySummaryText(
    engineSnapshot,
    pipelineSnapshot,
    pipelines,
    selectedPipelineId,
    {
      loaded: telemetryLoaded,
      unavailable: telemetryUnavailable,
      stageUnavailable,
    },
  );
  const stageCounterCount = stages.reduce(
    (total, stage) => total + Object.keys(stage.metrics || {}).length,
    0,
  );
  const statusLabel = !telemetryLoaded
    ? "Loading"
    : telemetryUnavailable
      ? "Stale"
      : stageUnavailable
        ? "Stage stale"
        : "Loaded";
  const focusLabel = !telemetryLoaded
    ? "Telemetry snapshots are loading. The dense counter surfaces below will populate once the first snapshot arrives."
    : telemetryUnavailable
      ? "Some telemetry is stale. Compare the counters below with Status before changing runtime configuration."
      : normalizedSearch && filteredReaders.length + filteredStages.length + filteredEgresses.length === 0
        ? "No telemetry item matches this search. Clear the filter to return to the full counter set."
        : stageUnavailable
          ? "The selected stage detail is stale or unavailable. Use the stage cards below to pick a currently active stage."
          : "Telemetry is current for this scope. Use the dense cards below to validate the specific reader, stage, or egress path.";
  const nextStep = !telemetryLoaded
    ? "Wait for the snapshot or refresh manually."
    : telemetryUnavailable || stageUnavailable
      ? "Open Status to confirm process health, then refresh telemetry."
      : normalizedSearch
        ? "Clear search when the filtered counter is resolved."
        : "Select a stage for detailed counters or search for the affected output.";

  return {
    canOpenStatus: true,
    counterLabel: `${stageCounterCount.toLocaleString()} stage counters`,
    egressLabel: pluralize(egresses.length, "egress", "egresses"),
    focusLabel,
    metrics: [
      {
        label: "Readers",
        value: String(readers.length),
      },
      {
        label: "Transcoder buffers",
        value: String(engineSnapshot?.activeTranscoderBuffers ?? "—"),
      },
    ],
    nextStep,
    pipelineLabel: scope === "fleet" ? "Fleet" : scope,
    searchLabel,
    statusLabel,
    statusTone: telemetryStatusTone(),
    summary,
    title: "Engineer telemetry",
  };
}

export function renderEngineerTelemetryHtml(
  engine: EngineTelemetrySnapshot | null,
  pipeline: PipelineTelemetrySnapshot | null,
  stage: StageTelemetrySnapshot | null,
  health: HealthData | null,
  pipelines: TelemetryPipelineOption[],
  pipelineId: string,
  status: {
    loaded?: boolean;
    unavailable?: boolean;
    stageUnavailable?: boolean;
  } = {},
  searchQuery = "",
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
  const normalizedSearch = normalizeTelemetrySearch(searchQuery);
  const filteredReaders = readers.filter(
    (reader) =>
      !normalizedSearch || readerSearchText(reader).includes(normalizedSearch),
  );
  const filteredStages = stages.filter(
    (item) =>
      !normalizedSearch ||
      stageSearchText(pipelineId, item).includes(normalizedSearch),
  );
  const filteredEgresses = egresses.filter(
    (egress) =>
      !normalizedSearch || egressSearchText(egress).includes(normalizedSearch),
  );
  const egressesAreBounded =
    !normalizedSearch &&
    filteredEgresses.length > TELEMETRY_EGRESS_CARD_LIMIT &&
    !telemetryEgressExpanded;
  const visibleEgresses = egressesAreBounded
    ? filteredEgresses.slice(0, TELEMETRY_EGRESS_CARD_LIMIT)
    : filteredEgresses;
  const showEgressToggle =
    !normalizedSearch && filteredEgresses.length > TELEMETRY_EGRESS_CARD_LIMIT;
  const egressCaption = showEgressToggle
    ? `${pluralize(visibleEgresses.length, "egress", "egresses")} shown of ${filteredEgresses.length}. Search to narrow the list, or show all when comparing destinations.`
    : "";
  const stagesAreBounded =
    !normalizedSearch &&
    filteredStages.length > TELEMETRY_STAGE_CARD_LIMIT &&
    !telemetryStagesExpanded;
  const visibleStages = stagesAreBounded
    ? filteredStages.slice(0, TELEMETRY_STAGE_CARD_LIMIT)
    : filteredStages;
  const showStagesToggle =
    !normalizedSearch && filteredStages.length > TELEMETRY_STAGE_CARD_LIMIT;
  const stageCaption = showStagesToggle
    ? `${pluralize(visibleStages.length, "stage")} shown of ${filteredStages.length}. Search to narrow the list, or show all when comparing processing branches.`
    : "";
  const searchSummaryText = telemetrySearchSummaryText(
    filteredReaders.length,
    readers.length,
    filteredStages.length,
    stages.length,
    filteredEgresses.length,
    egresses.length,
    searchQuery.trim(),
  );
  const availability = !status.loaded
    ? `<div class="alert"><span>Loading telemetry snapshots…</span></div>`
    : status.unavailable
      ? `<div class="alert alert-warning"><span>Telemetry is temporarily unavailable. Last known counters remain visible.</span></div>`
      : "";
  const summaryText = telemetrySummaryText(
    engine,
    pipeline,
    pipelines,
    pipelineId,
    status,
  );
  return `<div class="mx-auto max-w-7xl space-y-4">
    <header class="flex flex-wrap items-end justify-between gap-3"><div><h1 class="text-lg font-semibold">Engineer telemetry</h1><p class="text-base-content/60 mt-1 text-sm">Point-in-time engine, ring, reader, stage, and egress counters.</p></div><div class="flex items-center gap-2"><select id="telemetry-pipeline-select" class="select select-sm" aria-label="Filter telemetry by pipeline">${options || `<option value="">No pipelines</option>`}</select><button id="telemetry-refresh-btn" type="button" class="btn btn-sm btn-outline" aria-label="Refresh telemetry data">Refresh</button></div></header>
    <p id="telemetry-route-summary" class="text-base-content/60 text-sm" role="status" aria-live="polite">${escapeHtml(summaryText)}</p>
    <section class="border-base-content/10 bg-base-200 rounded-lg border p-3" aria-label="Telemetry filter">
      <div class="flex flex-wrap items-end gap-3">
        <label class="min-w-60 flex-1 text-sm">
          <span class="text-base-content/70 mb-1 block text-xs font-semibold uppercase">Search telemetry items</span>
          <input id="telemetry-search" class="input input-sm input-bordered w-full" type="search" value="${escapeHtml(searchQuery)}" placeholder="reader, stage, egress, counter…" aria-label="Search telemetry items" autocomplete="off" />
        </label>
        <button id="telemetry-clear-search-btn" type="button" class="btn btn-sm btn-outline ${normalizedSearch ? "" : "hidden"}" aria-label="Clear telemetry search">Clear search</button>
      </div>
      <p id="telemetry-search-results-summary" class="text-base-content/60 mt-2 text-sm" role="status" aria-live="polite">${escapeHtml(searchSummaryText)}</p>
    </section>
    ${availability}
    <section class="grid gap-3 sm:grid-cols-2 lg:grid-cols-4" aria-label="Engine telemetry summary">
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Active ingests</div><div class="stat-value text-2xl">${engine?.ingests.length ?? "—"}</div></div>
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Stages</div><div class="stat-value text-2xl">${engine?.stages.length ?? "—"}</div></div>
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Egresses</div><div class="stat-value text-2xl">${engine?.egresses.length ?? "—"}</div></div>
      <div class="stat bg-base-200 rounded-lg"><div class="stat-title">Transcoder buffers</div><div class="stat-value text-2xl">${engine?.activeTranscoderBuffers ?? "—"}</div></div>
    </section>
    ${renderHostSettingsSection(health)}
    ${
      pipelineId && pipeline
        ? `<div class="grid gap-4 xl:grid-cols-[minmax(20rem,.75fr)_minmax(0,1.25fr)]">
      <div class="space-y-4">
        <section class="border-base-content/10 bg-base-200 rounded-lg border p-4"><h2 class="font-semibold">Source ring</h2>${ring ? `<div class="mt-3 grid grid-cols-2 gap-3 text-sm"><div><span class="text-base-content/60">Fill</span><div class="font-mono">${formatNumber(ring.fill)} / ${formatNumber(ring.capacity)} (${formatNumber(ring.fillPercent)}%)</div></div><div><span class="text-base-content/60">Depth</span><div class="font-mono">${formatNumber(ring.bufferDepthSecs)} s</div></div></div>` : `<p class="text-base-content/60 mt-3 text-sm">No active source ring.</p>`}</section>
        <section class="border-base-content/10 bg-base-200 rounded-lg border p-4"><h2 class="font-semibold">Readers</h2><div class="mt-3 space-y-2">${filteredReaders.length ? filteredReaders.map((reader) => `<div class="bg-base-100 rounded-md p-3 text-sm"><div class="font-medium">${escapeHtml(reader.name)}</div><div class="text-base-content/60 mt-1 text-xs">Lag ${formatNumber(reader.lagSlots)} slots · ${formatNumber(reader.overflowCount)} overflows · packet age ${formatNumber(reader.packetAgeMs)} ms</div></div>`).join("") : `<p class="text-base-content/60 text-sm">${normalizedSearch ? escapeHtml(telemetryNoResultText("readers", searchQuery)) : "No active readers."}</p>`}</div></section>
        <section class="border-base-content/10 bg-base-200 rounded-lg border p-4" aria-label="Telemetry egresses">
          <div class="flex flex-wrap items-start justify-between gap-2">
            <div>
              <h2 class="font-semibold">Egresses</h2>
              ${egressCaption ? `<p class="text-base-content/60 mt-1 text-xs">${escapeHtml(egressCaption)}</p>` : ""}
            </div>
            ${
              showEgressToggle
                ? `<button id="telemetry-egress-toggle" type="button" class="btn btn-xs btn-outline" aria-label="${telemetryEgressExpanded ? "Show fewer telemetry egresses" : `Show all ${filteredEgresses.length} telemetry egresses`}" aria-expanded="${telemetryEgressExpanded ? "true" : "false"}">${telemetryEgressExpanded ? "Show fewer" : `Show all ${filteredEgresses.length}`}</button>`
                : ""
            }
          </div>
          <div class="mt-3 space-y-2">${visibleEgresses.length ? visibleEgresses.map((egress) => `<div class="bg-base-100 rounded-md p-3 text-sm"><div class="flex justify-between gap-2"><span class="font-medium">${escapeHtml(egress.outputId)}</span><span class="badge badge-sm">${escapeHtml(egress.status || egress.phase || "unknown")}</span></div><div class="text-base-content/60 mt-1 text-xs">${formatBytes(egress.bytesOut)} sent${egress.lastError ? ` · ${escapeHtml(egress.lastError)}` : ""}</div></div>`).join("") : `<p class="text-base-content/60 text-sm">${normalizedSearch ? escapeHtml(telemetryNoResultText("egresses", searchQuery)) : "No active egresses."}</p>`}</div>
        </section>
      </div>
      <div class="space-y-4"><section aria-label="Telemetry processing stages"><div class="mb-3 flex flex-wrap items-start justify-between gap-2"><div><h2 class="font-semibold">Processing stages</h2>${stageCaption ? `<p class="text-base-content/60 mt-1 text-xs">${escapeHtml(stageCaption)}</p>` : ""}</div>${
        showStagesToggle
          ? `<button id="telemetry-stages-toggle" type="button" class="btn btn-xs btn-outline" aria-label="${telemetryStagesExpanded ? "Show fewer telemetry stages" : `Show all ${filteredStages.length} telemetry stages`}" aria-expanded="${telemetryStagesExpanded ? "true" : "false"}">${telemetryStagesExpanded ? "Show fewer" : `Show all ${filteredStages.length}`}</button>`
          : ""
      }</div><div class="grid gap-3 md:grid-cols-2">${visibleStages.length ? visibleStages.map((item) => renderStage(pipelineId, item)).join("") : `<div class="border-base-content/10 bg-base-200 rounded-lg border p-6 text-center text-sm">${normalizedSearch ? escapeHtml(telemetryNoResultText("stages", searchQuery)) : "No active stages."}</div>`}</div></section>
      <section id="stage-telemetry-detail" class="border-base-content/10 bg-base-200 rounded-lg border p-4">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <h2 class="font-semibold">Stage detail</h2>
          ${
            stage && telemetryV2Active()
              ? `<button id="telemetry-stage-detail-hide" type="button" class="btn btn-xs btn-outline" aria-label="Hide stage details for ${escapeHtml(stage.stageKey)}">Hide stage details</button>`
              : ""
          }
        </div>
        ${status.stageUnavailable ? `<p class="text-warning mt-2 text-sm">Fresh stage detail is unavailable; the stage may have stopped. Last known counters remain visible when available.</p>` : ""}
        ${stage ? `<div class="mt-1 text-xs text-base-content/60">${escapeHtml(stage.stageKey)}</div><div class="mt-3 grid gap-4 md:grid-cols-2"><div><h3 class="mb-2 text-sm font-medium">Throughput</h3>${metricRows(stage.metrics)}</div><div><h3 class="mb-2 text-sm font-medium">Pipe</h3>${metricRows(stage.pipeMetrics)}</div></div>` : status.stageUnavailable ? "" : `<p class="text-base-content/60 mt-3 text-sm">Select a stage to fetch its current detail.</p>`}
      </section></div>
    </div>`
        : pipelineId
          ? `<div class="border-base-content/10 bg-base-200 rounded-lg border p-8 text-center text-sm">${status.loaded ? "Pipeline telemetry is unavailable." : "Loading pipeline telemetry…"}</div>`
          : `<div class="border-base-content/10 bg-base-200 rounded-lg border p-8 text-center text-sm">Select or create a pipeline to inspect telemetry.</div>`
    }
  </div>`;
}

function isRenderCurrent(token: RenderScopeToken): boolean {
  return viewOptions?.active === true && telemetryScope.isCurrent(token);
}

function paintTelemetry(containerId = telemetryScope.current()): void {
  if (!viewOptions) return;
  telemetryCheckpointCallback?.(buildTelemetryCheckpointModel());
  const root = document.getElementById(containerId);
  if (!root) return;
  root.innerHTML = renderEngineerTelemetryHtml(
    engineSnapshot,
    pipelineSnapshot,
    stageSnapshot,
    healthSnapshot,
    viewOptions.pipelines,
    selectedPipelineId,
    {
      loaded: telemetryLoaded,
      unavailable: telemetryUnavailable,
      stageUnavailable,
    },
    telemetrySearchQuery,
  );
  suppressV2RouteChrome(root);
  const select = document.getElementById(
    "telemetry-pipeline-select",
  ) as HTMLSelectElement | null;
  select?.addEventListener("change", () => {
    selectTelemetryPipeline(select.value);
  });
  document
    .getElementById("telemetry-refresh-btn")
    ?.addEventListener("click", () => void refreshEngineerTelemetry(true));
  const search = document.getElementById(
    "telemetry-search",
  ) as HTMLInputElement | null;
  search?.addEventListener("input", () => {
    const cursor = search.selectionStart ?? search.value.length;
    telemetrySearchQuery = search.value;
    telemetryEgressExpanded = false;
    telemetryStagesExpanded = false;
    paintTelemetry();
    const nextSearch = document.getElementById(
      "telemetry-search",
    ) as HTMLInputElement | null;
    nextSearch?.focus();
    nextSearch?.setSelectionRange(cursor, cursor);
  });
  document
    .getElementById("telemetry-clear-search-btn")
    ?.addEventListener("click", () => {
      telemetrySearchQuery = "";
      telemetryEgressExpanded = false;
      telemetryStagesExpanded = false;
      paintTelemetry();
      const nextSearch = document.getElementById(
        "telemetry-search",
      ) as HTMLInputElement | null;
      nextSearch?.focus();
    });
  document
    .getElementById("telemetry-egress-toggle")
    ?.addEventListener("click", () => {
      telemetryEgressExpanded = !telemetryEgressExpanded;
      paintTelemetry();
    });
  document
    .getElementById("telemetry-stages-toggle")
    ?.addEventListener("click", () => {
      telemetryStagesExpanded = !telemetryStagesExpanded;
      paintTelemetry();
    });
  document
    .getElementById("telemetry-host-settings-toggle")
    ?.addEventListener("click", () => {
      telemetryHostSettingsExpanded = !telemetryHostSettingsExpanded;
      paintTelemetry();
    });
  document
    .getElementById("telemetry-stage-detail-hide")
    ?.addEventListener("click", () => {
      selectedStageKey = "";
      stageSnapshot = null;
      stageUnavailable = false;
      paintTelemetry();
    });
  document
    .querySelectorAll<HTMLElement>("[data-stage-telemetry-key]")
    .forEach((button) => {
      button.addEventListener("click", () => {
        const key = button.dataset.stageTelemetryKey;
        if (key) void fetchStageDetail(key);
      });
    });
}

function suppressV2RouteChrome(root: HTMLElement): void {
  if (
    typeof root.matches !== "function" ||
    !root.matches("[data-dashboard-v2-owned-route-body]")
  )
    return;
  root
    .querySelectorAll<HTMLElement>(
      ":scope > div > header:first-child h1, :scope > div > header:first-child p",
    )
    .forEach((element) => {
      element.hidden = true;
      element.setAttribute("aria-hidden", "true");
    });
}

export async function fetchStageDetail(stageKeyValue: string): Promise<void> {
  if (!viewOptions?.active || document.hidden) return;
  const sequence = ++stageRequestSequence;
  const pipelineAtRequest = selectedPipelineId;
  const scopeToken = telemetryScope.token();
  selectedStageKey = stageKeyValue;
  stageUnavailable = false;
  const result = await getStageTelemetry(stageKeyValue);
  if (
    sequence !== stageRequestSequence ||
    pipelineAtRequest !== selectedPipelineId ||
    selectedStageKey !== stageKeyValue ||
    !isRenderCurrent(scopeToken)
  )
    return;
  // A stage may disappear between the pipeline snapshot and this detail fetch.
  if (result) stageSnapshot = result;
  stageUnavailable = result === null;
  paintTelemetry(scopeToken.containerId);
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
  const scopeToken = telemetryScope.token();
  const request = (async () => {
    const [engine, health, pipeline, stage] = await Promise.all([
      getEngineTelemetry(),
      getHealth({ view: "summary" }),
      pipelineAtRequest
        ? getPipelineTelemetry(pipelineAtRequest)
        : Promise.resolve(null),
      stageAtRequest
        ? getStageTelemetry(stageAtRequest)
        : Promise.resolve(undefined),
    ]);
    if (
      sequence !== requestSequence ||
      pipelineAtRequest !== selectedPipelineId ||
      !isRenderCurrent(scopeToken)
    )
      return;
    engineSnapshot = engine ?? engineSnapshot;
    healthSnapshot = health ?? healthSnapshot;
    if (pipelineAtRequest) pipelineSnapshot = pipeline ?? pipelineSnapshot;
    telemetryLoaded = true;
    telemetryUnavailable =
      engine === null ||
      health === null ||
      (Boolean(pipelineAtRequest) && pipeline === null);
    if (stageAtRequest && stageAtRequest === selectedStageKey) {
      if (stage) stageSnapshot = stage;
      stageUnavailable = stage === null;
    }
    lastFetchedAt = Date.now();
    paintTelemetry(scopeToken.containerId);
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
  telemetryEgressExpanded = false;
  telemetryStagesExpanded = false;
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
  if (viewOptions?.active !== options.active) telemetryScope.invalidate();
  telemetryScope.setContainerId(options.containerId || "telemetry-mode-content");
  viewOptions = options;
  if (!options.active) {
    telemetryCheckpointCallback?.(null);
    return;
  }
  if (
    !options.pipelines.some((pipeline) => pipeline.id === selectedPipelineId)
  ) {
    selectTelemetryPipeline(options.pipelines[0]?.id || "");
  }
  paintTelemetry();
  void refreshEngineerTelemetry();
}

export function clearEngineerTelemetryMode(): void {
  if (!viewOptions || viewOptions.active === false) return;
  telemetryScope.invalidate();
  viewOptions = { ...viewOptions, active: false };
  telemetryCheckpointCallback?.(null);
}
