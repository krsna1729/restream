// How the workspaces are grouped in the navigation, and which ones a phone
// keeps in its bottom bar. Pure data; the shell draws it.

import type { IconName } from "../../ui/icons.js";
import type { Workspace } from "../route.js";

export interface NavGroup {
  readonly label: string;
  readonly items: readonly Workspace[];
}

/** Operate the plant, understand it, manage it; engineering tools last. */
export const NAV_GROUPS: readonly NavGroup[] = [
  { label: "Operate", items: ["fleet", "channels", "pipelines", "operate", "monitor"] },
  { label: "Understand", items: ["incidents", "capacity"] },
  { label: "Manage", items: ["media", "changes", "settings"] },
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
  logs: "logs",
  capacity: "capacity",
  media: "media",
  system: "system",
  changes: "changes",
  settings: "settings",
};

/** The phone's bottom bar: the on-call path first; the rest under More. */
export const MOBILE_TABS: readonly Workspace[] = ["fleet", "incidents", "channels", "operate"];

/** Workspaces that show one pipeline and need one picked. */
export const PIPELINE_WORKSPACES: ReadonlySet<Workspace> = new Set(["operate", "inspect", "monitor"]);
