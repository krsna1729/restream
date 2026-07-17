import { outputViewEncodingLabel } from "../core/output-config.js";
import { state } from "../core/state.js";
import { escapeHtml, escapeRedactedHtml, getUrlParam } from "../core/utils.js";
import type { OutputView, PipelineView } from "../types.js";
import { openDiagnosticsModal } from "./diagnostics.js";
import { getPipelineSummary, getResourceMap } from "../core/api.js";
import type {
  OperatorAlert,
  PipelineSummarySnapshot,
  ResourceMapNode,
  ResourceMapSnapshot,
} from "../core/api.js";
import { fetchProcessingGraph, renderGraphInto } from "./graph.js";
import {
  isOutputFlapping,
  isOutputIntentStopped,
  isOutputRunning,
  isOutputRetrying,
  isOutputUnexpectedlyDown,
} from "../core/output-status.js";
import type { PipelineInspectCheckpointModel } from "./pipeline-inspect-view-model.js";

interface PipelineInspectorDependencies {
  selectPipeline: (pipelineId: string) => void;
  openOperateView: (pipelineId: string) => void;
}

const dependencies: PipelineInspectorDependencies = {
  selectPipeline: () => {},
  openOperateView: () => {},
};

let graphPipelineId: string | null = null;
let graphInFlight: Promise<void> | null = null;
let graphRequestSeq = 0;
let graphRenderedStateKey: string | null = null;
let graphAutoRefresh = true;
let forceRuntimeScope = false;
let runtimeScopeMaskedPipelineId: string | null = null;
let summaryRequestSeq = 0;
let summaryInFlight: Promise<void> | null = null;
const pipelineSummaryCache = new Map<string, PipelineSummarySnapshot>();
const pipelineResourceMapCache = new Map<string, ResourceMapSnapshot>();
let inspectOutputSearchQuery = "";
let inspectResourceDetailsExpanded = false;
let inspectPresentationCallback:
  | ((model: PipelineInspectCheckpointModel | null) => void)
  | null = null;

const RUNTIME_SCOPE_VALUE = "__runtime";
const RESOURCE_MAP_TOP_N = 25;

export function configurePipelineInspectCheckpointPresentation(options: {
  readonly onPresentation?: (
    model: PipelineInspectCheckpointModel | null,
  ) => void;
}): void {
  inspectPresentationCallback = options.onPresentation ?? null;
}

type ScrollSnapshot = {
  windowX: number;
  windowY: number;
  documentLeft: number;
  documentTop: number;
  targets: { key: string; left: number; top: number }[];
};

export function setPipelineInspectorDependencies(
  next: Partial<PipelineInspectorDependencies>,
): void {
  Object.assign(dependencies, next || {});
}

function selectedPipeline(): PipelineView | null {
  const urlPipelineId = getUrlParam("p");
  if (
    forceRuntimeScope &&
    urlPipelineId &&
    runtimeScopeMaskedPipelineId !== urlPipelineId
  ) {
    forceRuntimeScope = false;
    runtimeScopeMaskedPipelineId = null;
  }
  const selectedId = forceRuntimeScope ? null : urlPipelineId;
  if (!selectedId) return null;
  return state.pipelines.find((pipeline) => pipeline.id === selectedId) || null;
}

function hasInvalidPipelineSelection(): boolean {
  const selectedId = getUrlParam("p");
  return (
    Boolean(selectedId) &&
    !state.pipelines.some((pipeline) => pipeline.id === selectedId)
  );
}

function clearPipelineUrlSelection(): void {
  const url = new URL(window.location.href);
  url.searchParams.delete("p");
  window.history.replaceState?.({}, "", url.toString());
}

function formatBitrate(kbps: number | null | undefined): string {
  if (!Number.isFinite(kbps as number) || (kbps as number) < 0) return "--";
  const value = kbps as number;
  return value >= 1000
    ? `${(value / 1000).toFixed(1)} Mb/s`
    : `${value.toFixed(0)} Kb/s`;
}

function formatBytes(bytes: number | null | undefined): string {
  if (!Number.isFinite(bytes as number) || (bytes as number) <= 0) return "--";
  const value = bytes as number;
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
  if (value < 1024 * 1024 * 1024)
    return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
}

function formatAgeMs(ms: number | null | undefined): string {
  if (!Number.isFinite(ms as number) || (ms as number) < 0) return "--";
  const seconds = Math.round((ms as number) / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  return remainder ? `${minutes}m ${remainder}s` : `${minutes}m`;
}

function scrollKeyForElement(element: Element): string | null {
  const explicit = element.getAttribute("data-scroll-preserve");
  if (explicit) return `data:${explicit}`;
  const id = element.getAttribute("id");
  return id ? `id:${id}` : null;
}

function scrollSelectorForKey(key: string): string {
  const isId = key.startsWith("id:");
  const value = isId ? key.slice(3) : key.slice(5);
  const escaped =
    typeof CSS !== "undefined" && typeof CSS.escape === "function"
      ? CSS.escape(value)
      : value.replace(/["\\]/g, "\\$&");
  return isId ? `#${escaped}` : `[data-scroll-preserve="${escaped}"]`;
}

function captureScrollSnapshot(root: HTMLElement): ScrollSnapshot {
  const documentElement = document.scrollingElement || document.documentElement;
  const candidates = [
    document.getElementById("inspect-mode-panel"),
    document.getElementById("inspect-graph-container"),
    root,
    ...Array.from(root.querySelectorAll<HTMLElement>("[id],[data-scroll-preserve]")),
  ].filter((element): element is HTMLElement => Boolean(element));
  const seen = new Set<string>();
  const targets: ScrollSnapshot["targets"] = [];
  for (const element of candidates) {
    const key = scrollKeyForElement(element);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    targets.push({
      key,
      left: element.scrollLeft,
      top: element.scrollTop,
    });
  }
  return {
    windowX: window.scrollX || 0,
    windowY: window.scrollY || 0,
    documentLeft: documentElement?.scrollLeft || 0,
    documentTop: documentElement?.scrollTop || 0,
    targets,
  };
}

function restoreScrollSnapshot(snapshot: ScrollSnapshot): void {
  const documentElement = document.scrollingElement || document.documentElement;
  if (documentElement) {
    documentElement.scrollLeft = snapshot.documentLeft;
    documentElement.scrollTop = snapshot.documentTop;
  }
  for (const target of snapshot.targets) {
    const element = document.querySelector<HTMLElement>(
      scrollSelectorForKey(target.key),
    );
    if (!element) continue;
    element.scrollLeft = target.left;
    element.scrollTop = target.top;
  }
  try {
    window.scrollTo?.(snapshot.windowX, snapshot.windowY);
  } catch {
    // Test DOMs can expose scrollTo without implementing it.
  }
}

function withPreservedScroll(root: HTMLElement, render: () => void): void {
  const snapshot = captureScrollSnapshot(root);
  render();
  restoreScrollSnapshot(snapshot);
}

function pipelineHealthLabel(pipe: PipelineView): {
  label: string;
  cls: string;
} {
  if (pipe.input.status === "error")
    return { label: "Input error", cls: "badge-error" };
  if (pipe.input.status === "warning")
    return {
      label: pipe.input.flapping ? "Input flapping" : "Input warning",
      cls: "badge-warning",
    };
  if (pipe.input.status !== "on") {
    return pipe.outs.some(isOutputUnexpectedlyDown)
      ? { label: "Input down", cls: "badge-error" }
      : { label: "Idle", cls: "badge-neutral" };
  }
  if (!pipe.input.probeReady)
    return { label: "Input probing", cls: "badge-warning" };
  if (pipe.outs.some(isOutputUnexpectedlyDown))
    return { label: "Output down", cls: "badge-error" };
  if (pipe.outs.some(isOutputRetrying))
    return { label: "Output retrying", cls: "badge-warning" };
  if (pipe.outs.some(isOutputFlapping))
    return { label: "Output flapping", cls: "badge-warning" };
  if (pipe.outs.some((output) => output.status === "warning"))
    return { label: "Output warning", cls: "badge-warning" };
  if (pipe.input.flapping)
    return { label: "Input flapping", cls: "badge-warning" };
  return { label: "Live", cls: "badge-success" };
}

function outputStateLabel(out: OutputView): { label: string; cls: string } {
  if (isOutputIntentStopped(out))
    return { label: "Stopped", cls: "badge-neutral" };
  if (out.status === "failed") return { label: "Failed", cls: "badge-error" };
  if (out.status === "stalled")
    return { label: "Stalled", cls: "badge-warning" };
  if (isOutputRetrying(out)) return { label: "Retrying", cls: "badge-warning" };
  if (isOutputFlapping(out)) return { label: "Flapping", cls: "badge-warning" };
  if (isOutputRunning(out)) return { label: "Running", cls: "badge-success" };
  if (out.status === "warning")
    return { label: "Warning", cls: "badge-warning" };
  return { label: "Down", cls: "badge-error" };
}

function inspectOutputSearchText(out: OutputView): string {
  const stateLabel = outputStateLabel(out).label.toLowerCase();
  const aliases = new Set<string>();
  const status = (out.status || "").trim().toLowerCase();
  const desiredState = (out.desiredState || "").trim().toLowerCase();
  if (status) aliases.add(status);
  if (desiredState) aliases.add(desiredState);
  if (stateLabel) aliases.add(stateLabel);
  if (stateLabel === "failed" || stateLabel === "down") {
    aliases.add("down");
  }
  if (stateLabel === "running") aliases.add("live");
  if (stateLabel === "stopped") {
    aliases.add("off");
    aliases.add("offline");
  }
  return [
    out.name,
    out.url,
    outputViewEncodingLabel(out),
    out.phase || "",
    out.failurePhase || "",
    out.lastError || "",
    ...aliases,
  ]
    .join(" ")
    .toLowerCase();
}

function titleCaseValue(value: string | null | undefined): string {
  const normalized = String(value || "").trim();
  if (!normalized) return "--";
  if (normalized === "--") return normalized;
  return normalized
    .split(/([\s/_-]+)/)
    .map((part) =>
      /^[a-z]/.test(part) ? part.charAt(0).toUpperCase() + part.slice(1) : part,
    )
    .join("");
}

function protocolValue(value: string | null | undefined): string {
  const normalized = String(value || "").trim();
  if (!normalized) return "--";
  return normalized.length <= 5 ? normalized.toUpperCase() : titleCaseValue(normalized);
}

function pipelineInspectV2Active(): boolean {
  const toggle = document.getElementById("dashboard-ui-v2-toggle");
  if (toggle instanceof HTMLInputElement && toggle.checked) return true;
  try {
    return new URLSearchParams(window.location.search).get("ui") === "v2";
  } catch (_err) {
    return false;
  }
}

function normalizeInspectSearch(value: string): string {
  return value.trim().toLowerCase();
}

function pluralize(
  count: number,
  singular: string,
  plural = `${singular}s`,
): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

function inspectInputLabel(pipe: PipelineView): string {
  if (pipe.input.status === "on") return "input live";
  if (pipe.input.status === "warning") return "input warning";
  if (pipe.input.status === "error") return "input error";
  return "input idle";
}

function inspectAttentionCount(pipe: PipelineView): number {
  return pipe.outs.filter(
    (output) =>
      isOutputUnexpectedlyDown(output) ||
      isOutputRetrying(output) ||
      isOutputFlapping(output),
  ).length;
}

function inspectSummaryText(
  pipe: PipelineView | null,
  invalidPipelineSelection = false,
): string {
  if (invalidPipelineSelection) return "Inspecting missing pipeline selection";
  if (!pipe)
    return `Inspecting whole runtime · ${pluralize(state.pipelines.length, "pipeline")}`;
  return `Inspecting ${pipe.name} · ${inspectInputLabel(pipe)} · ${pluralize(pipe.outs.length, "output")} · ${pluralize(inspectAttentionCount(pipe), "attention item")}`;
}

function inspectFaultCandidates(pipe: PipelineView): OutputView[] {
  return pipe.outs.filter(
    (output) =>
      isOutputUnexpectedlyDown(output) ||
      isOutputRetrying(output) ||
      isOutputFlapping(output),
  );
}

function inspectProbeBlockers(pipe: PipelineView): string[] {
  const blockers: string[] = [];
  if (pipe.input.status !== "on")
    blockers.push("Input must be online for active probes.");
  if (!pipe.input.publisher?.protocol)
    blockers.push("Publisher protocol is not known yet.");
  return blockers;
}

function inspectSuggestedNextStep(pipe: PipelineView): string {
  const retryingOutputs = pipe.outs.filter(isOutputRetrying);
  const flappingOutputs = pipe.outs.filter(isOutputFlapping);
  return pipe.input.status === "on"
    ? retryingOutputs.length
      ? "Inspect recent errors and retry backoff before forcing a restart."
      : flappingOutputs.length
        ? "Inspect recent sink failures before forcing a restart."
        : "Run diagnostics, then inspect graph edges with zero packet output."
    : "Start or reconnect the publisher before probing.";
}

function buildInspectCheckpointModel(
  pipe: PipelineView | null,
  invalidPipelineSelection = false,
): PipelineInspectCheckpointModel | null {
  if (invalidPipelineSelection) {
    return {
      pipelineId: null,
      title: "Missing pipeline",
      summary: "The selected pipeline is no longer available.",
      statusLabel: "Missing",
      statusTone: "error",
      inputLabel: "No pipeline selected",
      outputLabel: "0 outputs",
      attentionLabel: "0 attention items",
      graphLabel: "No graph",
      focusLabel: "Select another pipeline to inspect diagnostics.",
      nextStep: "Choose a valid pipeline from the selector.",
      canOpenPipeline: false,
      canRunDiagnostics: false,
      diagnosticsDisabledReason: "Select a valid pipeline first.",
      metrics: [],
    };
  }
  if (!pipe) {
    return {
      pipelineId: null,
      title: "Whole runtime",
      summary: inspectSummaryText(null),
      statusLabel: "Runtime",
      statusTone: "neutral",
      inputLabel: pluralize(state.pipelines.length, "pipeline"),
      outputLabel: "Runtime resource map",
      attentionLabel: "Select a pipeline for diagnostics",
      graphLabel: graphInFlight ? "Loading resources" : "Runtime resources",
      focusLabel: "Inspection focus · select a pipeline to inspect diagnostics.",
      nextStep: "Select a pipeline to inspect graph edges and active probes.",
      canOpenPipeline: false,
      canRunDiagnostics: false,
      diagnosticsDisabledReason: "Select a pipeline first.",
      metrics: [
        { label: "Pipelines", value: String(state.pipelines.length) },
        {
          label: "Graph",
          value: graphInFlight ? "Loading" : "Runtime",
        },
      ],
    };
  }

  const apiSummary = pipelineSummaryCache.get(pipe.id) || null;
  const health = pipelineHealthLabel(pipe);
  const graph = apiSummary?.graph;
  const graphLabel = graph?.hasGraph
    ? `${graph.activeNodes ?? 0}/${graph.nodes ?? 0} active`
    : apiSummary
      ? "not active"
      : "loading";
  const outputCountLabel =
    Number.isFinite(apiSummary?.outputs?.running as number) &&
    Number.isFinite(apiSummary?.outputs?.total as number)
      ? `${apiSummary?.outputs?.running}/${apiSummary?.outputs?.total}`
      : `${pipe.outs.filter(isOutputRunning).length}/${pipe.outs.length}`;
  const blockers = inspectProbeBlockers(pipe);
  const faultCandidates = inspectFaultCandidates(pipe);
  const statusTone =
    pipe.input.status === "error" || faultCandidates.some(isOutputUnexpectedlyDown)
      ? "error"
      : blockers.length || faultCandidates.length
        ? "warning"
        : health.label === "Healthy"
          ? "success"
          : "neutral";
  const nextStep = inspectSuggestedNextStep(pipe);

  return {
    pipelineId: pipe.id,
    title: pipe.name,
    summary: inspectSummaryText(pipe),
    statusLabel: health.label,
    statusTone,
    inputLabel: inspectInputLabel(pipe),
    outputLabel: `${outputCountLabel} outputs running`,
    attentionLabel: pluralize(faultCandidates.length, "fault candidate"),
    graphLabel,
    focusLabel: `Inspection focus · ${
      blockers.length
        ? `${pluralize(blockers.length, "blocker")} before active probes`
        : "ready for active probes"
    } · ${pluralize(faultCandidates.length, "fault candidate")} · ${nextStep}`,
    nextStep,
    canOpenPipeline: true,
    canRunDiagnostics: pipe.input.status === "on",
    diagnosticsDisabledReason:
      pipe.input.status === "on" ? "" : "Input must be online for diagnostics.",
    metrics: [
      { label: "Input", value: titleCaseValue(pipe.input.status) },
      {
        label: "Publisher",
        value: protocolValue(pipe.input.publisher?.protocol),
      },
      { label: "Graph", value: titleCaseValue(graphLabel) },
      { label: "In", value: formatBitrate(pipe.stats.inputBitrateKbps) },
      { label: "Out", value: formatBitrate(pipe.stats.outputBitrateKbps) },
      {
        label: "Alerts",
        value: apiSummary ? String(apiSummary.alerts?.length ?? 0) : "Loading",
      },
    ],
  };
}

function updateInspectRouteSummary(
  pipe: PipelineView | null,
  invalidPipelineSelection = false,
): void {
  const summary = document.getElementById("inspect-route-summary");
  if (!summary) return;
  summary.textContent = inspectSummaryText(pipe, invalidPipelineSelection);
}

function renderInspectCheckpointPresentation(
  pipe: PipelineView | null,
  invalidPipelineSelection = false,
): void {
  inspectPresentationCallback?.(
    buildInspectCheckpointModel(pipe, invalidPipelineSelection),
  );
}

export function renderPipelineInspector(): void {
  const pipe = selectedPipeline();
  const invalidPipelineSelection = hasInvalidPipelineSelection();
  updateInspectRouteSummary(pipe, invalidPipelineSelection);
  renderInspectCheckpointPresentation(pipe, invalidPipelineSelection);
  const stateKey = graphStateKey(pipe);
  const select = document.getElementById(
    "inspect-pipeline-select",
  ) as HTMLSelectElement | null;
  if (select) {
    select.innerHTML = [
      `<option value="${RUNTIME_SCOPE_VALUE}">Whole Runtime</option>`,
      ...state.pipelines.map(
        (pipeline) =>
          `<option value="${escapeHtml(pipeline.id)}">${escapeHtml(pipeline.name)}</option>`,
      ),
    ].join("");
    select.value = pipe?.id || RUNTIME_SCOPE_VALUE;
    select.onchange = () => {
      const pipelineId = select.value || "";
      if (pipelineId === RUNTIME_SCOPE_VALUE) {
        resetPipelineInspectorSelection(null);
        clearPipelineUrlSelection();
        renderPipelineInspector();
        void refreshPipelineInspectorGraph();
        return;
      }
      if (!pipelineId) return;
      forceRuntimeScope = false;
      dependencies.selectPipeline(pipelineId);
      resetPipelineInspectorSelection(pipelineId);
      renderPipelineInspector();
      void refreshPipelineInspectorGraph();
    };
  }
  if (!pipe && graphPipelineId !== null) resetPipelineInspectorSelection(null);

  const openBtn = document.getElementById(
    "inspect-open-pipeline-btn",
  ) as HTMLButtonElement | null;
  if (openBtn) {
    openBtn.disabled = !pipe;
    openBtn.onclick = () => {
      if (pipe) dependencies.openOperateView(pipe.id);
    };
  }

  setPipelineOnlySectionsVisible(Boolean(pipe) || invalidPipelineSelection);
  renderSummary(pipe, invalidPipelineSelection);
  if (pipe) refreshPipelineSummary(pipe.id);
  renderInspectorResourceDetails(
    pipe,
    pipe ? pipelineResourceMapCache.get(pipe.id) || null : null,
  );
  renderDiagnostics(pipe);
  const graphHeading = document.getElementById("inspect-graph-heading");
  if (graphHeading) {
    graphHeading.textContent = pipe ? "Graph Explorer" : "Runtime Resources";
  }

  const refreshBtn = document.getElementById(
    "inspect-refresh-graph-btn",
  ) as HTMLButtonElement | null;
  if (refreshBtn) {
    refreshBtn.textContent = graphAutoRefresh ? "Stop Refresh" : "Auto Refresh";
    refreshBtn.setAttribute(
      "aria-label",
      graphAutoRefresh ? "Stop graph auto refresh" : "Start graph auto refresh",
    );
    refreshBtn.classList.toggle("btn-accent", graphAutoRefresh);
    refreshBtn.classList.toggle("btn-outline", !graphAutoRefresh);
    refreshBtn.setAttribute(
      "aria-pressed",
      graphAutoRefresh ? "true" : "false",
    );
    refreshBtn.onclick = () => {
      graphAutoRefresh = !graphAutoRefresh;
      renderPipelineInspector();
      if (graphAutoRefresh) void refreshPipelineInspectorGraph();
    };
  }
  const diagnosticsBtn = document.getElementById(
    "inspect-open-diagnostics-btn",
  ) as HTMLButtonElement | null;
  if (diagnosticsBtn) {
    diagnosticsBtn.disabled = !pipe || pipe.input.status !== "on";
    diagnosticsBtn.onclick = () => {
      if (pipe) openDiagnosticsModal(pipe.id);
    };
  }

  if (
    pipe &&
    !graphInFlight &&
    (graphPipelineId !== pipe.id || graphRenderedStateKey !== stateKey)
  ) {
    void refreshPipelineInspectorGraph();
  } else if (pipe && shouldAutoRefreshGraph()) {
    void refreshPipelineInspectorGraph();
  } else if (
    !pipe &&
    !invalidPipelineSelection &&
    shouldAutoRefreshGraph()
  ) {
    void refreshPipelineInspectorGraph();
  }
}

function setPipelineOnlySectionsVisible(visible: boolean): void {
  for (const id of [
    "inspect-pipeline-summary",
    "inspect-resource-details",
    "inspect-diagnostics-summary",
  ]) {
    const section = document.getElementById(id)?.closest("section");
    section?.classList.toggle("hidden", !visible);
  }
}

export function resetPipelineInspectorSelection(
  pipelineId: string | null,
): void {
  forceRuntimeScope = pipelineId === null;
  inspectOutputSearchQuery = "";
  inspectResourceDetailsExpanded = false;
  runtimeScopeMaskedPipelineId =
    pipelineId === null ? getUrlParam("p") || graphPipelineId : null;
  graphRequestSeq++;
  graphPipelineId = pipelineId;
  graphRenderedStateKey = null;
  const status = document.getElementById("inspect-graph-status");
  const container = document.getElementById("inspect-graph-container");
  if (status)
    status.textContent = pipelineId
      ? "Loading graph..."
      : "Loading runtime resources...";
  if (container) {
    container.innerHTML = `<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">
            ${pipelineId ? "Loading graph..." : "Loading runtime resources..."}
        </div>`;
  }
}

function renderSummary(
  pipe: PipelineView | null,
  invalidPipelineSelection = false,
): void {
  const container = document.getElementById("inspect-pipeline-summary");
  if (!container) return;
  if (!pipe) {
    if (invalidPipelineSelection) {
      container.innerHTML =
        '<div class="text-base-content/60 text-sm">No pipeline selected.</div>';
      return;
    }
    container.innerHTML = `<div class="dashboard-card p-3">
        <div class="dashboard-kicker">Scope</div>
        <div class="mt-2 text-sm">Whole Runtime</div>
      </div>`;
    return;
  }

  const apiSummary = pipelineSummaryCache.get(pipe.id) || null;
  const health = pipelineHealthLabel(pipe);
  const apiOutputTotal = apiSummary?.outputs?.total;
  const apiOutputRunning = apiSummary?.outputs?.running;
  const graph = apiSummary?.graph;
  const alerts = apiSummary?.alerts || [];
  const outputCountLabel =
    Number.isFinite(apiOutputRunning as number) &&
    Number.isFinite(apiOutputTotal as number)
      ? `${apiOutputRunning}/${apiOutputTotal}`
      : `${pipe.outs.filter(isOutputRunning).length}/${pipe.outs.length}`;
  const graphLabel = graph?.hasGraph
    ? `${graph.activeNodes ?? 0}/${graph.nodes ?? 0} active`
    : apiSummary
      ? "not active"
      : "loading";
  const inputProgressStale =
    Number.isFinite(pipe.input.lastProgressAgeMs as number) &&
    (pipe.input.lastProgressAgeMs as number) >= 10_000;
  const inputRateLabel = inputProgressStale
    ? `${formatBitrate(pipe.stats.inputBitrateKbps)} · stale ${formatAgeMs(pipe.input.lastProgressAgeMs)}`
    : formatBitrate(pipe.stats.inputBitrateKbps);
  const metrics = [
    ["Input", titleCaseValue(pipe.input.status)],
    ["Publisher", protocolValue(pipe.input.publisher?.protocol)],
    ["Graph", titleCaseValue(graphLabel)],
    ["In", inputRateLabel],
    ["Out", formatBitrate(pipe.stats.outputBitrateKbps)],
    ["Outputs", outputCountLabel],
    ["Received", formatBytes(pipe.input.bytesReceived)],
    ["Sent", formatBytes(pipe.input.bytesSent)],
    ["Alerts", apiSummary ? String(alerts.length) : "Loading"],
  ];
  const outputPreviewLimit = 12;
  const normalizedOutputSearch = normalizeInspectSearch(inspectOutputSearchQuery);
  const filteredOutputs = normalizedOutputSearch
    ? pipe.outs.filter((out) =>
        inspectOutputSearchText(out).includes(normalizedOutputSearch),
      )
    : pipe.outs;
  const showOutputSearch =
    pipe.outs.length > 4 || normalizedOutputSearch !== "";
  const outputs = filteredOutputs
    .slice(0, outputPreviewLimit)
    .map((out) => {
      const stateLabel = outputStateLabel(out);
      const encodingLabel = outputViewEncodingLabel(out);
      return `<div class="border-base-content/10 border-t py-2 first:border-t-0">
                <div class="flex min-w-0 items-center justify-between gap-2">
                    <div class="min-w-0 truncate text-sm font-medium">${escapeHtml(out.name)}</div>
                    <span class="badge ${stateLabel.cls} badge-sm shrink-0 whitespace-nowrap">${stateLabel.label}</span>
                </div>
                <div class="text-base-content/60 mt-0.5 truncate text-xs">${escapeHtml(encodingLabel)} / ${escapeRedactedHtml(out.url, true)}</div>
            </div>`;
    })
    .join("");
  const remainingOutputs = Math.max(
    0,
    filteredOutputs.length - outputPreviewLimit,
  );

  container.innerHTML = `<div>
        <div class="mb-3 flex min-w-0 items-center justify-between gap-3">
            <div class="text-base-content/70 min-w-0 truncate text-sm">${escapeHtml(pipe.name)}</div>
            <span class="badge ${health.cls} badge-sm shrink-0 whitespace-nowrap">${health.label}</span>
        </div>
        <dl class="grid grid-cols-2 gap-2 text-sm md:grid-cols-3">
            ${metrics
              .map(
                ([label, value]) => `<div class="border-base-content/10 bg-base-100/60 rounded-lg border px-2.5 py-2">
                    <dt class="text-base-content/55 text-[0.68rem] font-semibold uppercase tracking-wide">${escapeHtml(label)}</dt>
                    <dd class="mt-0.5 truncate text-sm font-medium tabular-nums">${escapeHtml(value)}</dd>
                </div>`,
              )
              .join("")}
        </dl>
        ${alertSummaryHtml(apiSummary ? alerts : null, pipe)}
        <div class="mt-3">
            <div class="mb-2 flex flex-wrap items-end justify-between gap-2">
                <div>
                    <div class="text-base-content/60 text-[0.68rem] font-semibold uppercase tracking-wide">Outputs</div>
                    <div class="text-base-content/50 text-xs tabular-nums">${escapeHtml(outputCountLabel)}</div>
                </div>
                ${
                  showOutputSearch
                    ? `<div class="flex min-w-0 flex-1 flex-wrap items-center justify-end gap-2 sm:flex-none">
                        <label class="input input-bordered input-sm flex min-h-10 min-w-0 max-w-xs flex-1 items-center gap-2 sm:w-72">
                          <span class="text-base-content/55 text-xs font-semibold uppercase">Find</span>
                          <input id="inspect-output-search" class="min-w-0 grow" type="search" aria-label="Search inspect outputs" placeholder="output, state, URL" value="${escapeHtml(inspectOutputSearchQuery)}">
                        </label>
                        <button id="inspect-output-clear-search-btn" type="button" class="btn btn-xs btn-ghost ${normalizedOutputSearch ? "" : "hidden"}">Clear output search</button>
                      </div>`
                    : ""
                }
            </div>
            ${
              normalizedOutputSearch
                ? `<p id="inspect-output-search-summary" class="text-base-content/55 mb-2 text-xs tabular-nums" role="status" aria-live="polite">${escapeHtml(String(filteredOutputs.length))}/${escapeHtml(String(pipe.outs.length))} inspect outputs match · "${escapeHtml(inspectOutputSearchQuery.trim())}"</p>`
                : ""
            }
            <div id="inspect-output-preview-list" class="border-base-content/10 bg-base-100/40 rounded-md border px-2" aria-label="Inspect output preview">
                ${
                  outputs ||
                  (normalizedOutputSearch
                    ? `<div class="dashboard-muted py-2" role="status" aria-live="polite">No inspect outputs match "${escapeHtml(inspectOutputSearchQuery.trim())}". Clear output search to show all.</div>`
                    : '<div class="dashboard-muted py-2">No outputs configured.</div>')
                }
                ${remainingOutputs ? `<div class="text-base-content/60 border-base-content/10 border-t py-2 text-xs">+${remainingOutputs} more matching outputs in Operate</div>` : ""}
            </div>
        </div>
    </div>`;
  bindInspectOutputSearch(pipe);
}

function bindInspectOutputSearch(pipe: PipelineView): void {
  const input = document.getElementById(
    "inspect-output-search",
  ) as HTMLInputElement | null;
  if (input) {
    input.oninput = () => {
      inspectOutputSearchQuery = input.value;
      renderSummary(pipe);
      const nextInput = document.getElementById(
        "inspect-output-search",
      ) as HTMLInputElement | null;
      nextInput?.focus();
    };
  }
  document
    .getElementById("inspect-output-clear-search-btn")
    ?.addEventListener("click", () => {
      inspectOutputSearchQuery = "";
      renderSummary(pipe);
      const nextInput = document.getElementById(
        "inspect-output-search",
      ) as HTMLInputElement | null;
      nextInput?.focus();
    });
}

function alertSummaryHtml(
  alerts: OperatorAlert[] | null,
  pipe: PipelineView,
): string {
  if (alerts === null) {
    return `<section class="border-base-content/10 bg-base-100/40 mt-3 rounded-lg border px-2.5 py-2">
      <div class="text-base-content/55 text-[0.68rem] font-semibold uppercase tracking-wide">Active Alerts</div>
      <div class="text-base-content/60 mt-1 text-sm">Loading alert details.</div>
    </section>`;
  }
  if (alerts.length === 0) {
    return `<section class="border-base-content/10 bg-base-100/40 mt-3 rounded-lg border px-2.5 py-2">
      <div class="text-base-content/55 text-[0.68rem] font-semibold uppercase tracking-wide">Active Alerts</div>
      <div class="text-base-content/60 mt-1 text-sm">No active operator alerts.</div>
    </section>`;
  }
  return `<section class="border-warning/30 bg-warning/5 mt-3 rounded-lg border px-2.5 py-2">
    <div class="mb-1.5 flex items-center justify-between gap-2">
      <div class="text-warning text-[0.68rem] font-semibold uppercase tracking-wide">Active Alerts</div>
      <span class="text-warning text-xs font-medium tabular-nums">${escapeHtml(String(alerts.length))}</span>
    </div>
    <div class="max-h-64 space-y-2 overflow-y-auto pr-1" data-scroll-preserve="pipeline-alerts">
      ${alerts
        .map(
          (alert) => `<div class="border-warning/20 border-t pt-2 first:border-t-0 first:pt-0">
            <div class="flex min-w-0 items-center justify-between gap-2">
              <div class="min-w-0 truncate text-sm font-medium">${escapeHtml(operatorAlertText(alert.title, pipe))}</div>
              <span class="badge ${alertSeverityBadgeClass(alert.severity)} badge-sm shrink-0">${escapeHtml(titleCaseValue(alert.severity))}</span>
            </div>
            <div class="text-base-content/65 mt-0.5 line-clamp-2 text-xs">${escapeHtml(operatorAlertText(alert.cause, pipe))}</div>
            ${alert.recommendedAction ? `<div class="text-base-content/50 mt-1 text-xs">${escapeHtml(operatorAlertText(alert.recommendedAction, pipe))}</div>` : ""}
          </div>`,
        )
        .join("")}
    </div>
  </section>`;
}

function alertSeverityBadgeClass(severity: OperatorAlert["severity"]): string {
  if (severity === "critical") return "badge-error";
  if (severity === "warning") return "badge-warning";
  return "badge-info";
}

function operatorAlertText(text: string, pipe: PipelineView): string {
  let display = text || "";
  for (const output of pipe.outs) {
    if (!output.id || !output.name) continue;
    display = display.replaceAll(output.id, output.name);
  }
  display = display.replace(/\bOutput '([^']+)'/g, "$1");
  if (pipe.id) display = display.replaceAll(`${pipe.id}:`, "");
  return display;
}

function refreshPipelineSummary(pipelineId: string): void {
  if (pipelineSummaryCache.has(pipelineId)) return;
  if (summaryInFlight) return;
  const requestSeq = ++summaryRequestSeq;
  summaryInFlight = getPipelineSummary(pipelineId)
    .then((summary) => {
      if (!summary || requestSeq !== summaryRequestSeq) return;
      pipelineSummaryCache.set(pipelineId, summary);
      const pipe = selectedPipeline();
      if (pipe?.id === pipelineId) {
        renderSummary(pipe);
        renderInspectCheckpointPresentation(pipe);
      }
    })
    .catch(() => {})
    .finally(() => {
      summaryInFlight = null;
    });
}

function renderDiagnostics(pipe: PipelineView | null): void {
  const container = document.getElementById("inspect-diagnostics-summary");
  if (!container) return;
  const focusSummary = document.getElementById("inspect-focus-summary");
  if (!pipe) {
    if (focusSummary) {
      focusSummary.textContent =
        "Inspection focus · select a pipeline to inspect diagnostics.";
    }
    container.innerHTML =
      '<div class="text-base-content/60 text-sm">Select a pipeline to inspect diagnostics.</div>';
    return;
  }

  const blockers = inspectProbeBlockers(pipe);
  const faultCandidates = inspectFaultCandidates(pipe);
  const suggestedNextStep = inspectSuggestedNextStep(pipe);
  if (focusSummary) {
    focusSummary.textContent = `Inspection focus · ${blockers.length ? `${pluralize(blockers.length, "blocker")} before active probes` : "ready for active probes"} · ${pluralize(faultCandidates.length, "fault candidate")} · ${suggestedNextStep}`;
  }

  container.innerHTML = `<div class="grid gap-3 md:grid-cols-3">
        <div class="dashboard-stat-card-compact">
            <div class="dashboard-kicker">Probe Readiness</div>
            <div class="mt-2 text-sm">${blockers.length ? blockers.map(escapeHtml).join("<br>") : "Ready for active diagnostics."}</div>
        </div>
        <div class="dashboard-stat-card-compact">
            <div class="dashboard-kicker">Fault Candidates</div>
            <div class="mt-2 text-sm">${faultCandidates.length ? faultCandidates.map((out) => escapeHtml(out.name)).join("<br>") : "No unexpected output failures."}</div>
        </div>
        <div class="dashboard-stat-card-compact">
            <div class="dashboard-kicker">Suggested Next Step</div>
            <div class="mt-2 text-sm">${suggestedNextStep}</div>
        </div>
    </div>`;
}

function renderInspectorResourceDetails(
  pipe: PipelineView | null,
  resourceMap: ResourceMapSnapshot | null,
): void {
  const container = document.getElementById("inspect-resource-details");
  if (!container) return;
  if (!pipe) {
    container.innerHTML =
      '<div class="text-base-content/60 text-sm">Select a pipeline to inspect its FFmpeg workers and resource attribution.</div>';
    return;
  }
  if (!resourceMap) {
    container.innerHTML =
      '<div class="text-base-content/60 text-sm">Resource details are loading.</div>';
    return;
  }
  const detailHtml = resourceDetailPanelHtml(resourceMap, pipe);
  const resourceDetailsLabel = `${inspectResourceDetailsExpanded ? "Hide" : "Show"} resource details for ${pipe.name}`;
  const resourceDetailPanel = !pipelineInspectV2Active()
    ? detailHtml
    : `<section class="border-base-content/10 bg-base-100/35 rounded-lg border p-3">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <div>
            <div class="text-sm font-semibold">Resource detail tables</div>
            <div class="text-base-content/55 text-xs">Worker tables, truncation limits, and attribution accuracy.</div>
          </div>
          <button id="inspect-resource-details-toggle" type="button" class="btn btn-xs btn-outline" aria-label="${escapeHtml(resourceDetailsLabel)}" aria-expanded="${inspectResourceDetailsExpanded ? "true" : "false"}">${inspectResourceDetailsExpanded ? "Hide resource details" : "Show resource details"}</button>
        </div>
        ${inspectResourceDetailsExpanded ? `<div class="mt-3">${detailHtml}</div>` : ""}
      </section>`;
  container.innerHTML = `<div class="space-y-3">
    ${resourceSummaryGroupsHtml(resourceMap)}
    ${resourceDetailPanel}
  </div>`;
  document
    .getElementById("inspect-resource-details-toggle")
    ?.addEventListener("click", () => {
      inspectResourceDetailsExpanded = !inspectResourceDetailsExpanded;
      renderInspectorResourceDetails(pipe, resourceMap);
    });
}

export async function refreshPipelineInspectorGraph(): Promise<void> {
  const pipe = selectedPipeline();
  const requestStateKey = graphStateKey(pipe);
  const status = document.getElementById("inspect-graph-status");
  const container = document.getElementById("inspect-graph-container");
  if (!container) return;
  const requestSeq = ++graphRequestSeq;
  if (!pipe && hasInvalidPipelineSelection()) {
    graphPipelineId = null;
    if (status) status.textContent = "Select a pipeline.";
    withPreservedScroll(container, () => {
      container.innerHTML =
        '<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">Select a pipeline to inspect its graph.</div>';
    });
    return;
  }
  if (!pipe) {
    const canRefreshInPlace =
      graphPipelineId === null && graphRenderedStateKey === "runtime";
    graphPipelineId = null;
    if (status && !canRefreshInPlace)
      status.textContent = "Loading runtime resources...";
    if (!canRefreshInPlace) {
      withPreservedScroll(container, () => {
        container.innerHTML = `<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">
        Loading runtime resources...
    </div>`;
      });
    }
    graphInFlight = (async () => {
      const resourceMap = await getResourceMap(null, {
        view: "detail",
        topN: 200,
      });
      if (requestSeq !== graphRequestSeq || selectedPipeline()) return;
      if (!resourceMap) {
        if (status) status.textContent = "Runtime resources unavailable.";
        withPreservedScroll(container, () => {
          container.innerHTML =
            '<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">Runtime resources unavailable.</div>';
        });
        return;
      }
      withPreservedScroll(container, () => {
        renderResourceMapInto(container, resourceMap);
      });
      graphRenderedStateKey = "runtime";
      if (status) status.textContent = "Whole Runtime / resource overview";
    })();
    try {
      await graphInFlight;
    } finally {
      if (requestSeq === graphRequestSeq) graphInFlight = null;
    }
    return;
  }
  const requestPipelineId = pipe.id;
  const canRefreshInPlace =
    graphPipelineId === requestPipelineId && graphRenderedStateKey !== null;
  graphPipelineId = requestPipelineId;
  if (status && !canRefreshInPlace) status.textContent = "Loading graph...";
  if (!canRefreshInPlace) {
    withPreservedScroll(container, () => {
      container.innerHTML = `<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">
        Loading graph...
    </div>`;
    });
  }
  graphInFlight = (async () => {
    const [graph, resourceMap] = await Promise.all([
      fetchProcessingGraph(requestPipelineId),
      getResourceMap(requestPipelineId, {
        view: "detail",
        topN: 50,
      }),
    ]);
    if (
      requestSeq !== graphRequestSeq ||
      selectedPipeline()?.id !== requestPipelineId
    ) {
      return;
    }
    graphPipelineId = requestPipelineId;
    if (!graph || graph.pipelineId !== requestPipelineId) {
      if (status) status.textContent = "Graph unavailable.";
      withPreservedScroll(container, () => {
        container.innerHTML =
          '<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">Graph unavailable.</div>';
      });
      return;
    }
    withPreservedScroll(container, () => {
      renderProcessingGraphExplorer(
        container,
        graph as Parameters<typeof renderGraphInto>[1],
      );
    });
    if (resourceMap) {
      pipelineResourceMapCache.set(requestPipelineId, resourceMap);
      const currentPipe = selectedPipeline();
      if (currentPipe?.id === requestPipelineId) {
        withPreservedScroll(container, () => renderSummary(currentPipe));
        renderInspectorResourceDetails(currentPipe, resourceMap);
        renderInspectCheckpointPresentation(currentPipe);
      }
    }
    graphRenderedStateKey = requestStateKey;
    if (status) {
      const inputState =
        pipe.input.status === "on" ? "live" : pipe.input.status;
      status.textContent = `${pipe.name} / processing graph / ${pipe.outs.length} outputs / input ${inputState}`;
    }
  })();
  try {
    await graphInFlight;
  } finally {
    if (requestSeq === graphRequestSeq) graphInFlight = null;
  }
}

function renderProcessingGraphExplorer(
  container: HTMLElement,
  graph: Parameters<typeof renderGraphInto>[1],
): void {
  container.innerHTML = "";
  renderGraphInto(container, graph);
}

function renderGraphIntoShellSlot(
  container: HTMLElement,
  slot: HTMLElement | null,
  slotId: string,
  graph: Parameters<typeof renderGraphInto>[1],
): void {
  if (slot) {
    renderGraphInto(slot, graph);
    return;
  }
  const fallback = document.createElement("div");
  renderGraphInto(fallback, graph);
  const slotPattern = new RegExp(
    `(<div id="${slotId}"[^>]*>)(</div>)`,
  );
  container.innerHTML = container.innerHTML.replace(
    slotPattern,
    `$1${fallback.innerHTML}$2`,
  );
}

function graphExplorerShellHtml({
  graphSlotId,
  title,
  scopeLabel,
  footerHtml = "",
}: {
  graphSlotId: string;
  title: string;
  scopeLabel: string;
  footerHtml?: string;
}): string {
  return `<div class="space-y-3 p-3">
    <section class="dashboard-section p-3">
      <div class="mb-2 flex items-center justify-between gap-2">
        <h3 class="dashboard-section-title text-sm">${escapeHtml(title)}</h3>
        <span class="text-base-content/50 text-xs">${escapeHtml(scopeLabel)}</span>
      </div>
      <div id="${escapeHtml(graphSlotId)}" class="bg-base-100 h-[460px] overflow-auto rounded-lg"></div>
    </section>
    ${footerHtml}
  </div>`;
}

function resourceSummaryStripHtml(snapshot: ResourceMapSnapshot): string {
  const scopeKind = snapshot.scope?.kind || "runtime";
  const cards = resourceSummaryCards(
    snapshot.summary || {},
    scopeKind,
    snapshot.nodes || [],
    snapshot.scope?.pipelineId || null,
  ).slice(0, 6);
  return `<div class="mt-3 grid gap-2 border-base-content/10 border-t pt-3 sm:grid-cols-2 xl:grid-cols-3">
      ${cards
        .map(
          ({
            label,
            value,
            confidence,
            hint,
            subtext,
          }) => `<div class="dashboard-stat-card-compact p-2">
          <div class="flex items-center justify-between gap-2">
            <div class="text-base-content/60 text-[0.65rem] font-semibold uppercase">${escapeHtml(label)}</div>
            <span class="text-base-content/50 text-right text-[0.65rem]">${escapeHtml(confidence)}${hint ? ` <span>· ${escapeHtml(hint)}</span>` : ""}</span>
          </div>
          <div class="mt-0.5 text-sm font-medium tabular-nums">${escapeHtml(value)}</div>
          ${subtext ? `<div class="text-base-content/50 mt-0.5 text-xs">${escapeHtml(subtext)}</div>` : ""}
        </div>`,
        )
        .join("")}
    </div>`;
}

function resourceSummaryGroupsHtml(snapshot: ResourceMapSnapshot): string {
  const scopeKind = snapshot.scope?.kind || "runtime";
  const cards = resourceSummaryCards(
    snapshot.summary || {},
    scopeKind,
    snapshot.nodes || [],
    snapshot.scope?.pipelineId || null,
  );
  const processCards = cards.slice(0, 3);
  const pipelineCards = cards.slice(3);
  return `<div class="grid gap-3 lg:grid-cols-2">
    ${resourceSummaryGroupHtml("Process Metrics", "Measured on the restream process/runtime.", processCards)}
    ${resourceSummaryGroupHtml(scopeKind === "pipeline" ? "Pipeline Attribution" : "Runtime Attribution", "Derived or attributed to active pipeline resources.", pipelineCards)}
  </div>`;
}

function resourceSummaryGroupHtml(
  title: string,
  subtitle: string,
  cards: ResourceSummaryCard[],
): string {
  return `<section class="dashboard-section bg-base-100/35 p-3">
    <div class="mb-2">
      <div class="text-sm font-semibold">${escapeHtml(title)}</div>
      <div class="text-base-content/55 text-xs">${escapeHtml(subtitle)}</div>
    </div>
    <div class="grid gap-2 sm:grid-cols-3 lg:grid-cols-1 2xl:grid-cols-3">
      ${cards
        .map(
          ({ label, value, confidence, hint, subtext }) => `<div class="dashboard-stat-card-compact p-2">
            <div class="flex items-center justify-between gap-2">
              <div class="text-base-content/60 text-[0.65rem] font-semibold uppercase">${escapeHtml(label)}</div>
              <span class="text-base-content/50 text-right text-[0.65rem]">${escapeHtml(confidence)}${hint ? ` <span>· ${escapeHtml(hint)}</span>` : ""}</span>
            </div>
            <div class="mt-0.5 truncate text-sm font-medium tabular-nums">${escapeHtml(value)}</div>
            ${subtext ? `<div class="text-base-content/50 mt-0.5 text-xs">${escapeHtml(subtext)}</div>` : ""}
          </div>`,
        )
        .join("")}
    </div>
  </section>`;
}

function resourceDetailPanelHtml(
  snapshot: ResourceMapSnapshot,
  pipe: PipelineView | null,
): string {
  return `<div class="space-y-2">
    ${ffmpegWorkerBreakdownHtml(snapshot, pipe)}
    ${resourceLimitNoticeHtml(snapshot)}
    ${resourceAccuracyLegendHtml("compact")}
  </div>`;
}

function ffmpegWorkerBreakdownHtml(
  snapshot: ResourceMapSnapshot,
  pipe: PipelineView | null,
): string {
  const scopedPipelineId =
    snapshot.scope?.kind === "pipeline" ? snapshot.scope.pipelineId || null : null;
  const expectedCount =
    scopedPipelineId === null
      ? summaryNumber(snapshot.summary || {}, "externalFfmpegCount") || 0
      : null;
  const workers = (snapshot.nodes || [])
    .filter((node) => isFfmpegWorkerNode(node, scopedPipelineId))
    .sort((a, b) => (a.label || a.id).localeCompare(b.label || b.id));
  const displayedCount =
    scopedPipelineId === null ? expectedCount || 0 : workers.length;
  if (workers.length === 0 && displayedCount <= 0) return "";
  if (workers.length === 0) {
    return `<section class="dashboard-section p-3">
      <div class="text-sm font-semibold">FFmpeg workers</div>
      <div class="text-base-content/60 mt-1 text-sm">Measured ${escapeHtml(String(expectedCount))} child process${expectedCount === 1 ? "" : "es"}; stage attribution is still warming up.</div>
    </section>`;
  }
  const rows = workers
    .map(
      (node) => `<tr>
        <td class="min-w-0"><div class="whitespace-normal break-words text-sm font-medium">${escapeHtml(resourceNodeDisplayLabel(node, pipe))}</div></td>
        <td class="text-sm tabular-nums">${formatPercent(
          typeof node.cpuPercent === "number" ? node.cpuPercent : null,
        )}</td>
        <td><span class="text-sm tabular-nums">${formatBytes(node.memory?.attributedBytes ?? null)}</span> <span class="text-base-content/50 text-xs">${escapeHtml(node.memory?.confidence || "")}</span></td>
      </tr>`,
    )
    .join("");
  const mismatch =
    expectedCount !== null && workers.length !== expectedCount
      ? `<div class="text-warning mt-1">Accounted ${escapeHtml(String(workers.length))} stage worker${workers.length === 1 ? "" : "s"} for ${escapeHtml(String(expectedCount))} measured FFmpeg child process${expectedCount === 1 ? "" : "es"}.</div>`
      : "";
  return `<section class="dashboard-section p-3">
      <div class="flex items-center justify-between gap-2">
        <div class="text-sm font-semibold">FFmpeg workers</div>
      </div>
      <div class="mt-2 overflow-auto">
        <table class="table table-sm table-fixed">
          <colgroup>
            <col class="w-auto" />
            <col class="w-20" />
            <col class="w-32" />
          </colgroup>
          <thead>
            <tr class="text-base-content/55 text-[0.68rem] uppercase tracking-wide"><th>Worker</th><th>CPU</th><th>Memory</th></tr>
          </thead>
          <tbody>${rows}</tbody>
        </table>
      </div>
      ${mismatch}
    </section>`;
}

function isFfmpegWorkerNode(
  node: ResourceMapNode,
  pipelineId: string | null,
): boolean {
  if (node.kind !== "stage" || node.execution !== "child_process") {
    return false;
  }
  if (pipelineId === null) return Boolean(node.pipelineId);
  return node.pipelineId === pipelineId;
}

function summaryNumber(
  summary: ResourceMapSnapshot["summary"],
  key: string,
): number | null {
  const value = summary[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function formatPercent(value: number | null): string {
  return value === null ? "--" : `${value.toFixed(1)}%`;
}

function resourceNodeScore(node: ResourceMapNode): number {
  const memory = node.memory?.attributedBytes || 0;
  const cpu = node.cpuPercent || 0;
  const hotspots = node.hotspots?.length || 0;
  return memory + cpu * 1024 * 1024 + hotspots * 512 * 1024;
}

function renderResourceMapInto(
  container: HTMLElement,
  snapshot: ResourceMapSnapshot,
): void {
  const scopeKind = snapshot.scope?.kind || "runtime";
  const pipe =
    scopeKind === "pipeline" &&
    snapshot.scope?.pipelineId &&
    selectedPipeline()?.id === snapshot.scope.pipelineId
      ? selectedPipeline()
      : null;
  const nodes = resourceTopNodes(snapshot.nodes || [], scopeKind)
    .sort((a, b) => resourceNodeScore(b) - resourceNodeScore(a))
    .slice(0, 12);
  container.innerHTML = runtimeResourceOverviewHtml(
    snapshot,
    scopeKind,
    nodes,
    pipe,
  );
}

function resourceTopNodes(
  nodes: ResourceMapNode[],
  scopeKind: string,
): ResourceMapNode[] {
  if (scopeKind !== "runtime") return [...nodes];
  const hasChildProcessStageGroup = nodes.some(
    (node) =>
      node.kind === "resource_group" &&
      node.execution === "child_process" &&
      String(node.label || "").toLowerCase().includes("stage"),
  );
  if (!hasChildProcessStageGroup) return [...nodes];
  return nodes.filter((node) => node.kind !== "child_process_group");
}

function runtimeResourceOverviewHtml(
  snapshot: ResourceMapSnapshot,
  scopeKind: string,
  nodes: ResourceMapNode[],
  pipe: PipelineView | null,
): string {
  if (scopeKind === "runtime") {
    return wholeRuntimeResourceHtml(snapshot, nodes);
  }
  return `<div class="space-y-3 p-3">
    <section class="dashboard-section p-3">
      <div class="mb-3 flex flex-wrap items-start justify-between gap-2">
        <div>
          <h3 class="dashboard-section-title">Runtime Resource Overview</h3>
          <p class="dashboard-subtitle">Measured counters and attribution tables. Runtime scope avoids drawing a topology graph because shared workers and grouped resources do not map cleanly to graph edges.</p>
        </div>
        <span class="text-base-content/50 text-xs">${escapeHtml(scopeKind)}</span>
      </div>
      ${resourceSummaryStripHtml(snapshot)}
      <div class="mt-3">${resourceDetailPanelHtml(snapshot, pipe)}</div>
    </section>
    ${resourceTopNodesHtml(scopeKind, nodes, pipe)}
  </div>`;
}

function wholeRuntimeResourceHtml(
  snapshot: ResourceMapSnapshot,
  nodes: ResourceMapNode[],
): string {
  return `<div class="space-y-3 p-3">
    <header class="flex flex-wrap items-start justify-between gap-3">
      <div>
        <h3 class="dashboard-section-title">Whole Runtime</h3>
        <p class="dashboard-subtitle">Process, FFmpeg, thread, and shared-resource attribution across the running restream instance.</p>
      </div>
      <span class="text-base-content/50 text-xs">live resource snapshot</span>
    </header>
    ${resourceSummaryStripHtml(snapshot)}
    ${runtimeStageBreakdownHtml(snapshot)}
    <div class="grid min-w-0 gap-3 xl:grid-cols-[minmax(0,0.95fr)_minmax(0,1.05fr)]">
      ${ffmpegWorkerBreakdownHtml(snapshot, null)}
      ${resourceTopNodesHtml("runtime", nodes, null)}
    </div>
    ${resourceLimitNoticeHtml(snapshot)}
    ${resourceAccuracyLegendHtml("compact")}
  </div>`;
}

function runtimeStageBreakdownHtml(snapshot: ResourceMapSnapshot): string {
  const nodes = (snapshot.nodes || [])
    .filter(isRuntimeBreakdownNode)
    .sort((a, b) => resourceNodeScore(b) - resourceNodeScore(a));
  return `<section class="dashboard-section p-3 text-xs">
    <div class="mb-2 flex flex-wrap items-center justify-between gap-2">
      <h3 class="text-sm font-semibold">Stage Breakdown</h3>
      <span class="text-base-content/50 text-xs">${escapeHtml(String(nodes.length))} runtime resource${nodes.length === 1 ? "" : "s"}</span>
    </div>
    <div class="overflow-auto">
      <table class="table table-sm">
        <thead>
          <tr class="text-base-content/55 text-[0.68rem] uppercase tracking-wide">
            <th>Stage</th><th>Pipeline</th><th>Execution</th><th>CPU</th><th>Memory</th><th>Threads</th>
          </tr>
        </thead>
        <tbody>
          ${
            nodes.length
              ? nodes
                  .map(
                    (node) => `<tr>
                      <td><div class="max-w-96 whitespace-normal break-words font-medium">${escapeHtml(runtimeNodeDisplayLabel(node))}</div><div class="text-base-content/50 text-xs">${escapeHtml(runtimeNodeKindLabel(node))}</div></td>
                      <td>${escapeHtml(runtimeNodePipelineLabel(node))}</td>
                      <td>${escapeHtml(node.execution || "--")}</td>
                      <td class="tabular-nums">${formatPercent(typeof node.cpuPercent === "number" ? node.cpuPercent : null)}</td>
                      <td><span class="tabular-nums">${formatBytes(node.memory?.attributedBytes ?? null)}</span> <span class="text-base-content/50 text-xs">${escapeHtml(node.memory?.confidence || "")}</span></td>
                      <td>${escapeHtml(formatThreadCell(node))}</td>
                    </tr>`,
                  )
                  .join("")
              : `<tr><td colspan="6" class="text-base-content/60">No runtime resources reported.</td></tr>`
          }
        </tbody>
      </table>
    </div>
  </section>`;
}

function isRuntimeBreakdownNode(node: ResourceMapNode): boolean {
  return [
    "runtime_process",
    "stage",
    "egress",
    "source_ring",
  ].includes(node.kind);
}

function runtimeNodeDisplayLabel(node: ResourceMapNode): string {
  const pipe = pipelineForResourceNode(node);
  if (pipe && node.kind === "egress") return outputDisplayName(pipe, node.id);
  return stripPipelineScope(node.label || node.id, pipe);
}

function runtimeNodePipelineLabel(node: ResourceMapNode): string {
  const pipe = pipelineForResourceNode(node);
  if (pipe) return pipe.name || pipe.id;
  if (node.pipelineId) return compactPipelineId(node.pipelineId);
  return "Runtime";
}

function runtimeNodeKindLabel(node: ResourceMapNode): string {
  if (node.kind === "runtime_process") return "restream process";
  if (node.kind === "source_ring") return "source ring";
  if (node.kind === "egress") return "output worker";
  return "processing stage";
}

function pipelineForResourceNode(node: ResourceMapNode): PipelineView | null {
  if (!node.pipelineId) return null;
  return state.pipelines.find((pipeline) => pipeline.id === node.pipelineId) || null;
}

function compactPipelineId(pipelineId: string): string {
  const suffix = pipelineId.replace(/^pipeline_/, "").slice(-8);
  return suffix ? `Pipeline ${suffix}` : "Pipeline";
}

function resourceTopNodesHtml(
  _scopeKind: string,
  nodes: ResourceMapNode[],
  pipe: PipelineView | null,
): string {
  return `<section class="dashboard-section p-3 text-xs">
      <div class="mb-2 flex items-center justify-between gap-2">
        <h3 class="text-sm font-semibold">Top Resource Nodes</h3>
      </div>
      <div id="runtime-resource-table-scroll" class="overflow-auto" data-scroll-preserve="runtime-resource-table">
        <table class="table table-xs">
          <thead><tr><th>Node</th><th>Execution</th><th>CPU</th><th>Memory</th><th>Threads</th><th>Signals</th></tr></thead>
          <tbody>
            ${
              nodes.length
                ? nodes
                    .map(
                      (node) => `<tr>
                        <td><div class="font-medium">${escapeHtml(resourceNodeDisplayLabel(node, pipe))}</div>${resourceNodeSubtextHtml(node, pipe)}</td>
                        <td>${escapeHtml(node.execution || "--")}</td>
                        <td class="tabular-nums">${formatPercent(typeof node.cpuPercent === "number" ? node.cpuPercent : null)}</td>
                        <td>${formatBytes(node.memory?.attributedBytes ?? null)} <span class="text-base-content/50 text-xs">${escapeHtml(node.memory?.confidence || "")}</span></td>
                        <td>${escapeHtml(formatThreadCell(node))}</td>
                        <td>${escapeHtml((node.hotspots || []).join(", ") || "--")}</td>
                      </tr>`,
                    )
                    .join("")
                : `<tr><td colspan="6" class="text-base-content/60">No active resource nodes.</td></tr>`
            }
          </tbody>
        </table>
      </div>
    </section>`;
}

function resourceNodeDisplayLabel(
  node: ResourceMapNode,
  pipe: PipelineView | null,
): string {
  if (pipe && node.kind === "egress") {
    return outputDisplayName(pipe, node.id);
  }
  return stripPipelineScope(node.label || node.id, pipe);
}

function resourceNodeSubtextHtml(
  node: ResourceMapNode,
  pipe: PipelineView | null,
): string {
  if (pipe && node.kind === "egress") {
    const protocol = String(node.label || "")
      .replace(/\s+output$/i, "")
      .toUpperCase();
    return protocol
      ? `<div class="text-base-content/50 max-w-80 truncate text-xs">${escapeHtml(protocol)} output</div>`
      : "";
  }
  const subtext = stripPipelineScope(node.id, pipe);
  if (!subtext || subtext === resourceNodeDisplayLabel(node, pipe)) return "";
  return `<div class="text-base-content/50 max-w-80 truncate text-xs">${escapeHtml(subtext)}</div>`;
}

function outputDisplayName(pipe: PipelineView, outputId: string): string {
  const output = pipe.outs.find((candidate) => candidate.id === outputId);
  return output?.name || compactOutputId(outputId);
}

function compactOutputId(outputId: string): string {
  const suffix = outputId.replace(/^output_/, "").slice(-8);
  return suffix ? `Output ${suffix}` : "Output";
}

function stripPipelineScope(value: string, pipe: PipelineView | null): string {
  if (!pipe?.id) return value;
  return value.replaceAll(`${pipe.id}:`, "");
}

function resourceLimitNoticeHtml(snapshot: ResourceMapSnapshot): string {
  const truncated = Number(snapshot.limits?.truncatedNodeCount || 0);
  if (!Number.isFinite(truncated) || truncated <= 0) return "";
  const total = Number(snapshot.limits?.totalNodeCount || 0);
  const returned = Number(snapshot.limits?.returnedNodeCount || 0);
  const view = snapshot.view || "grouped";
  return `<div class="text-base-content/60 text-xs">
    ${escapeHtml(view)} view showing ${escapeHtml(String(returned))} of ${escapeHtml(String(total))} resource nodes.
  </div>`;
}

type ResourceSummaryCard = {
  label: string;
  value: string;
  hint?: string;
  subtext?: string;
  confidence: "measured" | "derived" | "estimated";
};

function resourceSummaryCards(
  summary: ResourceMapSnapshot["summary"],
  scopeKind: string,
  nodes: ResourceMapNode[],
  pipelineId: string | null,
): ResourceSummaryCard[] {
  const scoped = scopeKind === "pipeline";
  const runtimeSrtSenders = summaryNumber(summary, "srtSenderThreads");
  const srtSenderLimit = summaryNumber(summary, "srtSenderThreadLimit");
  const pipelineSrtSenders = scoped
    ? countPipelineSrtSenders(nodes, pipelineId)
    : null;
  return [
    {
      label: scoped ? "Process CPU" : "CPU",
      value: formatPercent(summaryNumber(summary, "cpuPercent")),
      confidence: "measured",
    },
    {
      label: scoped ? "Process RSS" : "RSS",
      value: formatBytes(summaryNumber(summary, "totalMemoryBytes")),
      confidence: "measured",
    },
    {
      label: "Threads",
      value: String(summaryNumber(summary, "processThreadCount") ?? "--"),
      confidence: "measured",
    },
    {
      label: "SRT Senders",
      value: scoped
        ? `${pipelineSrtSenders} / ${runtimeSrtSenders ?? "--"}`
        : `${runtimeSrtSenders ?? "--"} active`,
      hint: `max ${srtSenderLimit ?? "--"}`,
      confidence: "derived",
    },
    {
      label: "FFmpeg",
      value: `${summaryNumber(summary, "externalFfmpegCount") ?? 0} child`,
      confidence: "measured",
    },
    {
      label: "Retained",
      value: formatBytes(summaryNumber(summary, "retainedPayloadBytes")),
      confidence: "derived",
    },
  ];
}

function countPipelineSrtSenders(
  nodes: ResourceMapNode[],
  pipelineId: string | null,
): number {
  return nodes.filter((node) => {
    if (node.kind !== "egress" || node.execution !== "os_thread") {
      return false;
    }
    if (pipelineId && node.pipelineId !== pipelineId) return false;
    return String(node.label || "").toLowerCase().includes("srt");
  }).length;
}

function resourceAccuracyLegendHtml(mode: "compact" | "full"): string {
  const wrapperClass =
    mode === "compact"
      ? "text-base-content/60 text-xs"
      : "dashboard-section p-3 text-xs";
  const gridClass =
    mode === "compact"
      ? "mt-1 flex flex-wrap gap-x-3 gap-y-1"
      : "mt-2 grid gap-2 sm:grid-cols-3";
  return `<section class="${wrapperClass}">
      <div class="font-semibold uppercase">Accuracy</div>
      <div class="${gridClass}">
        <div><span class="font-semibold">Measured</span> values come from OS or process counters.</div>
        <div><span class="font-semibold">Derived</span> values come from runtime queues, rings, and permits.</div>
        <div><span class="font-semibold">Estimated</span> values are proportional attribution inside shared work.</div>
      </div>
    </section>`;
}

function renderRuntimeResourceGraph(
  container: HTMLElement,
  snapshot: ResourceMapSnapshot,
  nodes: ResourceMapNode[],
): void {
  const graphContainer = container.querySelector(
    "#runtime-resource-graph",
  ) as HTMLElement | null;
  const graphResources = runtimeGraphResources(
    nodes,
    snapshot.scope?.kind || "runtime",
  );
  const graphNodes = graphResources.map((node) => ({
    id: node.id,
    type: resourceGraphNodeType(node),
    label: node.label || node.id,
    active: true,
    details: {
      resource: true,
      cpu: formatPercent(
        typeof node.cpuPercent === "number" ? node.cpuPercent : null,
      ),
      execution: node.execution || "--",
      memory: `${formatBytes(node.memory?.attributedBytes ?? null)} ${
        node.memory?.confidence || ""
      }`.trim(),
      threads: formatThreadCell(node),
    },
  }));
  if (graphNodes.length === 0) {
    const emptyHtml =
      '<div class="text-base-content/60 flex min-h-72 items-center justify-center text-sm">No active resource nodes.</div>';
    if (graphContainer) {
      graphContainer.innerHTML = emptyHtml;
    } else {
      container.innerHTML = container.innerHTML.replace(
        /(<div id="runtime-resource-graph"[^>]*>)(<\/div>)/,
        `$1${emptyHtml}$2`,
      );
    }
    return;
  }

  const graphNodeIds = new Set(graphNodes.map((node) => node.id));
  const rootId =
    graphResources.find(isRuntimeProcessNode)?.id || graphNodes[0].id;
  const ffmpegId =
    graphResources.find(isExternalFfmpegGroupNode)?.id || null;
  const edges = graphNodes
    .filter((node) => node.id !== rootId)
    .map((node) => {
      const from =
        ffmpegId &&
        node.id !== ffmpegId &&
        (node.type === "transcoder" ||
          node.details?.execution === "child_process")
          ? ffmpegId
          : rootId;
      return {
        from,
        to: node.id,
        label: from === ffmpegId ? "child work" : "runtime",
      };
    })
    .filter((edge) => edge.from !== edge.to && graphNodeIds.has(edge.from));

  const graph = {
    pipelineId: String(snapshot.scope?.pipelineId || "runtime"),
    nodes: graphNodes,
    edges,
  } as Parameters<typeof renderGraphInto>[1];
  renderGraphIntoShellSlot(
    container,
    graphContainer,
    "runtime-resource-graph",
    graph,
  );
}

function runtimeGraphResources(
  nodes: ResourceMapNode[],
  scopeKind: string,
): ResourceMapNode[] {
  const hasExternalFfmpegGroup = nodes.some(isExternalFfmpegGroupNode);
  const graphNodes = nodes
    .filter(
      (node) =>
        !(
          hasExternalFfmpegGroup &&
          isStageAggregateNode(node)
        ),
    );
  if (scopeKind === "pipeline") return graphNodes.slice(0, 8);
  return collapseRuntimeEgressGroupsForGraph(graphNodes).slice(0, 8);
}

function collapseRuntimeEgressGroupsForGraph(
  nodes: ResourceMapNode[],
): ResourceMapNode[] {
  const egressNodes = nodes.filter(isRuntimeEgressGroupNode);
  if (egressNodes.length <= 1) return nodes;
  const egressIds = new Set(egressNodes.map((node) => node.id));
  const outputCount = egressNodes.reduce(
    (sum, node) => sum + countGroupedResources(node.label),
    0,
  );
  const childThreads = egressNodes.reduce(
    (sum, node) => sum + Number(node.threads?.childProcess || 0),
    0,
  );
  const appThreads = egressNodes.reduce(
    (sum, node) => sum + Number(node.threads?.appOwned || 0),
    0,
  );
  const cpuPercent = egressNodes.reduce(
    (sum, node) => sum + Number(node.cpuPercent || 0),
    0,
  );
  const memoryBytes = egressNodes.reduce(
    (sum, node) => sum + Number(node.memory?.attributedBytes || 0),
    0,
  );
  const hotspots = [
    ...new Set(egressNodes.flatMap((node) => node.hotspots || [])),
  ];
  const collapsed: ResourceMapNode = {
    id: "runtime:outputs",
    kind: "egress",
    label: `Outputs (${outputCount || egressNodes.length})`,
    execution: "mixed",
    cpuPercent,
    memory: {
      attributedBytes: memoryBytes,
      confidence: egressNodes.some(
        (node) => node.memory?.confidence === "measured",
      )
        ? "measured"
        : "derived",
    },
    threads: {
      appOwned: appThreads,
      childProcess: childThreads,
    },
    hotspots,
  };
  const firstEgressIndex = nodes.findIndex((node) => egressIds.has(node.id));
  return nodes.flatMap((node, index) => {
    if (!egressIds.has(node.id)) return [node];
    return index === firstEgressIndex ? [collapsed] : [];
  });
}

function isRuntimeProcessNode(node: ResourceMapNode): boolean {
  const kind = String(node.kind || "");
  const id = String(node.id || "");
  return (
    id === "runtime:restream" ||
    kind === "runtime_process" ||
    (kind === "resource_group" && node.execution === "process")
  );
}

function isExternalFfmpegGroupNode(node: ResourceMapNode): boolean {
  const id = String(node.id || "");
  const kind = String(node.kind || "");
  const label = String(node.label || "").toLowerCase();
  return (
    id === "runtime:external-ffmpeg" ||
    kind === "child_process_group" ||
    (kind === "resource_group" &&
      node.execution === "child_process" &&
      label.includes("ffmpeg"))
  );
}

function isStageAggregateNode(node: ResourceMapNode): boolean {
  const id = String(node.id || "");
  const kind = String(node.kind || "");
  const label = String(node.label || "").toLowerCase();
  return (
    id.includes("group:stage:") ||
    kind.includes("stage") ||
    label.includes(" stages")
  );
}

function isRuntimeEgressGroupNode(node: ResourceMapNode): boolean {
  const id = String(node.id || "");
  const label = String(node.label || "").toLowerCase();
  return (
    id.startsWith("group:egress:") ||
    (node.kind === "egress" && label.includes("outputs"))
  );
}

function countGroupedResources(label: string | null | undefined): number {
  const match = /\((\d+)\)/.exec(String(label || ""));
  return match ? Number(match[1]) : 1;
}

function resourceGraphNodeType(node: ResourceMapNode): string {
  const kind = String(node.kind || "");
  const label = String(node.label || "").toLowerCase();
  if (kind.includes("ingest")) return "ingest";
  if (kind.includes("egress")) return "egress";
  if (kind.includes("stage") || node.execution === "child_process")
    return "transcoder";
  if (kind.includes("ring") || kind.includes("pipeline")) return "ring_buffer";
  if (kind.includes("process")) return "packetizer";
  if (node.execution === "process") return "packetizer";
  if (node.execution === "os_thread") return "egress";
  if (node.execution === "shared" && label.includes("ring"))
    return "ring_buffer";
  return "demux";
}

function formatThreadCell(node: ResourceMapNode): string {
  const threads = node.threads || {};
  return (
    Object.entries(threads)
      .filter(([, value]) => Number(value) > 0)
      .map(([key, value]) => `${key}: ${value}`)
      .join(", ") || "--"
  );
}

function graphStateKey(pipe: PipelineView | null): string | null {
  if (!pipe) return null;
  const outputs = pipe.outs
    .map((out) =>
      [
        out.id,
        out.status,
        out.desiredState,
        outputViewEncodingLabel(out),
        out.phase || "",
        out.retrying ? "1" : "0",
        out.flapping ? "1" : "0",
        out.lastError || "",
      ].join(":"),
    )
    .join("|");
  return [
    pipe.id,
    pipe.name,
    pipe.input.status,
    pipe.input.probeStatus,
    pipe.input.readers,
    pipe.input.audioTracks.length,
    pipe.input.video?.codec || "",
    pipe.hlsPreview?.active ? "1" : "0",
    outputs,
  ].join("::");
}

export function syncPipelineInspectorVisibility(): void {
  if (graphAutoRefresh && !document.hidden && !graphInFlight) {
    void refreshPipelineInspectorGraph();
  }
}

function shouldAutoRefreshGraph(): boolean {
  return graphAutoRefresh && !document.hidden && !graphInFlight;
}
