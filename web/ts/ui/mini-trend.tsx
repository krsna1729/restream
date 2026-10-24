// A row-sized trend: the last minute of one rate as a line, its scale the
// window's own range, so the shape (steady, dipping, gone) reads at a
// glance beside the number.

const WIDTH = 64;
const HEIGHT = 18;

/** `values` oldest first; fewer than two draw nothing. */
export function MiniTrend(props: { values: readonly number[] | undefined; label: string; className?: string }) {
  const values = props.values ?? [];
  if (values.length < 2) return <span aria-hidden="true" className={`inline-block h-[18px] w-16 ${props.className ?? ""}`} />;
  const low = Math.min(...values);
  const high = Math.max(...values);
  const span = high - low || 1;
  const step = WIDTH / (values.length - 1);
  // A flat line sits in the middle rather than on the floor.
  const y = (value: number) => (high === low ? HEIGHT / 2 : HEIGHT - 1 - ((value - low) / span) * (HEIGHT - 2));
  const points = values.map((value, index) => `${(index * step).toFixed(1)},${y(value).toFixed(1)}`).join(" ");
  return (
    <svg viewBox={`0 0 ${WIDTH} ${HEIGHT}`} className={`inline-block h-[18px] w-16 text-primary ${props.className ?? ""}`} role="img" aria-label={props.label}>
      <polyline points={points} fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}
