import { state } from "../../core/state.js";
import { escapeHtml } from "../../core/utils.js";
import type { PipelineView } from "../../types.js";
import type { ResourceMapNode, ResourceMapSnapshot } from "../../core/api-types.js";
import type { renderGraphInto } from "../graph.js";
import {
  formatBytes,
  pipelineInspectV2Active,
  renderGraphIntoShellSlot,
  selectedPipeline,
} from "./index.js";

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

export function resourceSummaryGroupsHtml(snapshot: ResourceMapSnapshot): string {
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
  const subtitleHtml = pipelineInspectV2Active()
    ? ""
    : `<div class="text-base-content/55 text-xs">${escapeHtml(subtitle)}</div>`;
  return `<section class="dashboard-section bg-base-100/35 p-3">
    <div class="mb-2">
      <div class="text-sm font-semibold">${escapeHtml(title)}</div>
      ${subtitleHtml}
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

export function resourceDetailPanelHtml(
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

export function renderResourceMapInto(
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
