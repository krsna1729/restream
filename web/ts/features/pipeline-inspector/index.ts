import { outputViewEncodingLabel } from "../../core/output-config.js";
import { RenderScope } from "../../core/render-scope.js";
import type { RenderScopeToken } from "../../core/render-scope.js";
import { state } from "../../core/state.js";
import { escapeHtml, escapeRedactedHtml, getUrlParam } from "../../core/utils.js";
import type { OutputView, PipelineView } from "../../types.js";
import { openDiagnosticsModal } from "../diagnostics.js";
import { pipelineInspectorShellHtml } from "./shell.js";
import { getPipelineSummary, getResourceMap } from "../../core/api.js";
import type {
  OperatorAlert,
  PipelineSummarySnapshot,
  ResourceMapNode,
  ResourceMapSnapshot,
} from "../../core/api.js";
import { fetchProcessingGraph, renderGraphInto } from "../graph.js";
import {
  isOutputFlapping,
  isOutputIntentStopped,
  isOutputRunning,
  isOutputRetrying,
  isOutputUnexpectedlyDown,
} from "../../core/output-status.js";
import type { PipelineInspectCheckpointModel } from "../pipeline-inspect-view-model.js";
import {
  renderResourceMapInto,
  resourceDetailPanelHtml,
  resourceSummaryGroupsHtml,
} from "./resource-view.js";
import {
  getGraphAutoRefresh,
  getGraphInFlight,
  getGraphPipelineId,
  getGraphRenderedStateKey,
  getCachedResourceMap,
  refreshPipelineInspectorGraph,
  renderGraphIntoShellSlot,
  resetGraphState,
  setGraphAutoRefresh,
  setGraphDeps,
  syncPipelineInspectorVisibility,
  graphStateKey,
  shouldAutoRefreshGraph,
} from "./graph.js";

interface PipelineInspectorDependencies {
  selectPipeline: (pipelineId: string) => void;
  openOperateView: (pipelineId: string) => void;
}

const dependencies: PipelineInspectorDependencies = {
  selectPipeline: () => {},
  openOperateView: () => {},
};

let forceRuntimeScope = false;
let runtimeScopeMaskedPipelineId: string | null = null;
let summaryRequestSeq = 0;
let summaryInFlight: Promise<void> | null = null;
const pipelineSummaryCache = new Map<string, PipelineSummarySnapshot>();
let inspectOutputSearchQuery = "";
let inspectResourceDetailsExpanded = false;
let inspectProbeDetailsExpanded = false;
let inspectPresentationCallback:
  | ((model: PipelineInspectCheckpointModel | null) => void)
  | null = null;
const pipelineInspectorScope = new RenderScope("inspect-mode-content");

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

export function setPipelineInspectorContainerId(containerId: string): void {
  pipelineInspectorScope.setContainerId(containerId);
}

function ensurePipelineInspectorShell(container: HTMLElement): void {
  if (
    container.querySelector("#inspect-pipeline-select") ||
    document.getElementById("inspect-pipeline-select")
  )
    return;
  container.innerHTML = pipelineInspectorShellHtml();
}

function pipelineInspectorContainer(): HTMLElement | null {
  const root = document.getElementById(pipelineInspectorScope.current());
  if (root) return root;
  if (document.getElementById("inspect-pipeline-select")) return document.body;
  return pipelineInspectorScope.current() === "inspect-mode-content"
    ? document.getElementById("inspect-mode-panel")
    : null;
}

export function selectedPipeline(): PipelineView | null {
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

export function formatBytes(bytes: number | null | undefined): string {
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

export function pipelineInspectV2Active(): boolean {
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
      graphLabel: getGraphInFlight() ? "Loading resources" : "Runtime resources",
      focusLabel: "Inspection focus · select a pipeline to inspect diagnostics.",
      nextStep: "Select a pipeline to inspect graph edges and active probes.",
      canOpenPipeline: false,
      canRunDiagnostics: false,
      diagnosticsDisabledReason: "Select a pipeline first.",
      metrics: [
        { label: "Pipelines", value: String(state.pipelines.length) },
        {
          label: "Graph",
          value: getGraphInFlight() ? "Loading" : "Runtime",
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
  renderInspectCheckpointPresentation(pipe, invalidPipelineSelection);
  const root = pipelineInspectorContainer();
  if (!root) return;
  ensurePipelineInspectorShell(root);
  updateInspectRouteSummary(pipe, invalidPipelineSelection);
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
  if (!pipe && getGraphPipelineId() !== null) resetPipelineInspectorSelection(null);

  const openBtn = document.getElementById(
    "inspect-open-pipeline-btn",
  ) as HTMLButtonElement | null;
  if (openBtn) {
    openBtn.disabled = !pipe;
    openBtn.textContent = "Operate";
    openBtn.setAttribute(
      "aria-label",
      pipe ? `Operate ${pipe.name}` : "Operate selected pipeline",
    );
    openBtn.onclick = () => {
      if (pipe) dependencies.openOperateView(pipe.id);
    };
  }

  setPipelineOnlySectionsVisible(Boolean(pipe) || invalidPipelineSelection);
  renderSummary(pipe, invalidPipelineSelection);
  if (pipe) refreshPipelineSummary(pipe.id);
  renderInspectorResourceDetails(
    pipe,
    pipe ? getCachedResourceMap(pipe.id) || null : null,
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
    const autoRefresh = getGraphAutoRefresh();
    refreshBtn.textContent = autoRefresh ? "Stop Refresh" : "Auto Refresh";
    refreshBtn.setAttribute(
      "aria-label",
      autoRefresh ? "Stop graph auto refresh" : "Start graph auto refresh",
    );
    refreshBtn.classList.toggle("btn-accent", autoRefresh);
    refreshBtn.classList.toggle("btn-outline", !autoRefresh);
    refreshBtn.setAttribute(
      "aria-pressed",
      autoRefresh ? "true" : "false",
    );
    refreshBtn.onclick = () => {
      setGraphAutoRefresh(!getGraphAutoRefresh());
      renderPipelineInspector();
      if (getGraphAutoRefresh()) void refreshPipelineInspectorGraph();
    };
  }
  const diagnosticsBtn = document.getElementById(
    "inspect-open-diagnostics-btn",
  ) as HTMLButtonElement | null;
  if (diagnosticsBtn) {
    diagnosticsBtn.disabled = !pipe || pipe.input.status !== "on";
    diagnosticsBtn.textContent = "Run Diagnostics";
    diagnosticsBtn.setAttribute(
      "aria-label",
      pipe
        ? `Run diagnostics for ${pipe.name}`
        : "Run diagnostics for selected pipeline",
    );
    diagnosticsBtn.onclick = () => {
      if (pipe) openDiagnosticsModal(pipe.id);
    };
  }

  if (
    pipe &&
    !getGraphInFlight() &&
    (getGraphPipelineId() !== pipe.id || getGraphRenderedStateKey() !== stateKey)
  ) {
    void refreshPipelineInspectorGraph();
  } else if (pipe && shouldAutoRefreshGraph()) {
    void refreshPipelineInspectorGraph();
  } else if (
    !pipe &&
    !invalidPipelineSelection &&
    !getGraphInFlight() &&
    (getGraphPipelineId() !== null || getGraphRenderedStateKey() !== "runtime")
  ) {
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
  inspectProbeDetailsExpanded = false;
  runtimeScopeMaskedPipelineId =
    pipelineId === null ? getUrlParam("p") || getGraphPipelineId() : null;
  resetGraphState(pipelineId);
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
  const blockerText = blockers.length
    ? blockers.map(escapeHtml).join("<br>")
    : "Ready for active diagnostics.";
  const faultText = faultCandidates.length
    ? faultCandidates.map((out) => escapeHtml(out.name)).join("<br>")
    : "No unexpected output failures.";
  if (focusSummary) {
    focusSummary.textContent = `Inspection focus · ${blockers.length ? `${pluralize(blockers.length, "blocker")} before active probes` : "ready for active probes"} · ${pluralize(faultCandidates.length, "fault candidate")} · ${suggestedNextStep}`;
  }

  if (pipelineInspectV2Active()) {
    const detailLabel = `${
      inspectProbeDetailsExpanded ? "Hide" : "Show"
    } probe details for ${pipe.name}`;
    container.innerHTML = `<section class="border-base-content/10 bg-base-100/35 rounded-lg border p-3">
        <div class="flex flex-wrap items-start justify-between gap-3">
          <div>
            <div class="dashboard-kicker">Probe plan</div>
            <div class="mt-1 text-sm">${escapeHtml(suggestedNextStep)}</div>
            <div class="text-base-content/55 mt-1 text-xs">${escapeHtml(pluralize(blockers.length, "blocker"))} · ${escapeHtml(pluralize(faultCandidates.length, "fault candidate"))}</div>
          </div>
          <button id="inspect-probe-details-toggle" type="button" class="btn btn-xs btn-outline" aria-label="${escapeHtml(detailLabel)}" aria-expanded="${inspectProbeDetailsExpanded ? "true" : "false"}">${inspectProbeDetailsExpanded ? "Hide probe details" : "Show probe details"}</button>
        </div>
        ${
          inspectProbeDetailsExpanded
            ? `<div class="mt-3 grid gap-3 md:grid-cols-2">
                <div class="dashboard-stat-card-compact">
                  <div class="dashboard-kicker">Probe Readiness</div>
                  <div class="mt-2 text-sm">${blockerText}</div>
                </div>
                <div class="dashboard-stat-card-compact">
                  <div class="dashboard-kicker">Fault Candidates</div>
                  <div class="mt-2 text-sm">${faultText}</div>
                </div>
              </div>`
            : ""
        }
      </section>`;
    document
      .getElementById("inspect-probe-details-toggle")
      ?.addEventListener("click", () => {
        inspectProbeDetailsExpanded = !inspectProbeDetailsExpanded;
        renderDiagnostics(pipe);
      });
    return;
  }

  container.innerHTML = `<div class="grid gap-3 md:grid-cols-3">
        <div class="dashboard-stat-card-compact">
            <div class="dashboard-kicker">Probe Readiness</div>
            <div class="mt-2 text-sm">${blockerText}</div>
        </div>
        <div class="dashboard-stat-card-compact">
            <div class="dashboard-kicker">Fault Candidates</div>
            <div class="mt-2 text-sm">${faultText}</div>
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

// Wire graph deps — the graph module calls several functions defined in this
// file; setGraphDeps breaks the circular dependency between index and graph.
setGraphDeps({
  selectedPipeline,
  hasInvalidPipelineSelection,
  withPreservedScroll,
  renderSummary,
  renderInspectorResourceDetails,
  renderInspectCheckpointPresentation,
  clearPipelineUrlSelection,
  pipelineInspectorScope,
});

// Re-export symbols that graph.ts owns but that callers might reach through
// index (even though no current consumer does — kept for consistency / safety).
export {
  refreshPipelineInspectorGraph,
  renderGraphIntoShellSlot,
  syncPipelineInspectorVisibility,
} from "./graph.js";


