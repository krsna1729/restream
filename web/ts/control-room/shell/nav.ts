// How the workspaces are grouped in the navigation, and which ones a phone
// keeps in its bottom bar. Pure data; the shell draws it.

import type { IconName } from "../../ui/icons.js";
import type { Role } from "../../core/api-types.js";
import type { Workspace } from "../route.js";
import { canOpen } from "../team-model.js";

export interface NavGroup {
  readonly label: string;
  readonly items: readonly Workspace[];
  /** Folded until opened (remembered per browser), and open while one of
   * its workspaces is the current one. */
  readonly folded?: boolean;
}

/**
 * What most people need first: run streams, see trouble, manage the
 * basics, learn. The deeper tools (capacity, stability, inspection, logs,
 * the host, changes, the audit trail, the architecture) sit in one folded
 * group, a click away, so a first look is not a wall of words.
 */
export const NAV_GROUPS: readonly NavGroup[] = [
  { label: "Operate", items: ["fleet", "channels", "pipelines", "operate", "monitor", "schedule"] },
  { label: "Understand", items: ["incidents", "health"] },
  { label: "Manage", items: ["media", "notifications", "team", "settings"] },
  { label: "Learn", items: ["guides"] },
  { label: "More tools", folded: true, items: ["capacity", "stability", "inspect", "logs", "system", "changes", "audit", "architecture"] },
];

export const NAV_ICONS: Record<Workspace, IconName> = {
  fleet: "fleet",
  channels: "channels",
  pipelines: "pipelines",
  operate: "operate",
  inspect: "inspect",
  monitor: "monitor",
  schedule: "schedule",
  incidents: "incidents",
  health: "health",
  logs: "logs",
  capacity: "capacity",
  stability: "stability",
  media: "media",
  system: "system",
  changes: "changes",
  notifications: "bell",
  team: "team",
  audit: "audit",
  settings: "settings",
  guides: "book",
  architecture: "layers",
};

/** The phone's bottom bar: the on-call path first; the rest under More. */
export const MOBILE_TABS: readonly Workspace[] = ["fleet", "incidents", "channels", "operate"];

/** Workspaces that show one pipeline and need one picked. */
export const PIPELINE_WORKSPACES: ReadonlySet<Workspace> = new Set(["operate", "health", "inspect", "monitor"]);

/** The groups a role may open; a group left empty is dropped. */
export function navGroupsFor(role: Role): NavGroup[] {
  return NAV_GROUPS.map((group) => ({ ...group, items: group.items.filter((id) => canOpen(role, id)) })).filter(
    (group) => group.items.length > 0,
  );
}
