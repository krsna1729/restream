// The schedule as people read it: occurrences laid out by local day, the
// action in words, and how the last run went. Occurrences follow the
// engine's rule (`application/schedules.rs`): the first time, then every
// 24 h or 7 days of UTC. Pure.

import type { Schedule, ScheduleAction, ScheduleRepeat } from "../core/api-types.js";

const DAY_MS = 86_400_000;
const PERIOD: Record<ScheduleRepeat, number | null> = { once: null, daily: DAY_MS, weekly: 7 * DAY_MS };

export const ACTION_LABEL: Record<ScheduleAction, string> = {
  startOutputs: "Go live",
  stopOutputs: "Stop destinations",
  startFile: "Play file source",
  stopFile: "Stop file source",
};

export const REPEAT_LABEL: Record<ScheduleRepeat, string> = {
  once: "Once",
  daily: "Every day",
  weekly: "Every week",
};

/** Occurrences in [fromMs, toMs), in order. */
export function occurrences(atMs: number, repeat: ScheduleRepeat, fromMs: number, toMs: number): number[] {
  const period = PERIOD[repeat];
  if (period === null) return atMs >= fromMs && atMs < toMs ? [atMs] : [];
  const out: number[] = [];
  let k = atMs >= fromMs ? 0 : Math.ceil((fromMs - atMs) / period);
  for (let at = atMs + k * period; at < toMs; k += 1, at = atMs + k * period) out.push(at);
  return out;
}

export interface AgendaItem {
  readonly schedule: Schedule;
  readonly atMs: number;
}

export interface AgendaDay {
  /** Local midnight. */
  readonly dayMs: number;
  readonly items: readonly AgendaItem[];
}

/** Local midnight of the day holding `ms`. */
export function localDay(ms: number): number {
  const date = new Date(ms);
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

/** `days` local days from the one holding `nowMs`, each with its enabled
 * occurrences in time order (past ones of today included). */
export function agenda(schedules: readonly Schedule[], nowMs: number, days: number): AgendaDay[] {
  const out: AgendaDay[] = [];
  let day = localDay(nowMs);
  for (let index = 0; index < days; index += 1) {
    const next = new Date(day);
    next.setDate(next.getDate() + 1);
    const end = next.getTime();
    const items: AgendaItem[] = [];
    for (const schedule of schedules) {
      if (!schedule.enabled) continue;
      for (const atMs of occurrences(schedule.atMs, schedule.repeat, day, end)) items.push({ schedule, atMs });
    }
    items.sort((a, b) => a.atMs - b.atMs || a.schedule.name.localeCompare(b.schedule.name));
    out.push({ dayMs: day, items });
    day = end;
  }
  return out;
}

/** The soonest enabled occurrence after now. */
export function nextUp(schedules: readonly Schedule[]): Schedule | null {
  return schedules
    .filter((schedule) => schedule.enabled && schedule.nextAtMs !== null)
    .reduce<Schedule | null>((best, schedule) => (best === null || (schedule.nextAtMs ?? Infinity) < (best.nextAtMs ?? Infinity) ? schedule : best), null);
}

export type RunTone = "ok" | "bad" | "warn" | "neutral";

export function resultTone(result: string | null): RunTone {
  if (!result) return "neutral";
  if (result.startsWith("ok")) return "ok";
  if (result.startsWith("failed")) return "bad";
  if (result.startsWith("skipped")) return "warn";
  return "neutral";
}

/** `YYYY-MM-DDTHH:MM` in local time, for a datetime-local input. */
export function toLocalInput(ms: number): string {
  const date = new Date(ms);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** A datetime-local value as UTC ms; NaN when empty or malformed. */
export function fromLocalInput(value: string): number {
  const match = value.match(/^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/);
  if (!match) return Number.NaN;
  const [, year, month, day, hour, minute] = match.map(Number);
  return new Date(year, month - 1, day, hour, minute).getTime();
}

/** "in 2 h 5 min", "in 40 s", "now". */
export function untilLabel(atMs: number, nowMs: number): string {
  const seconds = Math.round((atMs - nowMs) / 1000);
  if (seconds <= 0) return "now";
  if (seconds < 60) return `in ${seconds} s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `in ${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `in ${hours} h${minutes % 60 ? ` ${minutes % 60} min` : ""}`;
  return `in ${Math.round(hours / 24)} days`;
}
