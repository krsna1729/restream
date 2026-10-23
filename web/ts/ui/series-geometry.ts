// Geometry for sampled series: SVG polyline segments that break at gaps,
// shared by the chart primitive and the control room's sparklines.

export function max(values: Iterable<number | null>): number | null {
  let best: number | null = null;
  for (const value of values) {
    if (value !== null && (best === null || value > best)) best = value;
  }
  return best;
}

/**
 * SVG polyline point lists for one signal over `[fromMs, toMs]`, scaled to
 * `width` x `height` with 0 at the bottom and the window's maximum at the
 * top (or `top`, when the signal has a fixed scale). A missing value breaks
 * the line rather than drawing a zero; a lone value between gaps is a
 * zero-length segment, drawn as a dot by a round cap.
 */
export function sparkline<T extends { readonly atMs: number }>(
  history: readonly T[],
  pick: (sample: T) => number | null,
  fromMs: number,
  toMs: number,
  width: number,
  height: number,
  top: number | null = null,
): readonly string[] {
  const scale = top ?? max(history.map(pick)) ?? 0;
  const span = Math.max(1, toMs - fromMs);
  const segments: string[][] = [[]];
  for (const sample of history) {
    const value = pick(sample);
    if (value === null) {
      segments.push([]);
      continue;
    }
    const x = ((sample.atMs - fromMs) / span) * width;
    const y = scale > 0 ? height - (value / scale) * height : height;
    segments[segments.length - 1]?.push(`${x.toFixed(1)},${y.toFixed(1)}`);
  }
  return segments
    .filter((points) => points.length > 0)
    .map((points) =>
      points.length === 1 ? `${points[0]} ${points[0]}` : points.join(" "),
    );
}

/**
 * Labels for the value axis at `fractions` (ascending) of `top`. When two
 * ticks would print the same text (a tiny or empty scale, where "1 b" would
 * show three times at different heights), only the two ends are labelled,
 * so no printed tick misstates its height.
 */
export function axisLabels(top: number, fractions: readonly number[], format: (value: number) => string): string[] {
  const labels = fractions.map((fraction) => format(top * fraction));
  if (new Set(labels).size === labels.length) return labels;
  return labels.map((label, index) => (index === 0 || index === labels.length - 1 ? label : ""));
}
