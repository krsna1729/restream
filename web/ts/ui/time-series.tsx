// A multi-series time chart: lines (optionally one area), a fixed or shared
// scale, shaded phase bands, labelled event cursors, an end-of-line dot and
// a hover readout. A missing value is a gap, never a zero. Pure SVG, so it
// themes with currentColor and token classes.

import { useMemo, useRef, useState } from "react";
import type { PointerEvent, ReactNode } from "react";

import { max, sparkline } from "./series-geometry.js";

export interface Series<T> {
  readonly id: string;
  readonly label: string;
  readonly pick: (sample: T) => number | null;
  /** Tailwind stroke/fill/text colour classes, e.g. `text-success`. */
  readonly color: string;
  readonly dashed?: boolean;
  readonly area?: boolean;
  readonly format?: (value: number) => string;
}

export interface Band {
  readonly fromMs: number;
  readonly toMs: number;
  readonly tone: "warn" | "bad" | "info";
  readonly label?: string;
}

/** A horizontal reference line (a target or a limit). */
export interface Threshold {
  readonly value: number;
  readonly label: string;
  readonly tone: "warn" | "bad" | "ok";
}

export interface Mark {
  readonly atMs: number;
  /** Names the kind of mark for tests and assistive tech (`incident`). */
  readonly kind?: string;
  readonly label?: string;
  readonly tone?: "warn" | "bad" | "ok" | "info";
}

const W = 800;
const BAND_FILL = { warn: "fill-warning/12", bad: "fill-error/12", info: "fill-info/10" };
const MARK_STROKE = { warn: "stroke-warning", bad: "stroke-error", ok: "stroke-success", info: "stroke-info" };

const COMPACT = new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 });

function compact(value: number): string {
  return COMPACT.format(value);
}

/** The x of a polyline's last point. */
function lastX(points: string): string {
  const all = points.split(" ");
  return all[all.length - 1]?.split(",")[0] ?? "0";
}

function niceTop(value: number): number {
  if (value <= 0) return 1;
  const exponent = 10 ** Math.floor(Math.log10(value));
  const fraction = value / exponent;
  const nice = fraction <= 1 ? 1 : fraction <= 2 ? 2 : fraction <= 2.5 ? 2.5 : fraction <= 5 ? 5 : 10;
  return nice * exponent;
}

export function TimeSeries<T extends { readonly atMs: number }>(props: {
  samples: readonly T[];
  series: readonly Series<T>[];
  label: string;
  /** Plot height in px. */
  height?: number;
  /** Fixed top of the scale (100 for a percentage); else a nice max. */
  top?: number;
  bands?: readonly Band[];
  marks?: readonly Mark[];
  /** Overlay drawn on the same scale, dashed and faint (compare windows). */
  ghost?: readonly T[];
  /** Right-aligned header content (time, speed, verdict). */
  aside?: ReactNode;
  /** A centred callout over the plot ("STALLED · 0 delivered"). */
  callout?: ReactNode;
  showLegend?: boolean;
  /** The x range; defaults to the samples' first and last. */
  fromMs?: number;
  toMs?: number;
  /** Tick labels; defaults to compact numbers (1.2k, 4.5M). */
  axisFormat?: (value: number) => string;
  thresholds?: readonly Threshold[];
}) {
  const height = props.height ?? 140;
  const svgRef = useRef<SVGSVGElement | null>(null);
  const [hover, setHover] = useState<number | null>(null);
  const first = props.samples[0];
  const last = props.samples[props.samples.length - 1];
  const fromMs = props.fromMs ?? first?.atMs ?? 0;
  const toMs = props.toMs ?? last?.atMs ?? 1;
  const span = Math.max(1, toMs - fromMs);
  const top = useMemo(() => {
    if (props.top !== undefined) return props.top;
    const values = props.series.flatMap((series) =>
      [...props.samples, ...(props.ghost ?? [])].map(series.pick),
    );
    const ceiling = Math.max(max(values) ?? 0, ...(props.thresholds ?? []).map((threshold) => threshold.value));
    return niceTop(ceiling * 1.08);
  }, [props.samples, props.series, props.ghost, props.top, props.thresholds]);
  const x = (atMs: number) => ((atMs - fromMs) / span) * W;
  const y = (value: number) => height - (value / top) * height;
  const hovered = hover === null ? null : props.samples[hover] ?? null;

  const onMove = (event: PointerEvent<SVGSVGElement>) => {
    const svg = svgRef.current;
    if (!svg || props.samples.length === 0) return;
    const rect = svg.getBoundingClientRect();
    const atMs = fromMs + ((event.clientX - rect.left) / rect.width) * span;
    let best = 0;
    for (let index = 1; index < props.samples.length; index += 1) {
      if (Math.abs(props.samples[index].atMs - atMs) < Math.abs(props.samples[best].atMs - atMs)) best = index;
    }
    setHover(best);
  };

  const gridLines = [0, 0.25, 0.5, 0.75, 1];
  const fmt = (series: Series<T>, value: number | null) =>
    value === null ? "--" : series.format ? series.format(value) : value.toLocaleString(undefined, { maximumFractionDigits: 1 });

  return (
    <figure className="min-w-0">
      {(props.showLegend !== false || props.aside) && (
        <figcaption className="mb-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs">
          {props.showLegend !== false &&
            props.series.map((series) => (
              <span key={series.id} className="inline-flex items-center gap-1.5 text-base-content/80">
                <svg width="18" height="6" aria-hidden="true" className={series.color}>
                  <line x1="0" x2="18" y1="3" y2="3" stroke="currentColor" strokeWidth="2.5" strokeDasharray={series.dashed ? "4 3" : undefined} />
                </svg>
                {series.label}
                {hovered && <b className="tabular font-semibold text-base-content">{fmt(series, series.pick(hovered))}</b>}
              </span>
            ))}
          {props.aside && <span className="ml-auto text-base-content/70 tabular">{props.aside}</span>}
        </figcaption>
      )}
      <div className="relative grid grid-cols-[3.5rem_minmax(0,1fr)]">
        <div className="relative text-right text-[0.7rem] text-base-content/60 tabular" style={{ height }}>
          {gridLines.map((fraction) => (
            <span key={fraction} className="absolute right-2 -translate-y-1/2 whitespace-nowrap" style={{ top: `${(1 - fraction) * 100}%` }}>
              {(props.axisFormat ?? compact)(top * fraction)}
            </span>
          ))}
        </div>
        <svg
          ref={svgRef}
          viewBox={`0 0 ${W} ${height}`}
          preserveAspectRatio="none"
          className="w-full touch-none overflow-visible"
          style={{ height }}
          role="img"
          aria-label={props.label}
          onPointerMove={onMove}
          onPointerLeave={() => setHover(null)}
        >
          {gridLines.map((fraction) => (
            <line key={fraction} x1={0} x2={W} y1={y(top * fraction)} y2={y(top * fraction)} className="stroke-base-content/8" strokeWidth={1} vectorEffect="non-scaling-stroke" />
          ))}
          {(props.bands ?? []).map((band) => (
            <rect key={`${band.fromMs}-${band.tone}`} x={x(Math.max(fromMs, band.fromMs))} width={Math.max(0, x(Math.min(toMs, band.toMs)) - x(Math.max(fromMs, band.fromMs)))} y={0} height={height} className={BAND_FILL[band.tone]} />
          ))}
          {props.ghost &&
            props.series.map((series) =>
              sparkline(props.ghost ?? [], series.pick, props.ghost?.[0]?.atMs ?? fromMs, props.ghost?.[props.ghost.length - 1]?.atMs ?? toMs, W, height, top).map((points) => (
                <polyline key={`ghost-${series.id}-${points}`} data-series="compare" points={points} fill="none" stroke="currentColor" strokeOpacity={0.35} strokeWidth={1.5} strokeDasharray="4 3" vectorEffect="non-scaling-stroke" className={series.color} />
              )),
            )}
          {props.series.map((series) =>
            sparkline(props.samples, series.pick, fromMs, toMs, W, height, top).map((points) => (
              <g key={`${series.id}-${points}`} className={series.color}>
                {series.area && (
                  <polygon
                    points={`${points.split(" ")[0].split(",")[0]},${height} ${points} ${lastX(points)},${height}`}
                    fill="currentColor"
                    fillOpacity={0.12}
                  />
                )}
                <polyline points={points} fill="none" stroke="currentColor" strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" strokeDasharray={series.dashed ? "6 4" : undefined} vectorEffect="non-scaling-stroke" />
              </g>
            )),
          )}
          {(props.thresholds ?? []).map((threshold) => (
            <line key={`threshold-${threshold.value}`} data-series="threshold" x1={0} x2={W} y1={y(threshold.value)} y2={y(threshold.value)} className={MARK_STROKE[threshold.tone]} strokeWidth={1.25} strokeDasharray="6 4" vectorEffect="non-scaling-stroke" />
          ))}
          {(props.marks ?? []).filter((mark) => mark.atMs >= fromMs && mark.atMs <= toMs).map((mark) => (
            <line key={`mark-${mark.atMs}`} data-series={mark.kind ?? "mark"} x1={x(mark.atMs)} x2={x(mark.atMs)} y1={0} y2={height} className={MARK_STROKE[mark.tone ?? "warn"]} strokeWidth={1.5} strokeDasharray="3 3" vectorEffect="non-scaling-stroke" />
          ))}
          {hovered && <line x1={x(hovered.atMs)} x2={x(hovered.atMs)} y1={0} y2={height} className="stroke-base-content/40" strokeWidth={1} vectorEffect="non-scaling-stroke" />}
        </svg>
        {/* Labels and end dots are HTML so they do not stretch with the plot. */}
        <div className="pointer-events-none absolute inset-y-0 right-0 left-[3.5rem]">
          {(props.marks ?? []).filter((mark) => mark.label && mark.atMs >= fromMs && mark.atMs <= toMs).map((mark) => (
            <span key={`label-${mark.atMs}`} className="absolute top-0 -translate-x-full pr-1 text-[0.7rem] font-semibold whitespace-nowrap text-base-content/80" style={{ left: `${(x(mark.atMs) / W) * 100}%` }}>
              {mark.label}
            </span>
          ))}
          {last &&
            props.series.map((series) => {
              const value = series.pick(last);
              if (value === null || series.dashed) return null;
              return (
                <span key={`dot-${series.id}`} className={`absolute size-2 -translate-1/2 rounded-full bg-current ${series.color}`} style={{ left: `${(x(last.atMs) / W) * 100}%`, top: y(value) }} />
              );
            })}
          {(props.thresholds ?? []).map((threshold) => (
            <span key={`threshold-label-${threshold.value}`} className="absolute right-1 -translate-y-full text-[0.7rem] font-semibold text-base-content/70" style={{ top: y(threshold.value) }}>
              {threshold.label}
            </span>
          ))}
          {props.callout && (
            <div className="absolute inset-0 flex items-center justify-center">{props.callout}</div>
          )}
        </div>
      </div>
    </figure>
  );
}
