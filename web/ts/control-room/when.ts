// Times as people say them: "3 min ago", "in 2 h", and the clock time in
// the viewer's zone ("Today 15:32 BST", "Mon 21:04 BST", "23 Oct, 15:32
// BST"). Pure: the zone is a parameter so tests pin it; the page passes the
// browser's.

export interface WhenOptions {
  /** An IANA zone; the browser's when omitted. */
  readonly timeZone?: string;
  /** Show seconds (logs, audit). */
  readonly seconds?: boolean;
  /** Leave the zone name off (a column header already says it). */
  readonly noZone?: boolean;
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** "just now", "3 min ago", "in 2 h", "yesterday", "4 d ago". */
export function relativeTime(atMs: number, nowMs: number): string {
  const delta = atMs - nowMs;
  const span = Math.abs(delta);
  const future = delta > 0;
  if (span < 45_000) return future ? "in a moment" : "just now";
  let text: string;
  if (span < 90 * MINUTE) text = `${Math.max(1, Math.round(span / MINUTE))} min`;
  else if (span < 36 * HOUR) text = `${Math.round(span / HOUR)} h`;
  else {
    const days = Math.round(span / DAY);
    if (days === 1 || (days === 2 && span < 48 * HOUR)) return future ? "tomorrow" : "yesterday";
    text = `${days} d`;
  }
  return future ? `in ${text}` : `${text} ago`;
}

/** A span of time: "45 s", "12 min", "1 h 23 min", "2 d 4 h". */
export function duration(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds} s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return minutes % 60 ? `${hours} h ${minutes % 60} min` : `${hours} h`;
  const days = Math.floor(hours / 24);
  return hours % 24 ? `${days} d ${hours % 24} h` : `${days} d`;
}

function parts(atMs: number, timeZone: string | undefined, options: Intl.DateTimeFormatOptions) {
  const out: Record<string, string> = {};
  for (const part of new Intl.DateTimeFormat("en-GB", { ...options, timeZone }).formatToParts(atMs)) out[part.type] = part.value;
  return out;
}

/** The calendar day of an instant in a zone, as a sortable number. */
function dayNumber(atMs: number, timeZone: string | undefined): number {
  const day = parts(atMs, timeZone, { year: "numeric", month: "2-digit", day: "2-digit" });
  return Date.UTC(Number(day.year), Number(day.month) - 1, Number(day.day)) / DAY;
}

/**
 * The clock time, with as much date as the distance from now needs:
 * "15:32 BST" today, "Yesterday 21:04 BST", "Mon 21:04 BST" within a week,
 * "23 Oct, 15:32 BST" this year, "23 Oct 2025, 15:32 BST" before.
 */
export function absoluteTime(atMs: number, nowMs: number, options: WhenOptions = {}): string {
  const { timeZone } = options;
  const clock = parts(atMs, timeZone, {
    hour: "2-digit",
    minute: "2-digit",
    second: options.seconds ? "2-digit" : undefined,
    hourCycle: "h23",
    timeZoneName: options.noZone ? undefined : "short",
  });
  const time = `${clock.hour}:${clock.minute}${options.seconds ? `:${clock.second}` : ""}${options.noZone ? "" : ` ${clock.timeZoneName}`}`;
  const days = dayNumber(atMs, timeZone) - dayNumber(nowMs, timeZone);
  if (days === 0) return `Today ${time}`;
  if (days === -1) return `Yesterday ${time}`;
  if (days === 1) return `Tomorrow ${time}`;
  const date = parts(atMs, timeZone, { weekday: "short", day: "numeric", month: "short", year: "numeric" });
  if (Math.abs(days) < 7) return `${date.weekday} ${time}`;
  const sameYear = date.year === parts(nowMs, timeZone, { year: "numeric" }).year;
  return `${date.day} ${date.month}${sameYear ? "" : ` ${date.year}`}, ${time}`;
}

/** The full instant for a tooltip: the viewer's zone and UTC. */
export function fullTime(atMs: number, timeZone?: string): string {
  const local = new Intl.DateTimeFormat("en-GB", {
    weekday: "short",
    day: "numeric",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
    timeZoneName: "short",
    timeZone,
  }).format(atMs);
  return `${local} (${new Date(atMs).toISOString().slice(11, 19)} UTC)`;
}

/** The viewer's zone name now ("BST", "GMT+5:30"), for column headers. */
export function zoneName(nowMs: number, timeZone?: string): string {
  return parts(nowMs, timeZone, { hour: "2-digit", timeZoneName: "short" }).timeZoneName ?? "local";
}

/** Time left, to the second while it matters: "45 s", "1 min 39 s", "2 h 5 min". */
export function countdown(ms: number): string {
  const seconds = Math.max(0, Math.ceil(ms / 1000));
  if (seconds < 60) return `${seconds} s`;
  if (seconds < 3600) return seconds % 60 ? `${Math.floor(seconds / 60)} min ${seconds % 60} s` : `${seconds / 60} min`;
  return duration(seconds * 1000);
}

/** A retrying destination's state: "Attempt 7 · next try in 1 min 39 s". */
export function retryWords(attempts: number, untilMs: number): string {
  return `Attempt ${attempts} · ${untilMs > 500 ? `next try in ${countdown(untilMs)}` : "trying again now"}`;
}
