import { outputViewEncodingLabel } from "../core/output-config.js";
import { state } from "../core/state.js";
import { escapeHtml, getUrlParam, sanitizeLogMessage } from "../core/utils.js";
import type { OutputView, PipelineView } from "../types.js";
import { openDiagnosticsModal } from "./diagnostics.js";
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

export function setPipelineInspectorDependencies(
  next: Partial<PipelineInspectorDependencies>,
): void {
  Object.assign(dependencies, next || {});
}

function selectedPipeline(): PipelineView | null {
  const selectedId = getUrlParam("p");
  if (!selectedId) return null;
  return state.pipelines.find((pipeline) => pipeline.id === selectedId) || null;
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
  const stateKey = graphStateKey(pipe);
  const select = document.getElementById(
    "inspect-pipeline-select",
  ) as HTMLSelectElement | null;
  if (select) {
    select.innerHTML = state.pipelines
      .map(
        (pipeline) =>
          `<option value="${escapeHtml(pipeline.id)}">${escapeHtml(pipeline.name)}</option>`,
      )
      .join("");
    select.value = pipe?.id || "";
    select.onchange = () => {
      const pipelineId = select.value || "";
      if (!pipelineId) return;
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

  renderSummary(pipe);
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
  }
}

export function resetPipelineInspectorSelection(
  pipelineId: string | null,
): void {
  graphRequestSeq++;
  graphPipelineId = pipelineId;
  graphRenderedStateKey = null;
  const status = document.getElementById("inspect-graph-status");
  const container = document.getElementById("inspect-graph-container");
  if (status)
    status.textContent = pipelineId ? "Loading graph..." : "Select a pipeline.";
  if (container) {
    container.innerHTML = `<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">
            ${pipelineId ? "Loading graph..." : "Select a pipeline to inspect its graph."}
        </div>`;
  }
}

function renderSummary(pipe: PipelineView | null): void {
  const container = document.getElementById("inspect-pipeline-summary");
  if (!container) return;
  if (!pipe) {
    container.innerHTML =
      '<div class="text-base-content/60 text-sm">No pipeline selected.</div>';
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
                    <div class="text-base-content/60 truncate text-xs">${escapeHtml(encodingLabel)} / ${escapeHtml(sanitizeLogMessage(out.url, true))}</div>
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
  if (!pipe || !container) return;
  const requestPipelineId = pipe.id;
  const requestSeq = ++graphRequestSeq;
  graphPipelineId = requestPipelineId;
  if (status) status.textContent = "Loading graph...";
  container.innerHTML = `<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">
        Loading graph...
    </div>`;
  graphInFlight = (async () => {
    const graph = await fetchProcessingGraph(requestPipelineId);
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
