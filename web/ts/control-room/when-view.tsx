// One way to show a moment everywhere: a <time> element whose tooltip
// gives the full instant in the viewer's zone and UTC. A shared 30 s clock
// keeps every "3 min ago" current without a timer per row.

import { useSyncExternalStore } from "react";

import { absoluteTime, fullTime, relativeTime } from "./when.js";

const listeners = new Set<() => void>();
let now = Date.now();
let timer: number | null = null;

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  if (timer === null) {
    timer = window.setInterval(() => {
      now = Date.now();
      for (const notify of listeners) notify();
    }, 30_000);
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && timer !== null) {
      window.clearInterval(timer);
      timer = null;
    }
  };
}

/** Now, to the half minute, shared by every caller. */
export function useClock(): number {
  return useSyncExternalStore(subscribe, () => now);
}

/**
 * `ago`: "3 min ago". `at`: "Today 15:32 BST". `both`: the clock time with
 * the relative time after it, quieter. The tooltip always has the rest.
 */
export function When(props: {
  atMs: number | null | undefined;
  show?: "ago" | "at" | "both";
  seconds?: boolean;
  noZone?: boolean;
  className?: string;
}) {
  const clock = useClock();
  if (props.atMs === null || props.atMs === undefined || !Number.isFinite(props.atMs)) return <span className={props.className}>--</span>;
  const at = props.atMs;
  const reference = Math.max(clock, Date.now() - 30_000);
  const relative = relativeTime(at, reference);
  const absolute = absoluteTime(at, reference, { seconds: props.seconds, noZone: props.noZone });
  const show = props.show ?? "both";
  return (
    <time dateTime={new Date(at).toISOString()} title={`${fullTime(at)} · ${relative}`} className={`tabular ${props.className ?? ""}`}>
      {show === "ago" ? relative : absolute}
      {show === "both" && <span className="text-base-content/60"> · {relative}</span>}
    </time>
  );
}
