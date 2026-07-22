import { useState } from "react";

import type { DashboardV2PipelineSelectorActions } from "../dashboard-v2-loader.js";
import type { PipelineOperateSelectorModel } from "../../features/pipeline-operate-view-model.js";

import { toneClasses } from "./common.js";

export function DashboardV2PipelineSelector({
  actions,
  model,
}: {
  actions: DashboardV2PipelineSelectorActions;
  model: PipelineOperateSelectorModel;
}): React.JSX.Element {
  const [pipelineQuery, setPipelineQuery] = useState("");
  const normalizedPipelineQuery = pipelineQuery.trim().toLowerCase();
  const clearPipelineSearch = (): void => setPipelineQuery("");
  const filteredPipelines = normalizedPipelineQuery
    ? model.pipelines.filter((pipeline) =>
        [
          pipeline.name,
          pipeline.statusLabel,
          pipeline.inputRate,
          pipeline.outputRate,
          `${pipeline.runningOutputs}/${pipeline.totalOutputs}`,
        ]
          .join(" ")
          .toLowerCase()
          .includes(normalizedPipelineQuery),
      )
    : model.pipelines;
  const showPipelineSearch =
    model.pipelines.length > 6 || normalizedPipelineQuery !== "";
  const selectedPipeline =
    model.pipelines.find((pipeline) => pipeline.selected) ?? model.pipelines[0];
  const compactPipelineSelector =
    model.pipelines.length > 0 &&
    model.pipelines.length <= 3 &&
    !normalizedPipelineQuery;

  return (
    <section aria-labelledby="dashboard-v2-pipelines-selector-title">
      <div className="border-base-content/10 flex items-center justify-between gap-2 border-b px-4 py-3">
        <div>
          <div
            className="text-base-content/70 text-sm font-semibold uppercase"
            id="dashboard-v2-pipelines-selector-title"
          >
            Pipelines
          </div>
          <p className="text-base-content/50 mt-0.5 text-xs tabular-nums">
            {model.pipelines.length} configured
          </p>
        </div>
        <button
          aria-label="Add a new pipeline from the pipeline selector"
          className="btn btn-xs btn-accent btn-outline dashboard-sturdy-control"
          onClick={actions.addPipeline}
          type="button"
        >
          Add
        </button>
      </div>
      {model.pipelines.length ? (
        <div>
          {showPipelineSearch ? (
            <div className="border-base-content/10 space-y-2 border-b p-2">
              <div className="flex flex-wrap items-center gap-2">
                <label className="input input-bordered input-sm flex min-h-10 min-w-0 flex-1 items-center gap-2">
                  <span className="text-base-content/55 text-xs font-semibold uppercase">
                    Find
                  </span>
                  <input
                    aria-label="Search pipelines"
                    className="min-w-0 grow"
                    onChange={(event) =>
                      setPipelineQuery(event.currentTarget.value)
                    }
                    placeholder="name, state, rate"
                    type="search"
                    value={pipelineQuery}
                  />
                </label>
                {normalizedPipelineQuery ? (
                  <button
                    aria-label="Clear pipeline selector search"
                    className="btn btn-xs btn-ghost"
                    onClick={clearPipelineSearch}
                    type="button"
                  >
                    Clear search
                  </button>
                ) : null}
              </div>
              {normalizedPipelineQuery ? (
                <p
                  aria-live="polite"
                  className="text-base-content/55 px-1 text-xs tabular-nums"
                  role="status"
                >
                  {filteredPipelines.length}/{model.pipelines.length} pipelines
                  shown · "{pipelineQuery.trim()}"
                </p>
              ) : null}
            </div>
          ) : null}
          {compactPipelineSelector ? (
            <div className="space-y-2 p-2">
              <label className="flex flex-col gap-1 text-sm">
                <span className="text-base-content/55 text-xs font-semibold uppercase tracking-wide">
                  Active pipeline
                </span>
                <select
                  aria-label="Select pipeline"
                  className="select select-sm w-full"
                  onChange={(event) =>
                    actions.selectPipeline(event.currentTarget.value)
                  }
                  value={selectedPipeline?.id ?? ""}
                >
                  {model.pipelines.map((pipeline) => (
                    <option key={pipeline.id} value={pipeline.id}>
                      {pipeline.name}
                    </option>
                  ))}
                </select>
              </label>
            </div>
          ) : filteredPipelines.length ? (
            <ul className="max-h-52 w-full space-y-1 overflow-x-hidden overflow-y-auto p-2 md:max-h-none">
              {filteredPipelines.map((pipeline) => {
                const safePipelineId = pipeline.id.replace(
                  /[^a-zA-Z0-9_-]/g,
                  "-",
                );
                const detailId = `dashboard-v2-pipeline-selector-detail-${safePipelineId}`;
                const rateId = `dashboard-v2-pipeline-selector-rate-${safePipelineId}`;
                return (
                  <li className="min-w-0" key={pipeline.id}>
                    <button
                      aria-describedby={`${detailId} ${rateId}`}
                      aria-label={`Select pipeline ${pipeline.name}`}
                      aria-current={pipeline.selected ? "page" : undefined}
                      className={`${pipeline.selected ? "bg-base-100 border-base-content/10" : "border-transparent"} hover:bg-base-100 flex w-full min-w-0 items-start gap-3 rounded-lg border px-3 py-2 text-left`}
                      onClick={() => actions.selectPipeline(pipeline.id)}
                      type="button"
                    >
                      <span
                        aria-hidden="true"
                        className={`${toneClasses[pipeline.statusTone]} mt-1 h-3 w-3 shrink-0 rounded-full border`}
                      />
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-sm font-semibold">
                          {pipeline.name}
                        </span>
                        <span
                          className="text-base-content/60 mt-1 block truncate text-xs"
                          id={detailId}
                        >
                          {pipeline.statusLabel} · {pipeline.runningOutputs}/
                          {pipeline.totalOutputs} outputs
                        </span>
                        <span
                          className="text-base-content/50 mt-1 flex flex-wrap gap-x-2 text-[0.6875rem] tabular-nums"
                          id={rateId}
                        >
                          <span>{pipeline.inputRate} in</span>
                          <span>{pipeline.outputRate} out</span>
                        </span>
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          ) : (
            <div className="border-base-content/10 m-2 rounded-lg border border-dashed px-3 py-4">
              <p className="text-sm font-semibold">No pipelines match.</p>
              <p
                aria-live="polite"
                className="text-base-content/60 mt-1 text-xs"
                role="status"
              >
                No pipelines match "{pipelineQuery.trim()}". Clear search to
                return to all pipelines.
              </p>
            </div>
          )}
        </div>
      ) : (
        <div className="text-base-content/60 px-4 py-5 text-sm">
          No pipelines configured.
        </div>
      )}
    </section>
  );
}
