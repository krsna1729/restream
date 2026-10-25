// The multiview wall: one tile per pipeline, its program and its state at a
// glance. Tiles keep their place (by name) unless the operator sorts worst
// first, so a wall on a screen does not reshuffle as states change. Live
// video is opt-in per tile and capped: each watched tile makes the engine
// produce a preview.

import type { OperatorAlert } from "../core/api-types.js";
import type { FleetModel, FleetPipelineRow, PipelineHealth } from "./fleet-model.js";
import { pipelineHealth } from "./fleet-model.js";

/** At most this many tiles play at once (each costs a server preview). */
export const MAX_WATCHED_TILES = 9;

export type WallLayout = "auto" | 2 | 3 | 4 | 5;
export const WALL_LAYOUTS: readonly WallLayout[] = ["auto", 2, 3, 4, 5];

export type WallSort = "name" | "worst";
export type WallFilter = "all" | "live" | "alarms";

/** A tile's border and badge, strongest first. */
export type Tally = "alarm" | "warning" | "live" | "idle";

export interface WallAlarm {
  readonly title: string;
  readonly severity: OperatorAlert["severity"];
}

export interface WallTile {
  readonly id: string;
  readonly name: string;
  readonly health: PipelineHealth;
  readonly tally: Tally;
  /** What is on air: protocol and codec, `null` while no input is connected. */
  readonly source: string | null;
  readonly sourceCodec: string | null;
  readonly ingestBps: number | null;
  readonly delivering: number;
  readonly wanted: number;
  readonly recording: boolean;
  /** Open alarms on the pipeline, critical first. */
  readonly alarms: readonly WallAlarm[];
}

const SEVERITY_RANK: Record<string, number> = { critical: 0, warning: 1, info: 2 };
const TALLY_RANK: Record<Tally, number> = { alarm: 0, warning: 1, live: 2, idle: 3 };

function tallyOf(row: FleetPipelineRow, health: PipelineHealth, alarms: readonly WallAlarm[]): Tally {
  if (alarms.some((alarm) => alarm.severity === "critical")) return "alarm";
  if (alarms.length > 0 || health.tone === "warning") return "warning";
  return row.source === null ? "idle" : "live";
}

/** The wall's tiles, filtered and ordered. */
export function buildWall(
  model: FleetModel,
  alerts: readonly OperatorAlert[],
  options: { sort: WallSort; filter: WallFilter },
): WallTile[] {
  const byPipeline = new Map<string, WallAlarm[]>();
  for (const alert of alerts) {
    if (!alert.pipelineId) continue;
    const list = byPipeline.get(alert.pipelineId) ?? [];
    list.push({ title: alert.title, severity: alert.severity });
    byPipeline.set(alert.pipelineId, list);
  }
  const tiles = model.pipelines.map((row): WallTile => {
    const alarms = (byPipeline.get(row.id) ?? []).sort(
      (a, b) => (SEVERITY_RANK[a.severity] ?? 3) - (SEVERITY_RANK[b.severity] ?? 3) || a.title.localeCompare(b.title),
    );
    const health = pipelineHealth(row);
    return {
      id: row.id,
      name: row.name,
      health,
      tally: tallyOf(row, health, alarms),
      source: row.source
        ? [row.source.bonded ? `${row.source.protocol} bonded` : row.source.protocol, row.source.videoCodec]
            .filter(Boolean)
            .join(" · ")
        : null,
      sourceCodec: row.source?.videoCodec ?? null,
      ingestBps: row.ingestBps,
      delivering: row.delivering,
      wanted: row.wanted,
      recording: row.recording,
      alarms,
    };
  });
  const shown = tiles.filter((tile) => {
    if (options.filter === "live") return tile.source !== null;
    if (options.filter === "alarms") return tile.tally === "alarm" || tile.tally === "warning";
    return true;
  });
  return shown.sort((a, b) =>
    options.sort === "worst"
      ? TALLY_RANK[a.tally] - TALLY_RANK[b.tally] || a.name.localeCompare(b.name)
      : a.name.localeCompare(b.name),
  );
}

/** Grid columns: the chosen count, or for `auto` the smallest square that
 * fits every tile (at most 6 across). */
export function wallColumns(layout: WallLayout, tiles: number): number {
  if (layout !== "auto") return layout;
  return Math.min(6, Math.max(1, Math.ceil(Math.sqrt(tiles))));
}

/** Start or stop watching one tile. Starting past the cap is refused (the
 * set is returned unchanged); the caller says why. */
export function toggleWatched(watched: readonly string[], id: string, cap = MAX_WATCHED_TILES): string[] {
  if (watched.includes(id)) return watched.filter((item) => item !== id);
  if (watched.length >= cap) return [...watched];
  return [...watched, id];
}

/** Watch every shown tile with a live input, up to the cap, in wall order. */
export function watchAll(tiles: readonly WallTile[], cap = MAX_WATCHED_TILES): string[] {
  return tiles
    .filter((tile) => tile.source !== null)
    .slice(0, cap)
    .map((tile) => tile.id);
}

/** Watched ids that still name a tile with a live input (a pipeline deleted
 * or gone idle stops its preview). */
export function prunedWatched(watched: readonly string[], tiles: readonly WallTile[]): string[] {
  const live = new Set(tiles.filter((tile) => tile.source !== null).map((tile) => tile.id));
  return watched.filter((id) => live.has(id));
}
