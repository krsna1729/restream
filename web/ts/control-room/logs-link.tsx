// A quiet link from a problem to the engine's log for it, already filtered
// to the pipeline (and destination, when there is one).

import { PlaceLink } from "./router.js";

export function LogsLink(props: { pipelineId: string | null | undefined; outputId?: string | null; label?: string }) {
  if (!props.pipelineId) return null;
  return (
    <PlaceLink
      place={{ workspace: "logs", pipelineId: props.pipelineId, outputId: props.outputId ?? null }}
      className="link link-hover touch-36 inline-flex shrink-0 items-center text-xs text-base-content/70"
    >
      {props.label ?? "Logs"}
    </PlaceLink>
  );
}
