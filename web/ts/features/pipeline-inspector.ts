import { outputViewEncodingLabel } from "../core/output-config.js";
import { state } from "../core/state.js";
import { escapeHtml, escapeRedactedHtml, getUrlParam } from "../core/utils.js";
import type { OutputView, PipelineView } from "../types.js";
import { openDiagnosticsModal } from "./diagnostics.js";
import { getResourceMap } from "../core/api.js";
import type { ResourceMapNode, ResourceMapSnapshot } from "../core/api.js";
import { fetchProcessingGraph, renderGraphInto } from "./graph.js";
import {
  isOutputFlapping,
  isOutputIntentStopped,
  isOutputRunning,
  isOutputRetrying,
  isOutputUnexpectedlyDown,
} from "../core/output-status.js";

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

const RUNTIME_SCOPE_VALUE = "__runtime";
const RESOURCE_MAP_TOP_N = 25;
const PROCESSING_GRAPH_OUTPUT_LIMIT = 50;

export function setPipelineInspectorDependencies(
  next: Partial<PipelineInspectorDependencies>,
): void {
  Object.assign(dependencies, next || {});
}

function selectedPipeline(): PipelineView | null {
  const urlPipelineId = getUrlParam("p");
  if (urlPipelineId) forceRuntimeScope = false;
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

export function renderPipelineInspector(): void {
  const pipe = selectedPipeline();
  const invalidPipelineSelection = hasInvalidPipelineSelection();
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
        forceRuntimeScope = true;
        clearPipelineUrlSelection();
        resetPipelineInspectorSelection(null);
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

  renderSummary(pipe, invalidPipelineSelection);
  renderDiagnostics(pipe);

  const refreshBtn = document.getElementById(
    "inspect-refresh-graph-btn",
  ) as HTMLButtonElement | null;
  if (refreshBtn) {
    refreshBtn.textContent = graphAutoRefresh ? "Stop Refresh" : "Auto Refresh";
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
  } else if (pipe && graphAutoRefresh && !document.hidden) {
    void refreshPipelineInspectorGraph();
  } else if (
    !pipe &&
    !invalidPipelineSelection &&
    graphAutoRefresh &&
    !document.hidden
  ) {
    void refreshPipelineInspectorGraph();
  }
}

export function resetPipelineInspectorSelection(
  pipelineId: string | null,
): void {
  forceRuntimeScope = pipelineId === null;
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
    container.innerHTML = `<div class="bg-base-100 rounded-lg p-3">
        <div class="text-base-content/60 text-xs font-semibold uppercase">Scope</div>
        <div class="mt-2 text-sm">Whole Runtime</div>
      </div>`;
    return;
  }

  const health = pipelineHealthLabel(pipe);
  const outputs = pipe.outs
    .map((out) => {
      const stateLabel = outputStateLabel(out);
      const encodingLabel = outputViewEncodingLabel(out);
      return `<div class="flex items-center justify-between gap-2 border-base-content/10 border-t py-2">
                <div class="min-w-0">
                    <div class="truncate text-sm font-medium">${escapeHtml(out.name)}</div>
                    <div class="text-base-content/60 truncate text-xs">${escapeHtml(encodingLabel)} / ${escapeRedactedHtml(out.url, true)}</div>
                </div>
                <span class="badge ${stateLabel.cls} shrink-0">${stateLabel.label}</span>
            </div>`;
    })
    .join("");

  container.innerHTML = `<section class="border-base-content/10 bg-base-200 rounded-lg border p-3">
        <div class="mb-2 flex min-w-0 items-start justify-between gap-2">
            <h2 class="min-w-0 truncate font-semibold">${escapeHtml(pipe.name)}</h2>
            <span class="badge ${health.cls} shrink-0 whitespace-nowrap">${health.label}</span>
        </div>
        <dl class="grid grid-cols-2 gap-2 text-sm">
            <div><dt class="text-base-content/60">Input</dt><dd>${escapeHtml(pipe.input.status)}</dd></div>
            <div><dt class="text-base-content/60">Publisher</dt><dd>${escapeHtml(pipe.input.publisher?.protocol || "--")}</dd></div>
            <div><dt class="text-base-content/60">Input Rate</dt><dd>${formatBitrate(pipe.stats.inputBitrateKbps)}</dd></div>
            <div><dt class="text-base-content/60">Output Rate</dt><dd>${formatBitrate(pipe.stats.outputBitrateKbps)}</dd></div>
            <div><dt class="text-base-content/60">Received</dt><dd>${formatBytes(pipe.input.bytesReceived)}</dd></div>
            <div><dt class="text-base-content/60">Sent</dt><dd>${formatBytes(pipe.input.bytesSent)}</dd></div>
        </dl>
        <div class="mt-3">${outputs || '<div class="text-base-content/60 text-sm">No outputs configured.</div>'}</div>
    </section>`;
}

function renderDiagnostics(pipe: PipelineView | null): void {
  const container = document.getElementById("inspect-diagnostics-summary");
  if (!container) return;
  if (!pipe) {
    container.innerHTML =
      '<div class="text-base-content/60 text-sm">Select a pipeline to inspect diagnostics.</div>';
    return;
  }

  const blockers: string[] = [];
  if (pipe.input.status !== "on")
    blockers.push("Input must be online for active probes.");
  if (!pipe.input.publisher?.protocol)
    blockers.push("Publisher protocol is not known yet.");
  const downOutputs = pipe.outs.filter(isOutputUnexpectedlyDown);
  const retryingOutputs = pipe.outs.filter(isOutputRetrying);
  const flappingOutputs = pipe.outs.filter(isOutputFlapping);
  const faultCandidates = [
    ...downOutputs,
    ...retryingOutputs,
    ...flappingOutputs,
  ];

  container.innerHTML = `<div class="grid gap-3 md:grid-cols-3">
        <div class="bg-base-100 rounded-lg p-3">
            <div class="text-base-content/60 text-xs font-semibold uppercase">Probe Readiness</div>
            <div class="mt-2 text-sm">${blockers.length ? blockers.map(escapeHtml).join("<br>") : "Ready for active diagnostics."}</div>
        </div>
        <div class="bg-base-100 rounded-lg p-3">
            <div class="text-base-content/60 text-xs font-semibold uppercase">Fault Candidates</div>
            <div class="mt-2 text-sm">${faultCandidates.length ? faultCandidates.map((out) => escapeHtml(out.name)).join("<br>") : "No unexpected output failures."}</div>
        </div>
        <div class="bg-base-100 rounded-lg p-3">
            <div class="text-base-content/60 text-xs font-semibold uppercase">Suggested Next Step</div>
            <div class="mt-2 text-sm">${pipe.input.status === "on" ? (retryingOutputs.length ? "Inspect recent errors and retry backoff before forcing a restart." : flappingOutputs.length ? "Inspect recent sink failures before forcing a restart." : "Run diagnostics, then inspect graph edges with zero packet output.") : "Start or reconnect the publisher before probing."}</div>
        </div>
    </div>`;
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
    container.innerHTML =
      '<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">Select a pipeline to inspect its graph.</div>';
    return;
  }
  if (!pipe) {
    graphPipelineId = null;
    if (status) status.textContent = "Loading runtime resources...";
    container.innerHTML = `<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">
        Loading runtime resources...
    </div>`;
    graphInFlight = (async () => {
      const resourceMap = await getResourceMap(null, {
        view: "grouped",
        topN: RESOURCE_MAP_TOP_N,
      });
      if (requestSeq !== graphRequestSeq || selectedPipeline()) return;
      if (!resourceMap) {
        if (status) status.textContent = "Runtime resources unavailable.";
        container.innerHTML =
          '<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">Runtime resources unavailable.</div>';
        return;
      }
      renderResourceMapInto(container, resourceMap);
      graphRenderedStateKey = "runtime";
      if (status) status.textContent = "Whole Runtime / resource map";
    })();
    try {
      await graphInFlight;
    } finally {
      if (requestSeq === graphRequestSeq) graphInFlight = null;
    }
    return;
  }
  const requestPipelineId = pipe.id;
  graphPipelineId = requestPipelineId;
  if (pipe.outs.length > PROCESSING_GRAPH_OUTPUT_LIMIT) {
    if (status) status.textContent = "Loading grouped resource map...";
    container.innerHTML = `<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">
        Loading grouped resource map...
    </div>`;
    graphInFlight = (async () => {
      const resourceMap = await getResourceMap(requestPipelineId, {
        view: "grouped",
        topN: RESOURCE_MAP_TOP_N,
      });
      if (
        requestSeq !== graphRequestSeq ||
        selectedPipeline()?.id !== requestPipelineId
      ) {
        return;
      }
      if (!resourceMap) {
        if (status) status.textContent = "Resource map unavailable.";
        container.innerHTML =
          '<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">Resource map unavailable.</div>';
        return;
      }
      renderResourceMapInto(container, resourceMap);
      graphRenderedStateKey = requestStateKey;
      if (status) {
        status.textContent = `${pipe.name} / grouped resources / ${pipe.outs.length} outputs`;
      }
    })();
    try {
      await graphInFlight;
    } finally {
      if (requestSeq === graphRequestSeq) graphInFlight = null;
    }
    return;
  }
  if (status) status.textContent = "Loading graph...";
  container.innerHTML = `<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">
        Loading graph...
    </div>`;
  graphInFlight = (async () => {
    const [graph, resourceMap] = await Promise.all([
      fetchProcessingGraph(requestPipelineId),
      getResourceMap(requestPipelineId, {
        view: "grouped",
        topN: RESOURCE_MAP_TOP_N,
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
      container.innerHTML =
        '<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">Graph unavailable.</div>';
      return;
    }
    renderGraphInto(container, graph as Parameters<typeof renderGraphInto>[1]);
    if (resourceMap) {
      container.innerHTML = `${resourceSummaryHtml(resourceMap)}${container.innerHTML}`;
    }
    graphRenderedStateKey = requestStateKey;
    if (status) {
      const nodeCount = (graph as { nodes?: unknown[] }).nodes?.length || 0;
      const inputState =
        pipe.input.status === "on" ? "live" : pipe.input.status;
      status.textContent = `${pipe.name} / ${nodeCount} nodes / input ${inputState}`;
    }
  })();
  try {
    await graphInFlight;
  } finally {
    if (requestSeq === graphRequestSeq) graphInFlight = null;
  }
}

function resourceSummaryHtml(snapshot: ResourceMapSnapshot): string {
  const scopeKind = snapshot.scope?.kind || "runtime";
  const cards = resourceSummaryCards(snapshot.summary || {}, scopeKind).slice(
    0,
    4,
  );
  return `<div class="space-y-2 p-3">
    <div class="grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
      ${cards
        .map(
          ({
            label,
            value,
            confidence,
          }) => `<div class="bg-base-200 rounded-lg p-2">
          <div class="flex items-center justify-between gap-2">
            <div class="text-base-content/60 text-[0.65rem] font-semibold uppercase">${escapeHtml(label)}</div>
            <span class="text-base-content/50 text-[0.65rem]">${escapeHtml(confidence)}</span>
          </div>
          <div class="font-mono text-sm">${escapeHtml(value)}</div>
        </div>`,
        )
        .join("")}
    </div>
    ${resourceLimitNoticeHtml(snapshot)}
    ${resourceAccuracyLegendHtml("compact")}
  </div>`;
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
  const summary = snapshot.summary || {};
  const scopeKind = snapshot.scope?.kind || "runtime";
  const nodes = [...(snapshot.nodes || [])]
    .sort((a, b) => resourceNodeScore(b) - resourceNodeScore(a))
    .slice(0, 12);
  const cards = resourceSummaryCards(summary, scopeKind);
  container.innerHTML = `<div class="space-y-4 p-3">
    <section class="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
      ${cards
        .map(
          ({
            label,
            value,
            confidence,
          }) => `<div class="bg-base-200 rounded-lg p-3">
            <div class="flex items-center justify-between gap-2">
              <div class="text-base-content/60 text-xs font-semibold uppercase">${escapeHtml(label)}</div>
              <span class="text-base-content/50 text-xs">${escapeHtml(confidence)}</span>
            </div>
            <div class="mt-1 font-mono text-lg">${escapeHtml(value)}</div>
          </div>`,
        )
        .join("")}
    </section>
    <section class="bg-base-200 rounded-lg p-3">
      <div class="mb-2 flex items-center justify-between gap-2">
        <h3 class="text-sm font-semibold">${scopeKind === "pipeline" ? "Pipeline Resource Graph" : "Runtime Resource Graph"}</h3>
        <span class="text-base-content/50 text-xs">${escapeHtml(scopeKind)}</span>
      </div>
      <div id="runtime-resource-graph" class="bg-base-100 min-h-72 overflow-auto rounded-lg"></div>
    </section>
    ${resourceAccuracyLegendHtml("full")}
    ${resourceLimitNoticeHtml(snapshot)}
    <section>
      <div class="mb-2 flex items-center justify-between gap-2">
        <h3 class="text-sm font-semibold">Top Resource Nodes</h3>
        <span class="text-base-content/50 text-xs">${escapeHtml(scopeKind)}</span>
      </div>
      <div class="overflow-auto">
        <table class="table table-sm">
          <thead><tr><th>Node</th><th>Execution</th><th>Memory</th><th>Threads</th><th>Signals</th></tr></thead>
          <tbody>
            ${
              nodes.length
                ? nodes
                    .map(
                      (node) => `<tr>
                        <td><div class="font-medium">${escapeHtml(node.label || node.id)}</div><div class="text-base-content/50 max-w-80 truncate text-xs">${escapeHtml(node.id)}</div></td>
                        <td>${escapeHtml(node.execution || "--")}</td>
                        <td>${formatBytes(node.memory?.attributedBytes ?? null)} <span class="text-base-content/50 text-xs">${escapeHtml(node.memory?.confidence || "")}</span></td>
                        <td>${escapeHtml(formatThreadCell(node))}</td>
                        <td>${escapeHtml((node.hotspots || []).join(", ") || "--")}</td>
                      </tr>`,
                    )
                    .join("")
                : `<tr><td colspan="5" class="text-base-content/60">No active resource nodes.</td></tr>`
            }
          </tbody>
        </table>
      </div>
    </section>
  </div>`;
  renderRuntimeResourceGraph(container, snapshot, nodes);
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
  confidence: "measured" | "derived" | "estimated";
};

function resourceSummaryCards(
  summary: ResourceMapSnapshot["summary"],
  scopeKind: string,
): ResourceSummaryCard[] {
  const scoped = scopeKind === "pipeline";
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
      value: `${summaryNumber(summary, "srtSenderThreads") ?? "--"} / ${
        summaryNumber(summary, "srtSenderThreadLimit") ?? "--"
      }`,
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

function resourceAccuracyLegendHtml(mode: "compact" | "full"): string {
  const wrapperClass =
    mode === "compact"
      ? "text-base-content/60 text-xs"
      : "border-base-content/10 bg-base-200 rounded-lg border p-3 text-xs";
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
  if (!graphContainer) return;
  const graphNodes = nodes.slice(0, 8).map((node) => ({
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
    graphContainer.innerHTML =
      '<div class="text-base-content/60 flex min-h-72 items-center justify-center text-sm">No active resource nodes.</div>';
    return;
  }

  const graphNodeIds = new Set(graphNodes.map((node) => node.id));
  const rootId = graphNodeIds.has("runtime:restream")
    ? "runtime:restream"
    : graphNodes[0].id;
  const ffmpegId = graphNodeIds.has("runtime:external-ffmpeg")
    ? "runtime:external-ffmpeg"
    : null;
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

  renderGraphInto(graphContainer, {
    pipelineId: String(snapshot.scope?.pipelineId || "runtime"),
    nodes: graphNodes,
    edges,
  } as Parameters<typeof renderGraphInto>[1]);
}

function resourceGraphNodeType(node: ResourceMapNode): string {
  const kind = String(node.kind || "");
  if (kind.includes("ingest")) return "ingest";
  if (kind.includes("egress")) return "egress";
  if (kind.includes("stage") || node.execution === "child_process")
    return "transcoder";
  if (kind.includes("ring") || kind.includes("pipeline")) return "ring_buffer";
  if (kind.includes("process")) return "packetizer";
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
    pipe.hlsPreview?.segments || 0,
    outputs,
  ].join("::");
}

export function syncPipelineInspectorVisibility(): void {
  if (graphAutoRefresh) void refreshPipelineInspectorGraph();
}
