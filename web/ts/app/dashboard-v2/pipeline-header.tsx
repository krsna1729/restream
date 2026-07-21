import type {
  DashboardV2PipelineDetailsPlaceholder,
  DashboardV2PipelineHeaderActions,
} from "../dashboard-v2-loader.js";
import type { PipelineOperateHeaderModel } from "../../features/pipeline-operate-view-model.js";

import {
  StatusBadge,
  toneClasses,
  formatPipelineHeaderFileIngestActionLabel,
  formatPipelineHeaderRecordingActionLabel,
} from "./common.js";

export function DashboardV2PipelineHeader({
  actions,
  model,
}: {
  actions: DashboardV2PipelineHeaderActions;
  model: PipelineOperateHeaderModel;
}): React.JSX.Element {
  const fileIngestActionLabel = model.fileIngestControl
    ? formatPipelineHeaderFileIngestActionLabel(model.fileIngestControl.label)
    : "";
  const recordingActionLabel = formatPipelineHeaderRecordingActionLabel(
    model.recordingControl.label,
  );

  return (
    <section
      aria-labelledby="dashboard-v2-pipeline-title"
      className="space-y-2"
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h1
              className="whitespace-normal text-lg font-semibold leading-tight"
              id="dashboard-v2-pipeline-title"
            >
              {model.name}
            </h1>
            <StatusBadge status={model.health} />
          </div>
          <div className="text-base-content/60 mt-2 flex flex-wrap gap-x-3 gap-y-1 text-xs tabular-nums">
            <span>{model.sourceLabel}</span>
            <span>{model.inputRate} in</span>
            <span>{model.outputRate} out</span>
            <span>{model.outputsLabel}</span>
            <span>{model.recordingLabel}</span>
          </div>
        </div>
        <div className="flex shrink-0 flex-wrap gap-2">
          {model.fileIngestControl ? (
            <button
              aria-label={`${fileIngestActionLabel} for ${model.name}`}
              className={`btn btn-xs ${
                model.fileIngestControl.danger ? "btn-error" : "btn-accent"
              } ${model.fileIngestControl.outlined ? "btn-outline" : ""}`}
              disabled={model.fileIngestControl.disabled}
              onClick={() => void actions.toggleFileIngest(model.id)}
              title={model.fileIngestControl.title}
              type="button"
            >
              {model.fileIngestControl.label}
            </button>
          ) : null}
          <button
            aria-label={`${recordingActionLabel} for ${model.name}`}
            className={`btn btn-xs ${
              model.recordingControl.danger ? "btn-error" : "btn-accent"
            } ${model.recordingControl.outlined ? "btn-outline" : ""}`}
            disabled={model.recordingControl.disabled}
            onClick={() => void actions.toggleRecording(model.id)}
            title={model.recordingControl.title}
            type="button"
          >
            {model.recordingControl.label}
          </button>
          <button
            aria-label={`Inspect graph for ${model.name}`}
            className="btn btn-xs btn-accent btn-outline dashboard-sturdy-control"
            onClick={() => actions.inspectPipeline(model.id)}
            type="button"
          >
            Graph
          </button>
          <button
            aria-label={`Diagnose ${model.name}`}
            className="btn btn-xs btn-accent btn-outline dashboard-sturdy-control"
            disabled={!model.canDiagnose}
            onClick={() => actions.diagnosePipeline(model.id)}
            title={model.diagnoseDisabledReason || ""}
            type="button"
          >
            Diagnose
          </button>
          <button
            aria-label={`Edit pipeline ${model.name}`}
            className="btn btn-xs btn-outline"
            disabled={!model.canEdit}
            onClick={() => actions.editPipeline(model.id)}
            title={model.editDisabledReason || ""}
            type="button"
          >
            Edit
          </button>
        </div>
      </div>
      {model.lifecycleMessages.length ? (
        <div aria-live="polite" className="space-y-2">
          {model.lifecycleMessages.map((message) => (
            <div
              className={`${toneClasses[message.tone]} rounded-lg border px-3 py-2 text-sm`}
              key={message.id}
              role="status"
            >
              <div className="font-semibold">{message.label}</div>
              <div className="mt-0.5 text-xs font-normal text-base-content/75">
                {message.detail}
              </div>
            </div>
          ))}
        </div>
      ) : null}
    </section>
  );
}

export function DashboardV2PipelineDetailsPlaceholderCard({
  model,
}: {
  model: DashboardV2PipelineDetailsPlaceholder;
}): React.JSX.Element {
  return (
    <section
      aria-labelledby="dashboard-v2-pipeline-details-placeholder-title"
      className="dashboard-section border-info/25 bg-info/5"
    >
      <div className="flex items-start gap-3 py-2">
        <span
          aria-hidden="true"
          className="border-info/30 bg-info/10 mt-1 inline-flex h-3 w-3 shrink-0 rounded-full border"
        />
        <div>
          <h1
            className="text-base-content text-lg font-semibold leading-tight"
            id="dashboard-v2-pipeline-details-placeholder-title"
          >
            {model.title}
          </h1>
          <p className="text-base-content/65 mt-1 max-w-2xl text-sm">
            {model.message}
          </p>
        </div>
      </div>
    </section>
  );
}
