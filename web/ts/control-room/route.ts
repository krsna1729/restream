// The control room's location lives in the URL query (`?w=<workspace>` and,
// for pipeline workspaces, `&p=<pipeline id>`), so reload, back/forward and
// shared links land on the same view. These are the pure rules; router.tsx
// wires them to TanStack Router, which owns history and link clicks.

export const WORKSPACES = [
  "fleet",
  "pipelines",
  "operate",
  "inspect",
  "monitor",
  "incidents",
  "logs",
  "capacity",
  "media",
  "system",
  "changes",
  "settings",
] as const;

export type Workspace = (typeof WORKSPACES)[number];

export const WORKSPACE_LABELS: Record<Workspace, string> = {
  fleet: "Fleet",
  pipelines: "Pipelines",
  operate: "Operate",
  inspect: "Inspect",
  monitor: "Monitor",
  incidents: "Incidents",
  logs: "Logs",
  capacity: "Capacity",
  media: "Media",
  system: "System",
  changes: "Changes",
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
  const rest: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(search)) {
    if (key !== "w" && key !== "p") rest[key] = value;
  }
  return {
    w: place.workspace,
    ...(place.pipelineId === null ? {} : { p: place.pipelineId }),
    ...rest,
  };
}
