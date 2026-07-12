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
const REPEATED_EGRESS_GROUP_MIN = 4;

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

export function renderGraphInto(container: HTMLElement, data: GraphData): void {
  data = aggregateRepeatedEgressLeaves(data);

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
  const svgW = 40 + (maxCol + 1) * (NODE_W + COL_GAP);
  const svgH = 40 + maxNodesInCol * (totalNodeH + ROW_GAP);

  let svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${svgW} ${svgH}" class="w-full h-full">`;
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
    const opacity = node.active ? "1" : "0.5";

    // Node box
    svg += `<g opacity="${opacity}">`;
    svg += `<rect x="${pos.x}" y="${pos.y}" width="${NODE_W}" height="${totalNodeH}" rx="8" fill="#1f2937" stroke="${color}" stroke-width="2"/>`;

    // Type badge
    svg += `<rect x="${pos.x}" y="${pos.y}" width="${NODE_W}" height="22" rx="8" fill="${color}" opacity="0.15"/>`;
    svg += `<rect x="${pos.x}" y="${pos.y + 14}" width="${NODE_W}" height="8" fill="${color}" opacity="0.15"/>`;
    svg += `<text x="${pos.x + 10}" y="${pos.y + 16}" fill="${color}" font-size="11" font-weight="600">${escapeXml(node.type.toUpperCase())}</text>`;

    // Label
    svg += `<text x="${pos.x + 10}" y="${pos.y + 40}" fill="#e5e7eb" font-size="13" font-weight="500">${escapeXml(truncate(node.label, 28))}</text>`;

    // Status dot
    const dotColor = node.active ? "#22c55e" : "#ef4444";
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
      const bitrate = Number(node.details.bitrateKbps);
      const bitrateLabel = Number.isFinite(bitrate)
        ? ` | ${bitrate.toFixed(0)} kbps`
        : "";
      detailLines.push(
        `received: ${formatBytes(node.details.bytesReceived as number)}${bitrateLabel}`,
      );
    } else if (
      node.type === "egress" &&
      node.details?.aggregate === true
    ) {
      const count = finiteNumber(node.details.count);
      const activeCount = finiteNumber(node.details.activeCount);
      const protocol = String(node.details.protocol || "egress");
      detailLines.push(`${count} ${protocol} outputs`);
      detailLines.push(`${activeCount}/${count} running`);
      detailLines.push(
        `${formatKbps(node.details.bitrateKbps)} | ${formatBytes(node.details.totalSize)}`,
      );
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

    // Metrics
    if (
      node.metrics &&
      node.metrics.packetsIn > 0 &&
      node.details?.aggregate !== true
    ) {
      const m = node.metrics;
      const packetsPerSec = finiteNumber(m.packetsPerSec);
      const avgUsPerPacket = finiteNumber(m.avgUsPerPacket);
      const uptimeSecs = finiteNumber(m.uptimeSecs ?? m.uptimeSec);
      const my = pos.y + NODE_H - 10;
      svg += `<text x="${pos.x + 10}" y="${my + 10}" fill="#9ca3af" font-size="10">in: ${formatRate(packetsPerSec)} pkt | ${formatBytes(finiteNumber(m.bytesIn))}</text>`;
      const outLabel =
        finiteNumber(m.bytesOut) > 0
          ? `out: ${formatBytes(finiteNumber(m.bytesOut))}`
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
  container.innerHTML = svg;
}

function aggregateRepeatedEgressLeaves(data: GraphData): GraphData {
  const incoming = new Map<string, GraphEdge[]>();
  const outgoing = new Map<string, GraphEdge[]>();
  for (const edge of data.edges) {
    if (!incoming.has(edge.to)) incoming.set(edge.to, []);
    incoming.get(edge.to)!.push(edge);
    if (!outgoing.has(edge.from)) outgoing.set(edge.from, []);
    outgoing.get(edge.from)!.push(edge);
  }

  const groups = new Map<string, { edge: GraphEdge; nodes: GraphNode[] }>();
  const keepNodes: GraphNode[] = [];
  const groupedNodeIds = new Set<string>();

  for (const node of data.nodes) {
    const inEdges = incoming.get(node.id) || [];
    const outEdges = outgoing.get(node.id) || [];
    if (node.type !== "egress" || inEdges.length !== 1 || outEdges.length > 0) {
      keepNodes.push(node);
      continue;
    }

    const edge = inEdges[0];
    const protocol = egressProtocolLabel(node, edge);
    const groupKey = [
      edge.from,
      edge.label,
      protocol,
      node.active ? "active" : "inactive",
      egressStatusLabel(node),
    ].join("|");
    if (!groups.has(groupKey)) groups.set(groupKey, { edge, nodes: [] });
    groups.get(groupKey)!.nodes.push(node);
  }

  const aggregateNodes: GraphNode[] = [];
  const aggregateEdges: GraphEdge[] = [];
  for (const [key, group] of groups) {
    if (group.nodes.length < REPEATED_EGRESS_GROUP_MIN) {
      keepNodes.push(...group.nodes);
      continue;
    }
    for (const node of group.nodes) groupedNodeIds.add(node.id);
    const protocol = egressProtocolLabel(group.nodes[0], group.edge);
    const activeCount = group.nodes.filter((node) => node.active).length;
    const aggregateId = `aggregate:${hashString(key)}`;
    aggregateNodes.push({
      id: aggregateId,
      type: "egress",
      label: `${protocol} egress x${group.nodes.length}`,
      active: activeCount > 0,
      details: aggregateEgressDetails(protocol, group.nodes, activeCount),
      metrics: undefined,
    });
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
    nodes: keepNodes.concat(aggregateNodes),
    edges,
  };
}

function egressProtocolLabel(node: GraphNode, edge: GraphEdge): string {
  const label = node.label || edge.label || "egress";
  const lower = label.toLowerCase();
  if (lower.includes("rtmp")) return "RTMP";
  if (lower.includes("srt")) return "SRT";
  if (lower.includes("hls")) return "HLS";
  return edge.label || "Egress";
}

function egressStatusLabel(node: GraphNode): string {
  const status = String(node.details?.status || "");
  const phase = String(node.details?.phase || "");
  return `${status}|${phase}`;
}

function aggregateEgressDetails(
  protocol: string,
  nodes: GraphNode[],
  activeCount: number,
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
  return {
    aggregate: true,
    protocol,
    count: nodes.length,
    activeCount,
    totalSize,
    bitrateKbps,
    lastProgressAgeMs: maxProgressAgeMs,
  };
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
