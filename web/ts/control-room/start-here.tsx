// Fleet's "Start here": the four steps from nothing to live, each ticked
// off by the plant itself, with the one to do now spelled out and a button
// for it. It leaves once something is live, or when hidden.

import { useEffect, useState } from "react";

import { listMediaFiles } from "../core/api.js";
import type { MediaFile } from "../core/api-types.js";
import { Panel } from "../ui/panel.js";
import { useSectionDetail } from "../ui/prefs.js";
import type { FleetPipelineRow } from "./fleet-model.js";
import { PipelineForm } from "./pipeline-form.js";
import { PlaceLink } from "./router.js";
import { useSession } from "./session.js";
import { startPath } from "./start-here-model.js";

export function StartHere(props: { rows: readonly FleetPipelineRow[] }) {
  const path = startPath(props.rows);
  const [hidden, setHidden] = useSectionDetail("start-here-hidden");
  const [creating, setCreating] = useState(false);
  const [files, setFiles] = useState<readonly MediaFile[]>([]);
  const canOperate = useSession().can("operator");
  useEffect(() => {
    if (creating) void listMediaFiles().then((media) => setFiles(media?.files ?? []));
  }, [creating]);
  if (path.next === null || hidden || !canOperate) return null;
  const step = path.steps.find((candidate) => candidate.key === path.next);
  const operate = path.pipelineId === null ? null : { workspace: "operate" as const, pipelineId: path.pipelineId };
  return (
    <Panel
      id="start-here"
      title="Start here"
      subtitle={`${path.steps.filter((candidate) => candidate.done).length} of ${path.steps.length} done · from nothing to live`}
      stripe="info"
      actions={
        <button type="button" className="btn btn-ghost btn-sm" onClick={() => setHidden(true)}>
          Hide
        </button>
      }
    >
      <div className="flex flex-col gap-4">
        <ul className="steps steps-vertical lg:steps-horizontal" aria-label="Steps to go live">
          {path.steps.map((candidate) => (
            <li
              key={candidate.key}
              data-content={candidate.done ? "✓" : undefined}
              aria-current={candidate.key === path.next ? "step" : undefined}
              className={`step text-sm ${candidate.done ? "step-success" : candidate.key === path.next ? "step-primary font-semibold" : ""}`}
            >
              {candidate.title}
              <span className="sr-only">{candidate.done ? " (done)" : candidate.key === path.next ? " (next)" : ""}</span>
            </li>
          ))}
        </ul>
        {step && (
          <div className="flex flex-col gap-3 rounded-box bg-base-100 p-3 sm:flex-row sm:items-center">
            <p className="min-w-0 flex-1 text-sm">
              <span className="font-semibold">Next: {step.title.toLowerCase()}.</span> {step.detail}
            </p>
            {step.key === "pipeline" && (
              <button type="button" className="btn btn-primary btn-sm" aria-expanded={creating} onClick={() => setCreating(!creating)}>
                {creating ? "Close" : "Create a pipeline"}
              </button>
            )}
            {step.key === "encoder" && operate && (
              <PlaceLink place={operate} className="btn btn-primary btn-sm">
                Show the server and key
              </PlaceLink>
            )}
            {step.key === "destination" && operate && (
              <PlaceLink place={operate} className="btn btn-primary btn-sm">
                Add a destination
              </PlaceLink>
            )}
            {step.key === "live" && (
              <PlaceLink place={{ workspace: "channels", pipelineId: path.pipelineId }} className="btn btn-primary btn-sm">
                Go live in Channels
              </PlaceLink>
            )}
          </div>
        )}
        {creating && step?.key === "pipeline" && <PipelineForm stored={null} files={files} keyLocked={false} onSaved={() => setCreating(false)} />}
      </div>
    </Panel>
  );
}
