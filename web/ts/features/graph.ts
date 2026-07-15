import { getProcessingGraph } from "../core/api.js";

interface GraphNode {
  id: string;
  type: string;
  label: string;
  active: boolean;
  stageKey?: string;
  details?: Record<string, unknown>;
  metrics?: {
    packetsIn: number;
    packetsOut: number;
    bytesIn: number;
    bytesOut: number;
    processingUs: number;
    avgUsPerPacket: number;
    packetsPerSec: number;
    uptimeSec?: number;
    uptimeSecs?: number;
  };
}

interface GraphEdge {
  from: string;
  to: string;
  label: string;
}

interface GraphData {
  pipelineId: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export async function fetchProcessingGraph(
  pipeId: string,
): Promise<GraphData | null> {
  const data = (await getProcessingGraph(pipeId)) as GraphData | null;
  return data;
}

function formatBytes(value: unknown): string {
  const b = Number(value);
  if (!Number.isFinite(b) || b < 0) return "--";
  if (b < 1024) return `${b.toFixed(0)} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KiB`;
  if (b < 1024 * 1024 * 1024) return `${(b / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(b / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
}

function formatRate(value: unknown): string {
  const pps = Number(value);
  if (!Number.isFinite(pps) || pps < 0) return "--";
  if (pps < 1000) return `${pps.toFixed(0)}/s`;
  return `${(pps / 1000).toFixed(1)}k/s`;
}

function formatKbps(value: unknown): string {
  const kbps = Number(value);
  if (!Number.isFinite(kbps) || kbps < 0) return "--";
  if (kbps >= 1000) return `${(kbps / 1000).toFixed(1)} Mbps`;
  return `${kbps.toFixed(0)} kbps`;
}

function formatAgeMs(value: unknown): string {
  const ms = Number(value);
  if (!Number.isFinite(ms) || ms < 0) return "no progress";
  if (ms < 1000) return `${ms.toFixed(0)} ms ago`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s ago`;
  return `${(ms / 60_000).toFixed(1)} min ago`;
}

function formatDurationMs(value: unknown): string {
  const ms = Number(value);
  if (!Number.isFinite(ms) || ms < 0) return "--";
  if (ms < 1000) return `${ms.toFixed(0)} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
  return `${(ms / 60_000).toFixed(1)} min`;
}

function finiteNumber(value: unknown, fallback = 0): number {
  const n = Number(value);
  return Number.isFinite(n) ? n : fallback;
}

const NODE_W = 220;
const NODE_H = 80;
const METRICS_H = 50;
const COL_GAP = 80;
const ROW_GAP = 30;
const MIN_CANVAS_W = 1680;
const REPEATED_LEAF_GROUP_MIN = 4;
let focusedAggregateGroupKey: string | null = null;

interface LeafGroup {
  edge: GraphEdge;
  nodes: GraphNode[];
}

function nodeColor(type: string, active: boolean): string {
  if (!active) return "#6b7280";
  switch (type) {
    case "ingest":
      return "#10b981";
    case "demux":
      return "#14b8a6";
    case "ring_buffer":
      return "#6366f1";
    case "transcoder":
      return "#f59e0b";
    case "audio_filter":
      return "#f97316";
    case "codec_edge":
      return "#ec4899";
    case "egress":
      return "#3b82f6";
    case "packetizer":
      return "#06b6d4";
    case "recording":
      return "#ef4444";
    case "hls":
      return "#8b5cf6";
    default:
      return "#9ca3af";
  }
}

function nodeHealthTone(node: GraphNode): {
  color: string;
  dot: string;
  label: "healthy" | "warning" | "error" | "idle";
} {
  const status = String(node.details?.status || "").toLowerCase();
  const healthStatus = String(node.details?.healthStatus || "").toLowerCase();
  const flapping = node.details?.flapping === true;
  const overflowCount = finiteNumber(node.details?.overflowCount);
  if (
    status === "failed" ||
    status === "error" ||
    status === "stopped" ||
    (!node.active && !status)
  ) {
    return { color: "#ef4444", dot: "#ef4444", label: "error" };
  }
  if (
    healthStatus === "warning" ||
    status === "stalled" ||
    status === "retrying" ||
    flapping ||
    overflowCount > 0
  ) {
    return { color: "#f59e0b", dot: "#f59e0b", label: "warning" };
  }
  if (!node.active) return { color: "#6b7280", dot: "#6b7280", label: "idle" };
  return { color: nodeColor(node.type, true), dot: "#22c55e", label: "healthy" };
}

export function renderGraphInto(container: HTMLElement, data: GraphData): void {
  const sourceData = data;
  data = aggregateRepeatedLeaves(sourceData);

  // Build adjacency for layout
  const childrenOf = new Map<string, string[]>();
  const parentOf = new Map<string, string[]>();
  const nodeMap = new Map<string, GraphNode>();
  for (const n of data.nodes) nodeMap.set(n.id, n);
  for (const e of data.edges) {
    if (!childrenOf.has(e.from)) childrenOf.set(e.from, []);
    childrenOf.get(e.from)!.push(e.to);
    if (!parentOf.has(e.to)) parentOf.set(e.to, []);
    parentOf.get(e.to)!.push(e.from);
  }

  // BFS layering from roots (nodes with no parents)
  const roots = data.nodes.filter(
    (n) => !parentOf.has(n.id) || parentOf.get(n.id)!.length === 0,
  );
  const layer = new Map<string, number>();
  const queue: string[] = [];
  for (const r of roots) {
    layer.set(r.id, 0);
    queue.push(r.id);
  }
  while (queue.length > 0) {
    const cur = queue.shift()!;
    const curLayer = layer.get(cur)!;
    for (const child of childrenOf.get(cur) || []) {
      const existing = layer.get(child) ?? -1;
      if (curLayer + 1 > existing) {
        layer.set(child, curLayer + 1);
        queue.push(child);
      }
    }
  }
  // Nodes not reached by BFS (orphans)
  for (const n of data.nodes) {
    if (!layer.has(n.id)) layer.set(n.id, 0);
  }

  // Group by column
  const columns = new Map<number, string[]>();
  for (const [id, col] of layer) {
    if (!columns.has(col)) columns.set(col, []);
    columns.get(col)!.push(id);
  }

  const maxCol = Math.max(...columns.keys(), 0);
  const positions = new Map<string, { x: number; y: number }>();

  for (let col = 0; col <= maxCol; col++) {
    const ids = columns.get(col) || [];
    const nodeH = NODE_H + METRICS_H;
    const totalH = ids.length * nodeH + (ids.length - 1) * ROW_GAP;
    const startY = 20;
    const x = 20 + col * (NODE_W + COL_GAP);
    for (let i = 0; i < ids.length; i++) {
      positions.set(ids[i], { x, y: startY + i * (nodeH + ROW_GAP) });
    }
  }

  const totalNodeH = NODE_H + METRICS_H;
  const maxNodesInCol = Math.max(
    ...[...columns.values()].map((c) => c.length),
    1,
  );
  const svgW = Math.max(
    MIN_CANVAS_W,
    40 + (maxCol + 1) * (NODE_W + COL_GAP),
  );
  const svgH = 40 + maxNodesInCol * (totalNodeH + ROW_GAP);

  let svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${svgW} ${svgH}" class="h-full w-full min-w-[900px]">`;
  svg += `<defs><marker id="arrowhead" markerWidth="10" markerHeight="7" refX="10" refY="3.5" orient="auto"><polygon points="0 0, 10 3.5, 0 7" fill="#9ca3af"/></marker></defs>`;

  // Draw edges
  for (const edge of data.edges) {
    const from = positions.get(edge.from);
    const to = positions.get(edge.to);
    if (!from || !to) continue;
    const x1 = from.x + NODE_W;
    const y1 = from.y + totalNodeH / 2;
    const x2 = to.x;
    const y2 = to.y + totalNodeH / 2;
    const mx = (x1 + x2) / 2;
    svg += `<path d="M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2},${y2}" fill="none" stroke="#6b7280" stroke-width="1.5" marker-end="url(#arrowhead)"/>`;
    // Edge label
    const lx = mx;
    const ly = (y1 + y2) / 2 - 6;
    svg += `<text x="${lx}" y="${ly}" text-anchor="middle" fill="#9ca3af" font-size="10">${escapeXml(edge.label)}</text>`;
  }

  // Draw nodes
  for (const node of data.nodes) {
    const pos = positions.get(node.id);
    if (!pos) continue;
    const color = nodeColor(node.type, node.active);
    const healthTone = nodeHealthTone(node);
    const opacity = node.active || healthTone.label === "warning" ? "1" : "0.5";
    const aggregateKey =
      node.details?.aggregate === true
        ? String(node.details.aggregateKey || "")
        : "";

    // Node box
    svg += `<g opacity="${opacity}"${aggregateKey ? ` data-graph-aggregate-key="${escapeXml(aggregateKey)}" style="cursor:pointer"` : ""}>`;
    svg += `<rect x="${pos.x}" y="${pos.y}" width="${NODE_W}" height="${totalNodeH}" rx="8" fill="#1f2937" stroke="${healthTone.color}" stroke-width="2"/>`;

    // Type badge
    svg += `<rect x="${pos.x}" y="${pos.y}" width="${NODE_W}" height="22" rx="8" fill="${color}" opacity="0.15"/>`;
    svg += `<rect x="${pos.x}" y="${pos.y + 14}" width="${NODE_W}" height="8" fill="${color}" opacity="0.15"/>`;
    svg += `<text x="${pos.x + 10}" y="${pos.y + 16}" fill="${color}" font-size="11" font-weight="600">${escapeXml(node.type.toUpperCase())}</text>`;

    // Label
    svg += `<text x="${pos.x + 10}" y="${pos.y + 40}" fill="#e5e7eb" font-size="13" font-weight="500">${escapeXml(truncate(node.label, 28))}</text>`;

    // Status dot
    const dotColor = healthTone.dot;
    svg += `<circle cx="${pos.x + NODE_W - 14}" cy="${pos.y + 40}" r="5" fill="${dotColor}"/>`;

    const detailLines: string[] = [];
    if (node.details?.resource === true) {
      detailLines.push(`cpu: ${String(node.details.cpu || "--")}`);
      detailLines.push(`memory: ${String(node.details.memory || "--")}`);
      detailLines.push(`threads: ${String(node.details.threads || "--")}`);
    } else if (node.type === "ring_buffer" && node.details) {
      const fill = finiteNumber(node.details.fill);
      const capacity = finiteNumber(node.details.capacity);
      const readers = Array.isArray(node.details.readers)
        ? node.details.readers
        : [];
      const maxLag = readers.reduce((max, reader) => {
        const lag = finiteNumber((reader as Record<string, unknown>).lagSlots);
        return Math.max(max, lag);
      }, 0);
      detailLines.push(`lag: ${maxLag}/${capacity} slots`);
      detailLines.push(`fill: ${fill}/${capacity} now`);
    } else if (
      node.type === "ingest" &&
      node.details?.bytesReceived !== undefined
    ) {
      detailLines.push(`in: ${formatKbps(node.details.bitrateKbps)}`);
      detailLines.push(
        `received: ${formatBytes(node.details.bytesReceived as number)}`,
      );
      if (node.details.lastProgressAgeMs !== undefined) {
        detailLines.push(`progress: ${formatAgeMs(node.details.lastProgressAgeMs)}`);
      }
      if (node.details.srtRecvBufferPercent !== undefined) {
        detailLines.push(
          `srt recv: ${Number(node.details.srtRecvBufferPercent).toFixed(0)}%`,
        );
      }
    } else if (node.details?.aggregate === true) {
      const count = finiteNumber(node.details.count);
      const activeCount = finiteNumber(node.details.activeCount);
      const itemLabel = String(node.details.itemLabel || "nodes");
      const activityLabel =
        node.type === "egress" ? "running" : "active";
      detailLines.push(`${count} ${itemLabel}`);
      detailLines.push(`${activeCount}/${count} ${activityLabel}`);
      detailLines.push("click to expand");
      if (node.type !== "egress") {
        const phase = String(node.details.phase || "");
        const backend = String(node.details.backend || "");
        const status = String(node.details.status || "");
        detailLines.push(
          [status, phase, backend].filter(Boolean).join(" | ") || "grouped",
        );
      }
    } else if (node.type === "egress" && node.details) {
      const status = String(
        node.details.status || (node.active ? "running" : "inactive"),
      );
      const phase = String(node.details.phase || "unknown");
      detailLines.push(`${status} | ${phase}`);
      detailLines.push(
        `${formatKbps(node.details.bitrateKbps)} | ${formatBytes(node.details.totalSize)}`,
      );
      const lastError =
        typeof node.details.lastError === "string"
          ? node.details.lastError
          : "";
      if (lastError) {
        detailLines.push(`error: ${truncate(lastError, 30)}`);
      } else if (node.details.lastProgressAgeMs !== undefined) {
        detailLines.push(
          `progress: ${formatAgeMs(node.details.lastProgressAgeMs)}`,
        );
      }
    } else if (node.stageKey && node.details) {
      const phase = String(
        node.details.phase || (node.active ? "active" : "inactive"),
      );
      const backend = String(node.details.backend || "");
      detailLines.push(backend ? `${phase} | ${backend}` : phase);
      const waitMs = finiteNumber(node.details.capacityWaitMs);
      if (waitMs > 0) {
        detailLines.push(`capacity wait: ${formatDurationMs(waitMs)}`);
      }
    }
    const healthReason =
      typeof node.details?.healthReason === "string"
        ? node.details.healthReason
        : "";
    if (healthReason) {
      detailLines.unshift(`warn: ${truncate(healthReason, 30)}`);
    }
    appendBranchDetails(detailLines, node);

    // Metrics
    if (
      node.metrics &&
      node.metrics.packetsIn > 0 &&
      node.details?.aggregate !== true &&
      node.details?.branchStart !== true
    ) {
      const m = node.metrics;
      const packetsPerSec = finiteNumber(m.packetsPerSec);
      const avgUsPerPacket = finiteNumber(m.avgUsPerPacket);
      const uptimeSecs = finiteNumber(m.uptimeSecs ?? m.uptimeSec);
      const bytesInPerSec =
        node.details?.bytesReceivedPerSec !== undefined
          ? finiteNumber(node.details.bytesReceivedPerSec)
          : uptimeSecs > 0
            ? finiteNumber(m.bytesIn) / uptimeSecs
            : 0;
      const bytesOutPerSec =
        uptimeSecs > 0 ? finiteNumber(m.bytesOut) / uptimeSecs : 0;
      const my = pos.y + NODE_H - 10;
      const inMetricLabel =
        node.type === "ingest"
          ? `in: ${formatBytes(bytesInPerSec)}/s`
          : `in: ${formatRate(packetsPerSec)} pkt | ${formatBytes(bytesInPerSec)}/s`;
      svg += `<text x="${pos.x + 10}" y="${my + 10}" fill="#9ca3af" font-size="10">${inMetricLabel}</text>`;
      const outLabel =
        finiteNumber(m.bytesOut) > 0
          ? `out: ${formatBytes(bytesOutPerSec)}/s`
          : node.type === "ingest"
            ? "fan-out: source buffer"
            : "out: n/a";
      const avgLabel =
        finiteNumber(m.processingUs) > 0
          ? `avg: ${avgUsPerPacket.toFixed(0)}us/pkt`
          : "avg: n/a";
      svg += `<text x="${pos.x + 10}" y="${my + 24}" fill="#9ca3af" font-size="10">${escapeXml(outLabel)} | ${escapeXml(avgLabel)}</text>`;
      svg += `<text x="${pos.x + 10}" y="${my + 38}" fill="#9ca3af" font-size="10">uptime: ${uptimeSecs.toFixed(0)}s</text>`;
    } else if (node.details) {
      const my = pos.y + NODE_H - 10;
      for (let i = 0; i < Math.min(detailLines.length, 3); i++) {
        svg += `<text x="${pos.x + 10}" y="${my + 10 + i * 14}" fill="#9ca3af" font-size="10">${escapeXml(detailLines[i])}</text>`;
      }
    }

    svg += `</g>`;
  }

  svg += `</svg>`;
  container.innerHTML = `<div class="flex h-full min-h-[420px] flex-col gap-2">${graphToolbarHtml(data, sourceData)}<div id="processing-graph-canvas" class="min-h-0 flex-1 overflow-auto" data-scroll-preserve="processing-graph-canvas">${svg}</div></div>`;
  bindGraphInteractions(container, sourceData);
}

function graphToolbarHtml(data: GraphData, sourceData: GraphData): string {
  const aggregateCount = data.nodes.filter(
    (node) => node.details?.aggregate === true,
  ).length;
  const focus = focusedAggregateDetails(sourceData);
  const toolbar = `<div class="border-base-content/10 bg-base-200/80 flex flex-wrap items-center justify-between gap-2 rounded-lg border px-3 py-2 text-xs">
    <span class="text-base-content/70">${aggregateCount ? `${aggregateCount} grouped branch${aggregateCount === 1 ? "" : "es"}` : "Processing graph SVG"}</span>
    <div class="flex flex-wrap items-center gap-2">
      ${
        focus
          ? '<button type="button" class="btn btn-xs btn-outline" data-graph-clear-aggregate-focus>Clear focus</button>'
          : aggregateCount
            ? '<span class="text-base-content/50">Click a grouped node to inspect its members.</span>'
            : ""
      }
    </div>
  </div>`;
  return `${toolbar}${focus ? aggregateFocusHtml(focus) : ""}`;
}

function focusedAggregateDetails(data: GraphData):
  | {
      label: string;
      nodes: GraphNode[];
      activeCount: number;
    }
  | null {
  if (!focusedAggregateGroupKey?.startsWith(`${data.pipelineId}|`)) return null;
  const group = repeatedLeafGroups(data).get(focusedAggregateGroupKey);
  if (!group || group.nodes.length < REPEATED_LEAF_GROUP_MIN) return null;
  return {
    label: aggregateNodeLabel(group.nodes[0], group.edge, group.nodes.length),
    nodes: group.nodes,
    activeCount: group.nodes.filter((node) => node.active).length,
  };
}

function aggregateFocusHtml(focus: {
  label: string;
  nodes: GraphNode[];
  activeCount: number;
}): string {
  const visibleNodes = focus.nodes.slice(0, 12);
  const hiddenCount = focus.nodes.length - visibleNodes.length;
  return `<div class="border-base-content/10 bg-base-100/70 rounded-lg border px-3 py-3 text-xs">
    <div class="mb-2 flex flex-wrap items-center justify-between gap-2">
      <div>
        <div class="text-base-content text-sm font-semibold">${escapeXml(focus.label)}</div>
        <div class="text-base-content/60">${focus.activeCount}/${focus.nodes.length} active members brought forward</div>
      </div>
      ${hiddenCount > 0 ? `<span class="text-base-content/50">+${hiddenCount} more</span>` : ""}
    </div>
    <div class="grid grid-cols-1 gap-2 md:grid-cols-2 xl:grid-cols-3">
      ${visibleNodes.map(aggregateMemberHtml).join("")}
    </div>
  </div>`;
}

function aggregateMemberHtml(node: GraphNode): string {
  const status = String(
    node.details?.status || (node.active ? "running" : "inactive"),
  );
  const phase = String(node.details?.phase || "");
  const bitrate =
    node.details?.bitrateKbps !== undefined
      ? formatKbps(node.details.bitrateKbps)
      : "";
  const progress =
    node.details?.lastProgressAgeMs !== undefined
      ? `progress ${formatAgeMs(node.details.lastProgressAgeMs)}`
      : "";
  const meta = [status, phase, bitrate, progress].filter(Boolean).join(" / ");
  return `<div class="border-base-content/10 bg-base-200/70 rounded-md border px-3 py-2">
    <div class="flex items-start justify-between gap-2">
      <span class="text-base-content font-medium">${escapeXml(truncate(node.label, 42))}</span>
      <span class="${node.active ? "bg-success" : "bg-error"} mt-1 h-2 w-2 shrink-0 rounded-full"></span>
    </div>
    <div class="text-base-content/60 mt-1">${escapeXml(meta || node.type)}</div>
  </div>`;
}

function bindGraphInteractions(container: HTMLElement, sourceData: GraphData): void {
  container
    .querySelectorAll<SVGGElement>("[data-graph-aggregate-key]")
    .forEach((node) => {
      node.addEventListener("click", () => {
        const key = node.dataset.graphAggregateKey;
        if (!key) return;
        focusedAggregateGroupKey = key;
        renderGraphInto(container, sourceData);
      });
    });
  container
    .querySelector<HTMLButtonElement>("[data-graph-clear-aggregate-focus]")
    ?.addEventListener("click", () => {
      focusedAggregateGroupKey = null;
      renderGraphInto(container, sourceData);
    });
}

function aggregateRepeatedLeaves(data: GraphData): GraphData {
  const groups = repeatedLeafGroups(data);
  const candidateLeafIds = new Set<string>();
  for (const group of groups.values()) {
    for (const node of group.nodes) candidateLeafIds.add(node.id);
  }
  const keepNodes: GraphNode[] = data.nodes.filter(
    (node) => !candidateLeafIds.has(node.id),
  );
  const groupedNodeIds = new Set<string>();
  const branchSummaries = new Map<
    string,
    { totalLeaves: number; groups: string[] }
  >();

  const aggregateNodes: GraphNode[] = [];
  const aggregateEdges: GraphEdge[] = [];
  for (const [key, group] of groups) {
    if (group.nodes.length < REPEATED_LEAF_GROUP_MIN) {
      keepNodes.push(...group.nodes);
      continue;
    }
    for (const node of group.nodes) groupedNodeIds.add(node.id);
    const firstNode = group.nodes[0];
    const activeCount = group.nodes.filter((node) => node.active).length;
    const aggregateId = `aggregate:${hashString(key)}`;
    const aggregateLabel = aggregateNodeLabel(
      firstNode,
      group.edge,
      group.nodes.length,
    );
    aggregateNodes.push({
      id: aggregateId,
      type: firstNode.type,
      label: aggregateLabel,
      active: activeCount > 0,
      details: aggregateNodeDetails(firstNode, group.nodes, activeCount, key),
      metrics: undefined,
    });
    const branchSummary =
      branchSummaries.get(group.edge.from) || {
        totalLeaves: 0,
        groups: [],
      };
    branchSummary.totalLeaves += group.nodes.length;
    branchSummary.groups.push(aggregateLabel);
    branchSummaries.set(group.edge.from, branchSummary);
    aggregateEdges.push({
      from: group.edge.from,
      to: aggregateId,
      label: group.edge.label,
    });
  }

  const edges = data.edges
    .filter(
      (edge) => !groupedNodeIds.has(edge.from) && !groupedNodeIds.has(edge.to),
    )
    .concat(aggregateEdges);

  return {
    pipelineId: data.pipelineId,
    nodes: keepNodes
      .map((node) => annotateBranchStart(node, branchSummaries))
      .concat(aggregateNodes),
    edges,
  };
}

function repeatedLeafGroups(data: GraphData): Map<string, LeafGroup> {
  const incoming = new Map<string, GraphEdge[]>();
  const outgoing = new Map<string, GraphEdge[]>();
  for (const edge of data.edges) {
    if (!incoming.has(edge.to)) incoming.set(edge.to, []);
    incoming.get(edge.to)!.push(edge);
    if (!outgoing.has(edge.from)) outgoing.set(edge.from, []);
    outgoing.get(edge.from)!.push(edge);
  }

  const groups = new Map<string, LeafGroup>();
  for (const node of data.nodes) {
    const inEdges = incoming.get(node.id) || [];
    const outEdges = outgoing.get(node.id) || [];
    if (inEdges.length !== 1 || outEdges.length > 0) continue;

    const edge = inEdges[0];
    const protocol = egressProtocolLabel(node, edge);
    const groupKey = [
      data.pipelineId,
      edge.from,
      edge.label,
      node.type,
      protocol,
      node.active ? "active" : "inactive",
      nodeStateLabel(node),
    ].join("|");
    if (!groups.has(groupKey)) groups.set(groupKey, { edge, nodes: [] });
    groups.get(groupKey)!.nodes.push(node);
  }
  return groups;
}

function annotateBranchStart(
  node: GraphNode,
  branchSummaries: Map<string, { totalLeaves: number; groups: string[] }>,
): GraphNode {
  const summary = branchSummaries.get(node.id);
  if (!summary) return node;
  return {
    ...node,
    details: {
      ...(node.details || {}),
      branchStart: true,
      branchLeafCount: summary.totalLeaves,
      branchGroups: summary.groups,
    },
  };
}

function egressProtocolLabel(node: GraphNode, edge: GraphEdge): string {
  if (node.type !== "egress") return "";
  const label = node.label || edge.label || "egress";
  const lower = label.toLowerCase();
  if (lower.includes("rtmp")) return "RTMP";
  if (lower.includes("srt")) return "SRT";
  if (lower.includes("hls")) return "HLS";
  return edge.label || "Egress";
}

function nodeStateLabel(node: GraphNode): string {
  const status = String(node.details?.status || "");
  const phase = String(node.details?.phase || "");
  const backend = String(node.details?.backend || "");
  return `${status}|${phase}|${backend}`;
}

function humanizeNodeType(type: string): string {
  return type
    .split(/[_\s-]+/)
    .filter(Boolean)
    .map((part) => part.slice(0, 1).toUpperCase() + part.slice(1))
    .join(" ");
}

function aggregateNodeLabel(
  node: GraphNode,
  edge: GraphEdge,
  count: number,
): string {
  if (node.type === "egress") {
    return `${egressProtocolLabel(node, edge)} egress x${count}`;
  }
  return `${humanizeNodeType(node.type)} x${count}`;
}

function aggregateItemLabel(node: GraphNode): string {
  if (node.type === "egress") {
    const protocol = String(node.details?.protocol || "").trim();
    return `${
      protocol ||
      egressProtocolLabel(node, { from: "", to: "", label: "" }) ||
      "egress"
    } outputs`;
  }
  const type = humanizeNodeType(node.type).toLowerCase();
  return `${type} stages`;
}

function aggregateNodeDetails(
  template: GraphNode,
  nodes: GraphNode[],
  activeCount: number,
  aggregateKey: string,
): Record<string, unknown> {
  let totalSize = 0;
  let bitrateKbps = 0;
  let maxProgressAgeMs = 0;
  for (const node of nodes) {
    totalSize += finiteNumber(node.details?.totalSize);
    bitrateKbps += finiteNumber(node.details?.bitrateKbps);
    maxProgressAgeMs = Math.max(
      maxProgressAgeMs,
      finiteNumber(node.details?.lastProgressAgeMs),
    );
  }
  const status = String(template.details?.status || "");
  const phase = String(template.details?.phase || "");
  const backend = String(template.details?.backend || "");
  return {
    aggregate: true,
    aggregateKey,
    itemLabel: aggregateItemLabel(template),
    count: nodes.length,
    activeCount,
    status,
    phase,
    backend,
    totalSize,
    bitrateKbps,
    lastProgressAgeMs: maxProgressAgeMs,
  };
}

function appendBranchDetails(lines: string[], node: GraphNode): void {
  if (node.details?.branchStart !== true) return;
  const leafCount = finiteNumber(node.details.branchLeafCount);
  const rawGroups = Array.isArray(node.details.branchGroups)
    ? node.details.branchGroups
    : [];
  const groups = rawGroups
    .map((item) => String(item))
    .filter((item) => item.length > 0);
  const branchLines = [`branch starts: ${leafCount} leaves`];
  if (groups.length > 0) {
    const suffix = groups.length > 2 ? ` / +${groups.length - 2} more` : "";
    branchLines.push(`fan-out: ${groups.slice(0, 2).join(" / ")}${suffix}`);
  }
  lines.unshift(...branchLines);
}

function hashString(value: string): string {
  let hash = 2166136261;
  for (let i = 0; i < value.length; i++) {
    hash ^= value.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0).toString(36);
}

function escapeXml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function truncate(s: string, max: number): string {
  return s.length > max ? s.slice(0, max - 1) + "…" : s;
}
