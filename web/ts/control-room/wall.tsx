// The multiview wall: every pipeline's program and state on one screen.
// Tiles show state from the runtime stream at no cost; live video is opt-in
// per tile (or "Watch all"), capped, because each watched tile makes the
// engine produce a preview. Layout, sort and filter are kept per browser.

import { useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";

import { PageHeader } from "../ui/page-header.js";
import { EmptyState } from "../ui/panel.js";
import { StatusBadge, StatusDot } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";
import { buildFleetModel } from "./fleet-model.js";
import { formatBps } from "./format.js";
import { useOpenAlerts } from "./polled.js";
import { HlsProgramPreview } from "./preview.js";
import { PlaceLink } from "./router.js";
import type { LiveSnapshot } from "./store.js";
import {
  MAX_WATCHED_TILES,
  WALL_LAYOUTS,
  buildWall,
  prunedWatched,
  toggleWatched,
  wallColumns,
  watchAll,
} from "./wall-model.js";
import type { Tally, WallFilter, WallLayout, WallSort, WallTile } from "./wall-model.js";

interface WallPrefs {
  layout: WallLayout;
  sort: WallSort;
  filter: WallFilter;
}

const PREFS_KEY = "restream.wall";
const DEFAULT_PREFS: WallPrefs = { layout: "auto", sort: "name", filter: "all" };

function storedPrefs(): WallPrefs {
  try {
    const raw = JSON.parse(localStorage.getItem(PREFS_KEY) ?? "null") as Partial<WallPrefs> | null;
    return {
      layout: WALL_LAYOUTS.includes(raw?.layout as WallLayout) ? (raw?.layout as WallLayout) : DEFAULT_PREFS.layout,
      sort: raw?.sort === "worst" ? "worst" : "name",
      filter: raw?.filter === "live" || raw?.filter === "alarms" ? raw.filter : "all",
    };
  } catch {
    return DEFAULT_PREFS;
  }
}

const TALLY_TONE: Record<Tally, Tone> = { alarm: "bad", warning: "warn", live: "ok", idle: "neutral" };
const TALLY_RING: Record<Tally, string> = {
  alarm: "ring-2 ring-error",
  warning: "ring-2 ring-warning",
  live: "ring-1 ring-success/60",
  idle: "ring-1 ring-base-content/10",
};
const TALLY_LABEL: Record<Tally, string> = { alarm: "alarm", warning: "attention", live: "live", idle: "idle" };

export function Wall({ live }: { live: LiveSnapshot }) {
  const [prefs, setPrefs] = useState<WallPrefs>(storedPrefs);
  const [watched, setWatched] = useState<string[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const wall = useRef<HTMLDivElement | null>(null);
  const alerts = useOpenAlerts();
  const model = useMemo(() => (live.state ? buildFleetModel(live.state, live.rates) : null), [live.state, live.rates]);
  const tiles = useMemo(
    () => (model ? buildWall(model, alerts.value?.alerts ?? [], { sort: prefs.sort, filter: prefs.filter }) : []),
    [model, alerts.value, prefs.sort, prefs.filter],
  );
  // A pipeline that went idle, was deleted or is filtered out stops playing.
  useEffect(() => {
    setWatched((current) => {
      const next = prunedWatched(current, tiles);
      return next.length === current.length ? current : next;
    });
  }, [tiles]);

  const update = (next: Partial<WallPrefs>) => {
    const merged = { ...prefs, ...next };
    setPrefs(merged);
    try {
      localStorage.setItem(PREFS_KEY, JSON.stringify(merged));
    } catch {
      // Storage blocked: the choice lasts this visit.
    }
  };
  const toggle = (id: string) => {
    const next = toggleWatched(watched, id);
    setNotice(
      next.length === watched.length && !watched.includes(id)
        ? `At most ${MAX_WATCHED_TILES} tiles play at once: stop one first.`
        : null,
    );
    setWatched(next);
  };

  if (model === null) {
    return <EmptyState title="Waiting for runtime state…" detail="The dashboard is connecting to the engine's state stream." />;
  }
  const alarmed = tiles.filter((tile) => tile.tally === "alarm" || tile.tally === "warning").length;
  const columns = wallColumns(prefs.layout, tiles.length);
  const liveCount = tiles.filter((tile) => tile.source !== null).length;

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow={`Wall · ${tiles.length.toLocaleString()} of ${model.pipelinesTotal.toLocaleString()} pipelines`}
        title={alarmed > 0 ? `${alarmed.toLocaleString()} need attention` : "Every program looks fine"}
        tone={alarmed > 0 ? "warn" : "ok"}
        subtitle={`${liveCount.toLocaleString()} receiving · ${watched.length} of at most ${MAX_WATCHED_TILES} playing — each playing tile makes the engine produce a preview`}
        actions={
          <div className="flex flex-wrap items-center gap-2">
            <label className="flex items-center gap-1 text-xs">
              Layout
              <select
                className="select select-xs"
                value={String(prefs.layout)}
                onChange={(event) => {
                  const value = event.target.value;
                  update({ layout: value === "auto" ? "auto" : (Number(value) as WallLayout) });
                }}
              >
                {WALL_LAYOUTS.map((layout) => (
                  <option key={String(layout)} value={String(layout)}>
                    {layout === "auto" ? "Fit all" : `${layout} across`}
                  </option>
                ))}
              </select>
            </label>
            <label className="flex items-center gap-1 text-xs">
              Order
              <select className="select select-xs" value={prefs.sort} onChange={(event) => update({ sort: event.target.value as WallSort })}>
                <option value="name">By name (fixed places)</option>
                <option value="worst">Worst first</option>
              </select>
            </label>
            <div role="radiogroup" aria-label="Show" className="join">
              {(["all", "live", "alarms"] as const).map((filter) => (
                <button
                  key={filter}
                  type="button"
                  role="radio"
                  aria-checked={prefs.filter === filter}
                  className={`btn btn-xs join-item ${prefs.filter === filter ? "btn-active" : ""}`}
                  onClick={() => update({ filter })}
                >
                  {filter === "all" ? "All" : filter === "live" ? "Receiving" : "Attention"}
                </button>
              ))}
            </div>
            {watched.length > 0 ? (
              <button type="button" className="btn btn-xs btn-ghost" onClick={() => setWatched([])}>
                Stop all video
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-xs btn-outline btn-accent"
                disabled={liveCount === 0}
                onClick={() => setWatched(watchAll(tiles))}
              >
                Watch {Math.min(liveCount, MAX_WATCHED_TILES)}
              </button>
            )}
            <button
              type="button"
              className="btn btn-xs btn-ghost"
              onClick={() => void wall.current?.requestFullscreen?.().catch(() => undefined)}
            >
              Full screen
            </button>
          </div>
        }
      />
      {notice && (
        <p role="status" className="text-sm text-warning">
          {notice}
        </p>
      )}
      {tiles.length === 0 ? (
        <EmptyState
          title={prefs.filter === "all" ? "No pipelines yet" : "Nothing matches this filter"}
          detail={prefs.filter === "all" ? "Create a pipeline to see it here." : "Show all pipelines to see the whole wall."}
        />
      ) : (
        <div
          ref={wall}
          // The chosen layout applies from desktop widths: a phone shows one
          // tile across and a tablet two, so names stay readable.
          className="grid grid-cols-1 gap-2 bg-base-100 p-1 sm:grid-cols-2 lg:grid-cols-[repeat(var(--wall-columns),minmax(0,1fr))] [&:fullscreen]:grid-cols-[repeat(var(--wall-columns),minmax(0,1fr))] [&:fullscreen]:p-3"
          style={{ "--wall-columns": columns } as CSSProperties}
          aria-label="Multiview wall"
        >
          {tiles.map((tile) => (
            <WallTileView key={tile.id} tile={tile} watching={watched.includes(tile.id)} onToggle={() => toggle(tile.id)} />
          ))}
        </div>
      )}
    </div>
  );
}

function WallTileView(props: { tile: WallTile; watching: boolean; onToggle: () => void }) {
  const { tile } = props;
  return (
    <article
      className={`flex min-w-0 flex-col overflow-hidden rounded-box bg-base-200 ${TALLY_RING[tile.tally]}`}
      aria-label={`${tile.name}: ${TALLY_LABEL[tile.tally]}`}
    >
      {props.watching && tile.source !== null ? (
        <HlsProgramPreview pipelineId={tile.id} sourceCodec={tile.sourceCodec} variant="tile" />
      ) : (
        <div className="grid aspect-video place-items-center bg-base-300 p-2 text-center">
          <div className="flex flex-col items-center gap-1">
            <StatusDot tone={TALLY_TONE[tile.tally]} pulse={tile.tally === "alarm"} className="size-3" />
            <span className="text-xs text-base-content/70">{tile.source ?? "no input connected"}</span>
            {tile.source !== null && (
              <button type="button" className="btn btn-xs btn-outline" onClick={props.onToggle}>
                Watch
              </button>
            )}
          </div>
        </div>
      )}
      <div className="flex flex-col gap-1 p-2 text-xs">
        <div className="flex min-w-0 items-center gap-2">
          <PlaceLink place={{ workspace: "operate", pipelineId: tile.id }} className="link-hover min-w-0 truncate py-3 font-semibold sm:py-0">
            {tile.name}
          </PlaceLink>
          <StatusBadge tone={TALLY_TONE[tile.tally]} className="ml-auto shrink-0">
            {TALLY_LABEL[tile.tally]}
          </StatusBadge>
          {props.watching && (
            <button type="button" className="btn btn-ghost btn-xs shrink-0" onClick={props.onToggle} aria-label={`Stop video of ${tile.name}`}>
              Stop
            </button>
          )}
        </div>
        <div className="flex flex-wrap gap-x-3 text-base-content/70">
          {tile.source !== null && <span>{formatBps(tile.ingestBps)} in</span>}
          <span>
            {tile.delivering}/{tile.wanted} delivering
          </span>
          {tile.recording && <span className="text-error">● rec</span>}
        </div>
        {tile.alarms.slice(0, 2).map((alarm) => (
          <p key={alarm.title} className={`truncate ${alarm.severity === "critical" ? "text-error" : "text-warning"}`} title={alarm.title}>
            {alarm.title}
          </p>
        ))}
        {tile.alarms.length > 2 && <p className="text-base-content/60">+{tile.alarms.length - 2} more</p>}
      </div>
    </article>
  );
}
