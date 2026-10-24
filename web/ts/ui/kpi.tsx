// Numbers at a glance: KPI tiles (label, value, foot), threshold-toned
// meters, and key-value lists. A value that is not known renders as `--`,
// never as zero.

import type { ReactNode } from "react";

import { TONE_BG, TONE_TEXT, utilizationTone } from "./tone.js";
import type { Tone } from "./tone.js";

export function Kpi(props: {
  label: ReactNode;
  value: ReactNode;
  foot?: ReactNode;
  /** Colours the value: a KPI that is itself a judgement. */
  tone?: Tone;
  /** A 0..1 fill under the value. */
  meter?: number | null;
  meterTone?: Tone;
  onClick?: () => void;
  title?: string;
}) {
  const Tag = props.onClick ? "button" : "div";
  return (
    <Tag
      type={props.onClick ? "button" : undefined}
      onClick={props.onClick}
      title={props.title}
      role={props.onClick ? undefined : "group"}
      aria-label={typeof props.label === "string" ? props.label : undefined}
      className={`flex min-w-0 flex-col gap-1 rounded-box border border-base-content/10 bg-base-200 px-4 py-3 text-left ${props.onClick ? "transition-colors hover:border-base-content/30" : ""}`}
    >
      <span className="kicker truncate">{props.label}</span>
      {props.value === "--" ? (
        // Unknown: a quiet dash, not a big "- -".
        <span className="text-2xl leading-tight font-semibold text-base-content/40" aria-label="unknown">
          —
        </span>
      ) : (
        <span className={`tabular truncate text-2xl leading-tight font-semibold ${props.tone ? TONE_TEXT[props.tone] : ""}`}>
          {props.value}
        </span>
      )}
      {props.meter !== undefined && (
        <Meter value={props.meter} tone={props.meterTone} label={typeof props.label === "string" ? props.label : "level"} />
      )}
      {props.foot && <span className="line-clamp-2 text-xs text-base-content/70">{props.foot}</span>}
    </Tag>
  );
}

/** KPI tiles that reflow: 2 across on a phone, up to `max` on wide screens. */
export function KpiRow(props: { children: ReactNode; max?: 3 | 4 | 5 | 6 }) {
  // Columns follow the room the row has (a container query), not the
  // window: the same row fits a full page, a side column or a phone.
  const cols = {
    3: "@2xl:grid-cols-3",
    4: "@2xl:grid-cols-4",
    5: "@2xl:grid-cols-3 @4xl:grid-cols-5",
    6: "@2xl:grid-cols-3 @4xl:grid-cols-6",
  }[props.max ?? 4];
  return (
    <div className="@container">
      <div className={`grid grid-cols-2 gap-3 ${cols}`}>{props.children}</div>
    </div>
  );
}

/** A thin fill bar, toned by threshold unless a tone is given. */
export function Meter(props: { value: number | null | undefined; tone?: Tone; label: string; warnAt?: number; badAt?: number; className?: string }) {
  const value = props.value ?? null;
  const tone = props.tone ?? utilizationTone(value, props.warnAt, props.badAt);
  const pct = value === null || !Number.isFinite(value) ? 0 : Math.max(0, Math.min(1, value)) * 100;
  return (
    <div
      role="meter"
      aria-label={props.label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={value === null ? undefined : Math.round(value * 100)}
      className={`h-1.5 w-full overflow-hidden rounded-full bg-base-content/10 ${props.className ?? ""}`}
    >
      <div className={`h-full rounded-full ${TONE_BG[tone]} transition-[width] duration-300`} style={{ width: `${pct}%` }} />
    </div>
  );
}

/** A labelled meter row: label, bar, value. */
export function MeterRow(props: { label: string; value: number | null | undefined; display: string; tone?: Tone; warnAt?: number; badAt?: number }) {
  return (
    <div className="grid grid-cols-[minmax(0,7rem)_1fr_auto] items-center gap-3 text-sm">
      <span className="truncate text-base-content/80">{props.label}</span>
      <Meter value={props.value} tone={props.tone} label={props.label} warnAt={props.warnAt} badAt={props.badAt} />
      <span className="tabular w-14 text-right">{props.display}</span>
    </div>
  );
}

/** Labelled values in two columns that stack on narrow panels. */
export function KeyValue(props: { rows: ReadonlyArray<readonly [ReactNode, ReactNode] | null | false>; mono?: boolean; className?: string }) {
  const rows = props.rows.filter((row): row is readonly [ReactNode, ReactNode] => !!row);
  return (
    <dl className={`grid grid-cols-[minmax(0,auto)_minmax(0,1fr)] gap-x-4 gap-y-1.5 text-sm ${props.className ?? ""}`}>
      {rows.map(([key, value], index) => (
        <div key={index} className="contents">
          <dt className="truncate text-base-content/70">{key}</dt>
          <dd className={`tabular min-w-0 text-right break-words ${props.mono ? "font-mono text-xs" : ""}`}>{value}</dd>
        </div>
      ))}
    </dl>
  );
}
