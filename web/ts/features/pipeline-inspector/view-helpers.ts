import { escapeHtml, getUrlParam } from "../../core/utils.js";
import { state } from "../../core/state.js";
import type { ResourceMapSnapshot } from "../../core/api.js";
import type { OutputView, PipelineView } from "../../types.js";
import { fetchProcessingGraph, renderGraphInto } from "../graph.js";
import {
  isOutputFlapping,
  isOutputRetrying,
  isOutputUnexpectedlyDown,
} from "../../core/output-status.js";

export function renderGraphIntoShellSlot(
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

let forceRuntimeScope = false;
let runtimeScopeMaskedPipelineId: string | null = null;

export function setForceRuntimeScope(force: boolean, maskedId: string | null = null): void {
  forceRuntimeScope = force;
  runtimeScopeMaskedPipelineId = maskedId;
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

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) return "--";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
}

export function formatPercentage(pct: number | null | undefined): string {
  if (pct === null || pct === undefined || !Number.isFinite(pct)) return "--";
  return `${pct.toFixed(1)}%`;
}

export function pipelineInspectV2Active(): boolean {
  const toggle = document.getElementById("dashboard-ui-v2-toggle");
  if (toggle instanceof HTMLInputElement && toggle.checked) return true;
  try {
    return new URLSearchParams(window.location.search).get("ui") === "v2";
  } catch {
    return false;
  }
}

export function inspectFaultCandidates(pipe: PipelineView): OutputView[] {
  return pipe.outs.filter(
    (output) =>
      isOutputUnexpectedlyDown(output) ||
      isOutputRetrying(output) ||
      isOutputFlapping(output),
  );
}

export function inspectProbeBlockers(pipe: PipelineView): string[] {
  const blockers: string[] = [];
  if (pipe.input.status !== "on")
    blockers.push("Input must be online for active probes.");
  if (!pipe.input.publisher?.protocol)
    blockers.push("Publisher protocol is not known yet.");
  return blockers;
}

export function inspectSuggestedNextStep(pipe: PipelineView): string {
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

export function resourceSummaryGroupsHtml(
  snapshot: ResourceMapSnapshot,
): string {
  const nodeCount = snapshot.nodes.length;
  const sharedNodes = snapshot.nodes.filter((node) => ((node as any).pipelines || [(node as any).pipelineId]).length > 1);

  return `<div class="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
    <div class="dashboard-stat-card-compact">
      <div class="dashboard-kicker">Worker Threads</div>
      <div class="mt-1 text-sm font-semibold">${nodeCount} node${nodeCount === 1 ? "" : "s"}</div>
    </div>
    <div class="dashboard-stat-card-compact">
      <div class="dashboard-kicker">Shared Processes</div>
      <div class="mt-1 text-sm font-semibold">${sharedNodes.length} shared</div>
    </div>
    <div class="dashboard-stat-card-compact">
      <div class="dashboard-kicker">Attributed Memory</div>
      <div class="mt-1 text-sm font-semibold">${formatBytes((snapshot as any).totals?.rssBytes || 0)}</div>
    </div>
    <div class="dashboard-stat-card-compact">
      <div class="dashboard-kicker">Attributed CPU</div>
      <div class="mt-1 text-sm font-semibold">${formatPercentage((snapshot as any).totals?.cpuPercent || 0)}</div>
    </div>
  </div>`;
}

export function resourceDetailPanelHtml(
  snapshot: ResourceMapSnapshot,
  pipe: PipelineView,
): string {
  const nodes = snapshot.nodes.filter((node) =>
    ((node as any).pipelines || [(node as any).pipelineId]).includes(pipe.id),
  );
  if (nodes.length === 0) {
    return `<div class="text-base-content/60 text-sm">No dedicated worker resources currently mapped to ${escapeHtml(pipe.name)}.</div>`;
  }
  return `<div class="overflow-x-auto rounded-lg border border-base-content/10">
    <table class="table table-xs w-full">
      <thead>
        <tr>
          <th>Process</th>
          <th>Role</th>
          <th>PID</th>
          <th>CPU</th>
          <th>RSS</th>
        </tr>
      </thead>
      <tbody>
        ${nodes
          .map(
            (node: any) => `<tr>
            <td class="font-mono">${escapeHtml(node.processName || node.name || "--")}</td>
            <td>${escapeHtml(node.role || "--")}</td>
            <td class="font-mono">${node.pid ?? "--"}</td>
            <td class="font-mono">${formatPercentage(node.cpuPercent)}</td>
            <td class="font-mono">${formatBytes(node.rssBytes)}</td>
          </tr>`,
          )
          .join("")}
      </tbody>
    </table>
  </div>`;
}
