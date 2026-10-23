// What the command palette can find and how it ranks a query: workspaces,
// pipelines, destinations and open incidents from what the page already
// holds. Pure, so ranking is tested without a DOM; ten thousand entries
// rank in a few milliseconds.

import type { OperatorAlert } from "../../core/api-types.js";
import type { RuntimeState } from "../../core/generated/runtime-state.js";
import { WORKSPACE_LABELS, WORKSPACES } from "../route.js";
import type { Place, Workspace } from "../route.js";

export type PaletteKind = "workspace" | "pipeline" | "destination" | "incident" | "action";

export interface PaletteEntry {
  readonly id: string;
  readonly kind: PaletteKind;
  readonly label: string;
  /** A second line: protocol, pipeline, state. */
  readonly detail: string;
  /** Extra words that match but are not shown (ids, platform). */
  readonly keywords: string;
  readonly place?: Place;
  readonly action?: string;
}

export const PALETTE_ACTIONS: ReadonlyArray<{ id: string; label: string; detail: string }> = [
  { id: "theme:dark", label: "Theme: dark", detail: "Switch to the dark theme" },
  { id: "theme:light", label: "Theme: light", detail: "Switch to the light theme" },
  { id: "theme:system", label: "Theme: follow system", detail: "Match the operating system" },
  { id: "density:compact", label: "Density: compact", detail: "More rows per screen" },
  { id: "density:comfortable", label: "Density: comfortable", detail: "Larger type and targets" },
];

export function paletteEntries(
  state: RuntimeState | null,
  alerts: readonly OperatorAlert[],
  currentPipeline: string | null,
  /** The workspaces this session may open; all by default. */
  canOpen: (workspace: Workspace) => boolean = () => true,
): PaletteEntry[] {
  const entries: PaletteEntry[] = WORKSPACES.filter(canOpen).map((workspace) => ({
    id: `w:${workspace}`,
    kind: "workspace",
    label: WORKSPACE_LABELS[workspace],
    detail: "Workspace",
    keywords: workspace,
    place: { workspace, pipelineId: currentPipeline },
  }));
  const pipelineNames = new Map<string, string>();
  for (const pipeline of state?.pipelines ?? []) {
    pipelineNames.set(pipeline.id, pipeline.name);
    entries.push({
      id: `p:${pipeline.id}`,
      kind: "pipeline",
      label: pipeline.name,
      detail: pipeline.ingest ? `${pipeline.ingest.protocol.toUpperCase()} live` : "no source",
      keywords: pipeline.id,
      place: { workspace: "operate", pipelineId: pipeline.id },
    });
  }
  for (const output of state?.outputs ?? []) {
    entries.push({
      id: `o:${output.id}`,
      kind: "destination",
      label: output.name,
      detail: `${pipelineNames.get(output.pipelineId) ?? output.pipelineId} · ${output.protocol.toUpperCase()} · ${output.diagnosis ? "degraded" : output.desired === "running" ? output.convergence : "stopped"}`,
      keywords: `${output.id} ${output.protocol}`,
      place: { workspace: "operate", pipelineId: output.pipelineId },
    });
  }
  for (const alert of alerts) {
    entries.push({
      id: `a:${alert.id}`,
      kind: "incident",
      label: alert.title,
      detail: `${alert.severity} · ${alert.cause}`,
      keywords: alert.id,
      place: { workspace: "incidents", pipelineId: alert.pipelineId ?? currentPipeline },
    });
  }
  for (const action of PALETTE_ACTIONS) {
    entries.push({ id: `x:${action.id}`, kind: "action", label: action.label, detail: action.detail, keywords: "", action: action.id });
  }
  return entries;
}

/** How well `entry` matches every term of `query`; 0 is no match. */
export function score(entry: PaletteEntry, terms: readonly string[]): number {
  const label = entry.label.toLowerCase();
  const haystack = `${label} ${entry.detail.toLowerCase()} ${entry.keywords.toLowerCase()}`;
  let total = 0;
  for (const term of terms) {
    if (label.startsWith(term)) total += 8;
    else if (label.includes(` ${term}`)) total += 5;
    else if (label.includes(term)) total += 3;
    else if (haystack.includes(term)) total += 1;
    else return 0;
  }
  const kindBoost: Record<PaletteKind, number> = { workspace: 2, incident: 1.5, pipeline: 1, destination: 0, action: 0 };
  return total + kindBoost[entry.kind];
}

/** The best `limit` entries for `query`; an empty query lists workspaces
 * and incidents first. */
export function rank(entries: readonly PaletteEntry[], query: string, limit = 40): PaletteEntry[] {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (terms.length === 0) {
    return entries.filter((entry) => entry.kind === "workspace" || entry.kind === "incident").slice(0, limit);
  }
  const scored: Array<{ entry: PaletteEntry; value: number }> = [];
  for (const entry of entries) {
    const value = score(entry, terms);
    if (value > 0) scored.push({ entry, value });
  }
  scored.sort((a, b) => b.value - a.value || a.entry.label.localeCompare(b.entry.label));
  return scored.slice(0, limit).map((item) => item.entry);
}
