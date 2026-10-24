// One vocabulary for state colour. Every status in the console maps to a
// tone; the tone picks the colour, and the label always says the state in
// words, so colour is never the only signal.

import type { ReactNode } from "react";

export type Tone = "ok" | "warn" | "bad" | "info" | "neutral" | "accent";

export const TONE_TEXT: Record<Tone, string> = {
  ok: "text-success",
  warn: "text-warning",
  bad: "text-error",
  info: "text-info",
  neutral: "text-base-content/70",
  accent: "text-accent",
};

export const TONE_BG: Record<Tone, string> = {
  ok: "bg-success",
  warn: "bg-warning",
  bad: "bg-error",
  info: "bg-info",
  neutral: "bg-base-content/40",
  accent: "bg-accent",
};

// DaisyUI's status dot: the tone's colour with a little depth.
const STATUS: Record<Tone, string> = {
  ok: "status-success",
  warn: "status-warning",
  bad: "status-error",
  info: "status-info",
  neutral: "bg-base-content/40",
  accent: "status-accent",
};

const BADGE: Record<Tone, string> = {
  ok: "bg-success/15 text-success border-success/30",
  warn: "bg-warning/15 text-warning border-warning/30",
  bad: "bg-error/15 text-error border-error/30",
  info: "bg-info/15 text-info border-info/30",
  neutral: "bg-base-content/8 text-base-content/80 border-base-content/15",
  accent: "bg-accent/15 text-accent border-accent/30",
};

/** The worse of two tones, for roll-ups. */
export function worseTone(a: Tone, b: Tone): Tone {
  const rank: Record<Tone, number> = { bad: 0, warn: 1, info: 2, accent: 3, neutral: 4, ok: 5 };
  return rank[a] <= rank[b] ? a : b;
}

/** A utilization (0..1+) against the safe line (0.8) and saturation (1). */
export function utilizationTone(value: number | null | undefined, warnAt = 0.8, badAt = 1): Tone {
  if (value === null || value === undefined || !Number.isFinite(value)) return "neutral";
  if (value >= badAt) return "bad";
  if (value >= warnAt) return "warn";
  return "ok";
}

export function StatusDot(props: { tone: Tone; pulse?: boolean; className?: string }) {
  return (
    <span
      aria-hidden="true"
      className={`status inline-block size-2 shrink-0 rounded-full ${STATUS[props.tone]} ${props.pulse ? "motion-safe:animate-pulse" : ""} ${props.className ?? ""}`}
    />
  );
}

/** A short state label in its tone, optionally with a leading dot. */
export function StatusBadge(props: {
  tone: Tone;
  children: ReactNode;
  dot?: boolean;
  title?: string;
  className?: string;
}) {
  return (
    <span
      title={props.title}
      className={`inline-flex max-w-full items-center gap-1.5 truncate rounded-full border px-2 py-0.5 text-xs font-semibold leading-tight ${BADGE[props.tone]} ${props.className ?? ""}`}
    >
      {props.dot && <StatusDot tone={props.tone} />}
      {props.children}
    </span>
  );
}
