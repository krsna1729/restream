// The control room's location lives in the URL query (`?w=<workspace>` and,
// for pipeline workspaces, `&p=<pipeline id>`), so reload, back/forward and
// shared links land on the same view. These are the pure rules; router.tsx
// wires them to TanStack Router, which owns history and link clicks.

export const WORKSPACES = [
  "fleet",
  "channels",
  "pipelines",
  "operate",
  "inspect",
  "monitor",
  "incidents",
  "health",
  "logs",
  "capacity",
  "stability",
  "media",
  "system",
  "changes",
  "notifications",
  "team",
  "audit",
  "settings",
] as const;

export type Workspace = (typeof WORKSPACES)[number];

export const WORKSPACE_LABELS: Record<Workspace, string> = {
  fleet: "Fleet",
  channels: "Channels",
  pipelines: "Pipelines",
  operate: "Operate",
  inspect: "Inspect",
  monitor: "Monitor",
  incidents: "Incidents",
  health: "Stream health",
  logs: "Logs",
  capacity: "Capacity",
  stability: "Stability Lab",
  media: "Media",
  system: "System",
  changes: "Changes",
  notifications: "Notifications",
  team: "Team",
  audit: "Audit log",
  settings: "Settings",
};

export interface Place {
  readonly workspace: Workspace;
  /** The pipeline a pipeline workspace shows; kept across workspaces. */
  readonly pipelineId: string | null;
}

/**
 * The router's search: the place, plus every other parameter the URL
 * carried, so a link keeps them.
 */
export type PlaceSearch = {
  readonly w: Workspace;
  readonly p?: string;
} & Record<string, unknown>;

function isWorkspace(value: unknown): value is Workspace {
  return (
    typeof value === "string" && (WORKSPACES as readonly string[]).includes(value)
  );
}

/** The place a parsed query names; an absent or unknown workspace is Fleet. */
export function placeFromSearch(search: Record<string, unknown>): Place {
  const pipelineId = search.p;
  return {
    workspace: isWorkspace(search.w) ? search.w : "fleet",
    pipelineId:
      typeof pipelineId === "string" && pipelineId !== "" ? pipelineId : null,
  };
}

/** The search a place names; `p` is absent without a pipeline. */
export function placeSearch(
  search: Record<string, unknown>,
  place: Place,
): PlaceSearch {
  // fromEntries defines own properties, so `__proto__` stays a parameter.
  const rest = Object.fromEntries(
    Object.entries(search).filter(([key]) => key !== "w" && key !== "p"),
  );
  return {
    w: place.workspace,
    ...(place.pipelineId === null ? {} : { p: place.pipelineId }),
    ...rest,
  };
}

/**
 * A query string as plain strings: `?w=operate&p=1` names the same place the
 * hand-rolled parser read, and a parameter another part of the page set
 * survives as it was. (TanStack Router's default JSON round-trip would read
 * `p=1` as a number and drop the pipeline.)
 */
export function parseSearch(search: string): Record<string, unknown> {
  const parsed: Record<string, unknown> = {};
  for (const [key, value] of new URLSearchParams(search)) {
    // The first value of a repeated parameter wins, as `get` did before; a
    // name the object prototype also has stays an own parameter.
    if (!Object.prototype.hasOwnProperty.call(parsed, key)) {
      Object.defineProperty(parsed, key, {
        value,
        enumerable: true,
        writable: true,
        configurable: true,
      });
    }
  }
  return parsed;
}

/** The query string naming `search`; absent values are left out. */
export function stringifySearch(search: Record<string, unknown>): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(search)) {
    if (value === undefined || value === null) continue;
    params.set(key, String(value));
  }
  const query = params.toString();
  return query === "" ? "" : `?${query}`;
}
