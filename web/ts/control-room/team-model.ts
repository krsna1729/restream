// Roles in the control room: what each may open, which lower roles a member
// may view the dashboard as, and the audit log in plain words. The server
// enforces every rule here (`application/team.rs`); the dashboard only stops
// offering what would be refused.

import type { AuditEntry, Role } from "../core/api-types.js";
import type { Workspace } from "./route.js";

export const ROLES: readonly Role[] = ["viewer", "operator", "admin"];

export const ROLE_LABEL: Record<Role, string> = {
  viewer: "Viewer",
  operator: "Operator",
  admin: "Admin",
};

export const ROLE_SUMMARY: Record<Role, string> = {
  viewer: "Watches the plant: every dashboard, chart and log, no changes.",
  operator: "Runs the plant: starts and stops destinations, edits pipelines, media and inputs.",
  admin: "Owns the installation: settings, security, the team and the audit log.",
};

export function atLeast(role: Role, needed: Role): boolean {
  return ROLES.indexOf(role) >= ROLES.indexOf(needed);
}

/** The least role a workspace needs to be useful; the rest are viewer. */
const WORKSPACE_ROLE: Partial<Record<Workspace, Role>> = {
  settings: "operator",
  team: "admin",
  audit: "admin",
};

export function workspaceRole(workspace: Workspace): Role {
  return WORKSPACE_ROLE[workspace] ?? "viewer";
}

export function canOpen(role: Role, workspace: Workspace): boolean {
  return atLeast(role, workspaceRole(workspace));
}

/** The roles a member may view the dashboard as: those below their own. */
export function viewAsChoices(role: Role): Role[] {
  return ROLES.slice(0, ROLES.indexOf(role));
}

const SEGMENT = "([^/]+)";
const RULES: ReadonlyArray<[RegExp, (m: RegExpMatchArray) => string]> = [
  [new RegExp(`^POST /api/v1/pipelines/${SEGMENT}/outputs/${SEGMENT}/start$`), (m) => `Started destination ${m[2]} on ${m[1]}`],
  [new RegExp(`^POST /api/v1/pipelines/${SEGMENT}/outputs/${SEGMENT}/stop$`), (m) => `Stopped destination ${m[2]} on ${m[1]}`],
  [new RegExp(`^POST /api/v1/pipelines/${SEGMENT}/outputs$`), (m) => `Added a destination to ${m[1]}`],
  [new RegExp(`^(PUT|PATCH) /api/v1/pipelines/${SEGMENT}/outputs/${SEGMENT}$`), (m) => `Edited destination ${m[3]} on ${m[2]}`],
  [new RegExp(`^DELETE /api/v1/pipelines/${SEGMENT}/outputs/${SEGMENT}$`), (m) => `Removed destination ${m[2]} from ${m[1]}`],
  [new RegExp(`^POST /api/v1/pipelines/${SEGMENT}/inputs/${SEGMENT}/promote$`), (m) => `Put input ${m[2]} on air for ${m[1]}`],
  [new RegExp(`^POST /api/v1/pipelines/${SEGMENT}/recording/(start|stop)$`), (m) => `${m[2] === "start" ? "Started" : "Stopped"} recording ${m[1]}`],
  [/^POST \/api\/v1\/pipelines$/, () => "Created a pipeline"],
  [new RegExp(`^(PUT|PATCH) /api/v1/pipelines/${SEGMENT}$`), (m) => `Edited pipeline ${m[2]}`],
  [new RegExp(`^DELETE /api/v1/pipelines/${SEGMENT}$`), (m) => `Deleted pipeline ${m[1]}`],
  [new RegExp(`^POST /api/v1/ingests/${SEGMENT}/(start|stop)$`), (m) => `${m[2] === "start" ? "Started" : "Stopped"} file ingest ${m[1]}`],
  [/^POST \/api\/v1\/media\/upload$/, () => "Uploaded media"],
  [new RegExp(`^DELETE /api/v1/media/${SEGMENT}$`), (m) => `Deleted media ${decodeURIComponent(m[1])}`],
  [/^PATCH \/api\/v1\/settings$/, () => "Changed settings"],
  [/^POST \/api\/v1\/users$/, () => "Added a team member"],
  [new RegExp(`^PATCH /api/v1/users/${SEGMENT}$`), (m) => `Changed team member ${m[1]}`],
  [new RegExp(`^DELETE /api/v1/users/${SEGMENT}$`), (m) => `Removed team member ${m[1]}`],
  [/^POST \/api\/v1\/auth\/view-as$/, () => "Switched the role they view as"],
  [/^POST \/api\/v1\/auth\/logout$/, () => "Signed out"],
  [/^POST \/api\/v1\/auth\/change-password$/, () => "Changed their password"],
  [new RegExp(`^POST /api/v1/agent/operations/${SEGMENT}/(approve|apply|verify)$`), (m) => `${m[2][0].toUpperCase()}${m[2].slice(1)} change ${m[1]}`],
];

/** One change in plain words; the raw method and path when no rule knows it. */
export function describeChange(entry: Pick<AuditEntry, "method" | "path">): string {
  const line = `${entry.method} ${entry.path}`;
  for (const [pattern, words] of RULES) {
    const match = line.match(pattern);
    if (match) return words(match);
  }
  return line;
}

export type AuditOutcome = "all" | "done" | "refused" | "failed";

export function auditOutcome(status: number): Exclude<AuditOutcome, "all"> {
  if (status === 401 || status === 403) return "refused";
  return status >= 200 && status < 400 ? "done" : "failed";
}

export function filterAudit(entries: readonly AuditEntry[], query: string, outcome: AuditOutcome): AuditEntry[] {
  const needle = query.trim().toLowerCase();
  return entries.filter(
    (entry) =>
      (outcome === "all" || auditOutcome(entry.status) === outcome) &&
      (needle === "" ||
        entry.username.toLowerCase().includes(needle) ||
        entry.path.toLowerCase().includes(needle) ||
        describeChange(entry).toLowerCase().includes(needle)),
  );
}
