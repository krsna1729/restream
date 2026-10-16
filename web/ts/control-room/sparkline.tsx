import { max, sparkline } from "./sample-window.js";

const PLOT_WIDTH = 600;
const PLOT_HEIGHT = 40;

/** One signal of a sampled window, from its first sample to its last. */
export function Sparkline<T extends { readonly atMs: number }>(props: {
  history: readonly T[];
  pick: (sample: T) => number | null;
  label: string;
  /** A fixed top of the scale (a percentage: 100); else the window's max. */
  top?: number;
  /** Instants to mark with a vertical cursor (incident onsets). */
  marks?: readonly number[];
  /** A second window overlaid on its own time span and the same scale, so
   * the two are compared shape to shape. */
  compare?: readonly T[];
}) {
  const first = props.history[0];
  const last = props.history[props.history.length - 1];
  const top =
    props.top ??
    (props.compare
      ? max([...props.history, ...props.compare].map(props.pick))
      : null);
  const draw = (history: readonly T[]) => {
    const from = history[0];
    const to = history[history.length - 1];
    return from === undefined || to === undefined
      ? []
      : sparkline(history, props.pick, from.atMs, to.atMs, PLOT_WIDTH, PLOT_HEIGHT, top);
  };
  const span = first && last ? Math.max(1, last.atMs - first.atMs) : 0;
  const marks =
    first && last
      ? (props.marks ?? []).filter((at) => at >= first.atMs && at <= last.atMs)
      : [];
  return (
    <svg
      viewBox={`0 0 ${PLOT_WIDTH} ${PLOT_HEIGHT}`}
      preserveAspectRatio="none"
      className="h-10 w-full rounded bg-base-100"
      role="img"
      aria-label={props.label}
    >
      {props.compare &&
        draw(props.compare).map((points) => (
          <polyline
            key={`compare-${points}`}
            data-series="compare"
            points={points}
            fill="none"
            stroke="currentColor"
            strokeOpacity="0.35"
            strokeWidth="2"
            strokeDasharray="4 3"
            vectorEffect="non-scaling-stroke"
          />
        ))}
      {draw(props.history).map((points) => (
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
      {first &&
        marks.map((at) => {
          const x = ((at - first.atMs) / span) * PLOT_WIDTH;
          return (
            <line
              key={at}
              data-series="incident"
              x1={x}
              x2={x}
              y1={0}
              y2={PLOT_HEIGHT}
              className="stroke-warning"
              strokeWidth="2"
              vectorEffect="non-scaling-stroke"
            />
          );
        })}
    </svg>
  );
}
