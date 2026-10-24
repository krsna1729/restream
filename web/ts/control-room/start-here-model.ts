// The first-run path, read from the plant itself: make a pipeline, point
// an encoder at it, add a destination, go live. A step is done when the
// state shows it done, so the list never asks for what already happened,
// and it leaves Fleet once something is live.

import type { FleetPipelineRow } from "./fleet-model.js";

export type StartStepKey = "pipeline" | "encoder" | "destination" | "live";

export interface StartStep {
  readonly key: StartStepKey;
  readonly title: string;
  readonly detail: string;
  readonly done: boolean;
}

export interface StartPath {
  readonly steps: readonly StartStep[];
  /** The step to do now; null once every step is done. */
  readonly next: StartStepKey | null;
  /** The pipeline the later steps act on: the one furthest along. */
  readonly pipelineId: string | null;
}

function progress(row: FleetPipelineRow): number {
  const destinations = [...row.protocols.values()].reduce((sum, count) => sum + count, 0);
  return (row.source ? 2 : 0) + (destinations > 0 ? 1 : 0) + (row.delivering > 0 ? 4 : 0);
}

export function startPath(rows: readonly FleetPipelineRow[]): StartPath {
  let focus: FleetPipelineRow | null = null;
  for (const row of rows) {
    if (focus === null || progress(row) > progress(focus)) focus = row;
  }
  const destinations = focus ? [...focus.protocols.values()].reduce((sum, count) => sum + count, 0) : 0;
  const steps: StartStep[] = [
    {
      key: "pipeline",
      title: "Create a pipeline",
      detail: "One stream in, sent on to many places. Name it after the show or the camera.",
      done: focus !== null,
    },
    {
      key: "encoder",
      title: "Connect your encoder",
      detail: "Copy the server and stream key into OBS or your hardware encoder, then start streaming.",
      done: focus?.source != null,
    },
    {
      key: "destination",
      title: "Add a destination",
      detail: "Pick YouTube, Twitch or another platform and paste its stream key.",
      done: destinations > 0,
    },
    {
      key: "live",
      title: "Go live",
      detail: "Start the destination; its badge turns green when viewers can see you.",
      done: (focus?.delivering ?? 0) > 0,
    },
  ];
  return { steps, next: steps.find((step) => !step.done)?.key ?? null, pipelineId: focus?.id ?? null };
}
