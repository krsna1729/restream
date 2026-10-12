// The control room's location lives in the URL query (`?w=<workspace>`), so
// reload, back/forward and shared links land on the same workspace.

export const WORKSPACES = ["fleet"] as const;

export type Workspace = (typeof WORKSPACES)[number];

export const WORKSPACE_LABELS: Record<Workspace, string> = {
  fleet: "Fleet",
};

function isWorkspace(value: string | null): value is Workspace {
  return value !== null && (WORKSPACES as readonly string[]).includes(value);
}

/** The workspace a query string names; an absent or unknown one is Fleet. */
export function parseWorkspace(search: string): Workspace {
  const value = new URLSearchParams(search).get("w");
  return isWorkspace(value) ? value : "fleet";
}

/** `search` with `w` set to `workspace`; other parameters are kept. */
export function workspaceSearch(search: string, workspace: Workspace): string {
  const params = new URLSearchParams(search);
  params.set("w", workspace);
  return `?${params.toString()}`;
}
