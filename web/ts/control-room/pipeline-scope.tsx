import { useMemo } from "react";
import type { ReactNode } from "react";

import { buildFleetModel } from "./fleet-model.js";
import { formatBps } from "./format.js";
import { go, PlaceLink } from "./place-link.js";
import { HealthBadge } from "./pipelines.js";
import type { Workspace } from "./route.js";
import type { LiveSnapshot } from "./store.js";

/**
 * A workspace about one pipeline: a picker until one is chosen (or after it
 * is deleted), then its header and `children`, keyed by the pipeline so
 * per-pipeline state and polls restart when it changes.
 */
export function PipelineScope(props: {
  live: LiveSnapshot;
  workspace: Workspace;
  pipelineId: string | null;
  verb: string;
  children: (pipelineId: string) => ReactNode;
}) {
  const { live, workspace, pipelineId } = props;
  const state = live.state;
  const row = useMemo(
    () =>
      state
        ? (buildFleetModel(state, live.rates).pipelines.find(
            (candidate) => candidate.id === pipelineId,
          ) ?? null)
        : null,
    [state, live.rates, pipelineId],
  );
  if (state === null) {
    return <p className="opacity-70">Waiting for runtime state…</p>;
  }
  if (pipelineId === null || row === null) {
    return (
      <section aria-labelledby="scope-pick" className="space-y-2">
        <h2 id="scope-pick" className="text-xl font-semibold">
          {pipelineId === null
            ? `Choose a pipeline to ${props.verb}`
            : "That pipeline no longer exists"}
        </h2>
        <ul className="menu rounded-box bg-base-200">
          {state.pipelines.map((pipeline) => (
            <li key={pipeline.id}>
              <PlaceLink place={{ workspace, pipelineId: pipeline.id }}>
                {pipeline.name}
              </PlaceLink>
            </li>
          ))}
        </ul>
      </section>
    );
  }
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3">
        <h2 className="text-xl font-semibold">{row.name}</h2>
        <HealthBadge row={row} />
        <span className="text-sm opacity-70">
          {row.source
            ? `${row.source.protocol.toUpperCase()} in · ${formatBps(row.ingestBps)}`
            : "no input connected"}{" "}
          · {formatBps(row.egressBps)} out
        </span>
        <select
          className="select select-sm select-bordered ml-auto"
          aria-label="Pipeline"
          value={pipelineId}
          onChange={(event) => go({ workspace, pipelineId: event.target.value })}
        >
          {state.pipelines.map((pipeline) => (
            <option key={pipeline.id} value={pipeline.id}>
              {pipeline.name}
            </option>
          ))}
        </select>
      </div>
      <div key={pipelineId} className="space-y-4">
        {props.children(pipelineId)}
      </div>
    </div>
  );
}
