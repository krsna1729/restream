// The control room's location lives in the URL query (`?w=<workspace>` and,
// for pipeline workspaces, `&p=<pipeline id>`), so reload, back/forward and
// shared links land on the same view.

export const WORKSPACES = [
  "fleet",
  "pipelines",
  "operate",
  "inspect",
  "monitor",
  "incidents",
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

function isWorkspace(value: string | null): value is Workspace {
  return value !== null && (WORKSPACES as readonly string[]).includes(value);
}

/** The place a query string names; an absent or unknown workspace is Fleet. */
export function parsePlace(search: string): Place {
  const params = new URLSearchParams(search);
  const workspace = params.get("w");
  return {
    workspace: isWorkspace(workspace) ? workspace : "fleet",
    pipelineId: params.get("p") || null,
  };
}

/** `search` naming `place`; other parameters are kept. */
export function placeSearch(search: string, place: Place): string {
  const params = new URLSearchParams(search);
  params.set("w", place.workspace);
  if (place.pipelineId) params.set("p", place.pipelineId);
  else params.delete("p");
  return `?${params.toString()}`;
}
