import { sparkline } from "./sample-window.js";

const PLOT_WIDTH = 600;
const PLOT_HEIGHT = 40;

/** One signal of a sampled window, from its first sample to its last. */
export function Sparkline<T extends { readonly atMs: number }>(props: {
  history: readonly T[];
  pick: (sample: T) => number | null;
  label: string;
  /** A fixed top of the scale (a percentage: 100); else the window's max. */
  top?: number;
}) {
  const first = props.history[0];
  const last = props.history[props.history.length - 1];
  const lines =
    first === undefined || last === undefined
      ? []
      : sparkline(
          props.history,
          props.pick,
          first.atMs,
          last.atMs,
          PLOT_WIDTH,
          PLOT_HEIGHT,
          props.top ?? null,
        );
  return (
    <svg
      viewBox={`0 0 ${PLOT_WIDTH} ${PLOT_HEIGHT}`}
      preserveAspectRatio="none"
      className="h-10 w-full rounded bg-base-100"
      role="img"
      aria-label={props.label}
    >
      {lines.map((points) => (
        <polyline
          key={points}
          points={points}
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          vectorEffect="non-scaling-stroke"
        />
      ))}
    </svg>
  );
}
