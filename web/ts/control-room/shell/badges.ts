// Live counts the navigation shows next to a workspace: degraded outputs on
// Fleet, open alerts on Incidents. A count the shell cannot know is absent,
// never zero.

import type { RuntimeState } from "../../core/generated/runtime-state.js";
import type { Tone } from "../../ui/tone.js";
import type { Workspace } from "../route.js";

export interface NavBadge {
  readonly count: number;
  readonly tone: Tone;
  readonly title: string;
}

export function navBadges(
  state: RuntimeState | null,
  openAlerts: { critical: number; total: number } | null,
): Partial<Record<Workspace, NavBadge>> {
  const badges: Partial<Record<Workspace, NavBadge>> = {};
  if (state) {
    let degraded = 0;
    for (const output of state.outputs) if (output.desired === "running" && output.diagnosis) degraded += 1;
    if (degraded > 0) badges.fleet = { count: degraded, tone: "warn", title: `${degraded} outputs degraded` };
    badges.pipelines = { count: state.pipelines.length, tone: "neutral", title: `${state.pipelines.length} pipelines` };
  }
  if (openAlerts && openAlerts.total > 0) {
    badges.incidents = {
      count: openAlerts.total,
      tone: openAlerts.critical > 0 ? "bad" : "warn",
      title: `${openAlerts.total} open alerts, ${openAlerts.critical} critical`,
    };
  }
  return badges;
}

/** `1234` as `1.2k` so a badge stays narrow. */
export function compactCount(count: number): string {
  if (count < 1000) return String(count);
  if (count < 10_000) return `${(count / 1000).toFixed(1)}k`;
  return `${Math.round(count / 1000)}k`;
}
