// How the workspaces are grouped in the navigation, and which ones a phone
// keeps in its bottom bar. Pure data; the shell draws it.

import type { IconName } from "../../ui/icons.js";
import type { Role } from "../../core/api-types.js";
import type { Workspace } from "../route.js";
import { canOpen } from "../team-model.js";

export interface NavGroup {
  readonly label: string;
  readonly items: readonly Workspace[];
}

/** Operate the plant, understand it, manage it; engineering tools last. */
export const NAV_GROUPS: readonly NavGroup[] = [
  { label: "Operate", items: ["fleet", "channels", "pipelines", "operate", "monitor"] },
  { label: "Understand", items: ["incidents", "health", "capacity", "stability"] },
  { label: "Manage", items: ["media", "changes", "team", "audit", "settings"] },
  { label: "Engineer", items: ["inspect", "logs", "system"] },
];

export const NAV_ICONS: Record<Workspace, IconName> = {
  fleet: "fleet",
  channels: "channels",
  pipelines: "pipelines",
  operate: "operate",
  inspect: "inspect",
  monitor: "monitor",
  incidents: "incidents",
  health: "health",
  logs: "logs",
  capacity: "capacity",
  stability: "stability",
  media: "media",
  system: "system",
  changes: "changes",
  team: "team",
  audit: "audit",
  settings: "settings",
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
