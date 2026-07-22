import { useState } from "react";

import type { DashboardV2OverviewActions } from "../dashboard-v2-loader.js";
import type {
  OverviewStatus,
  OverviewViewModel,
} from "../../features/overview-view-model.js";

import {
  Panel,
  StatusBadge,
  MetricCard,
  toneClasses,
  formatActivityTime,
} from "./common.js";

export function DashboardV2Overview({
  actions,
  model,
}: {
  actions: DashboardV2OverviewActions;
  model: OverviewViewModel;
}): React.JSX.Element {
  const hasAttention = model.attention.length > 0;
  const [pipelineTableQuery, setPipelineTableQuery] = useState("");
  const normalizedPipelineTableQuery = pipelineTableQuery.trim().toLowerCase();
  const clearPipelineTableSearch = (): void => setPipelineTableQuery("");
  const filteredPipelineRows = normalizedPipelineTableQuery
    ? model.pipelines.filter((pipeline) =>
        pipeline.name.toLowerCase().includes(normalizedPipelineTableQuery),
      )
    : model.pipelines;
  const showPipelineTableSearch =
    model.pipelines.length > 8 || normalizedPipelineTableQuery !== "";
  const [activityQuery, setActivityQuery] = useState("");
  const normalizedActivityQuery = activityQuery.trim().toLowerCase();
  const clearActivitySearch = (): void => setActivityQuery("");
  const filteredActivity = normalizedActivityQuery
    ? model.activity.filter((item) =>
        [
          item.headline,
          item.summary,
          item.details.join(" "),
          item.tone,
          `${item.eventCount} event${item.eventCount === 1 ? "" : "s"}`,
        ]
          .join(" ")
          .toLowerCase()
          .includes(normalizedActivityQuery),
      )
    : model.activity;
  const showActivitySearch =
    model.activity.length > 2 || normalizedActivityQuery !== "";
  return (
    <div className="space-y-4" id="dashboard-v2-overview">
      <header className="flex flex-wrap items-end justify-between gap-3 px-1">
        <div>
          <p className="text-accent text-xs font-semibold uppercase tracking-[0.18em]">
            Live operations
          </p>
          <h1 className="dashboard-title mt-1">Fleet overview</h1>
          <p className="dashboard-subtitle">
            See what needs action before scanning throughput and system load.
          </p>
        </div>
        <button
          aria-label="Add a new pipeline"
          className="btn btn-sm btn-primary dashboard-sturdy-control"
          onClick={actions.addPipeline}
          type="button"
        >
          Add Pipeline
        </button>
      </header>

      <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,1.2fr)_minmax(22rem,0.8fr)]">
        <Panel
          className={
            hasAttention
              ? "border-warning/35 bg-warning/5"
              : "border-success/25 bg-success/5"
          }
          labelledBy="dashboard-v2-attention-title"
        >
          <div className="dashboard-section-header items-start py-4">
            <div>
              <p
                className={`${hasAttention ? "text-warning" : "text-success"} text-xs font-semibold uppercase tracking-wider`}
              >
                Current priority
              </p>
              <h2
                className="mt-1 text-xl font-semibold"
                id="dashboard-v2-attention-title"
              >
                {hasAttention
                  ? `${model.attention.length} pipeline${model.attention.length === 1 ? "" : "s"} needs attention`
                  : model.counts.pipelines === 0
                    ? "Ready for the first pipeline"
                    : "Fleet is clear"}
              </h2>
              <p className="text-base-content/80 mt-1 text-sm">
                Issues are ordered by upstream cause and severity.
              </p>
            </div>
            <button
              aria-label="Open restream runtime detail"
              className="btn btn-sm btn-outline dashboard-sturdy-control"
              onClick={actions.openStatus}
              type="button"
            >
              Runtime detail
            </button>
          </div>
          <div
            className={`grid gap-3 p-4 ${model.attention.length > 1 ? "lg:grid-cols-2" : ""}`}
          >
            {hasAttention ? (
              model.attention.map((item) => (
                <article
                  className="dashboard-card p-3"
                  key={item.pipelineId}
                >
                  <div className="flex min-w-0 flex-col items-start gap-3 sm:flex-row sm:justify-between">
                    <div className="min-w-0">
                      <h3 className="truncate font-semibold">
                        {item.pipelineName}
                      </h3>
                      <p className="text-base-content/70 mt-1 text-xs">
                        {item.detail}
                      </p>
                    </div>
                    <StatusBadge status={item.status} />
                  </div>
                  <div className="mt-3 flex flex-wrap gap-2">
                    <button
                      aria-label={`Operate ${item.pipelineName}`}
                      className="btn btn-xs btn-outline dashboard-sturdy-control"
                      onClick={() => actions.openPipeline(item.pipelineId)}
                      type="button"
                    >
                      Operate
                    </button>
                    <button
                      aria-label={`Inspect ${item.pipelineName}`}
                      className="btn btn-xs btn-outline dashboard-sturdy-control"
                      onClick={() => actions.inspectPipeline(item.pipelineId)}
                      type="button"
                    >
                      Inspect
                    </button>
                  </div>
                </article>
              ))
            ) : (
              <div className="dashboard-empty">
                {model.counts.pipelines === 0
                  ? "Add a pipeline to begin monitoring inputs and destinations."
                  : "No active incident-level issues. Runtime detail stays available under Status and Pipeline Inspect."}
              </div>
            )}
          </div>
        </Panel>

        <Panel className="p-3" labelledBy="dashboard-v2-signals-title">
          <div className="mb-3 flex flex-wrap items-start justify-between gap-3 px-1">
            <div>
              <h2
                className="dashboard-section-title"
                id="dashboard-v2-signals-title"
              >
                Fleet signals
              </h2>
              <p className="dashboard-subtitle mt-0.5 text-xs">
                Current snapshot and recent trend
              </p>
            </div>
            <span className="badge badge-outline">
              {model.counts.pipelines} pipeline
              {model.counts.pipelines === 1 ? "" : "s"}
            </span>
          </div>
          <div className="grid gap-2 sm:grid-cols-2">
            {model.metrics.map((metric) => (
              <MetricCard key={metric.key} metric={metric} />
            ))}
          </div>
        </Panel>
      </div>

      <Panel labelledBy="dashboard-v2-pipelines-title">
        <div className="dashboard-section-header items-start">
          <div>
            <h2
              className="dashboard-section-title"
              id="dashboard-v2-pipelines-title"
            >
              All pipelines
            </h2>
            <p className="dashboard-subtitle">
              Compare intent, runtime state, and data flow.
            </p>
          </div>
          {showPipelineTableSearch ? (
            <div className="w-full max-w-sm space-y-2 sm:w-80">
              <div className="flex flex-wrap items-center gap-2">
                <label className="input input-bordered input-sm flex min-h-10 min-w-0 flex-1 items-center gap-2">
                  <span className="text-base-content/55 text-xs font-semibold uppercase">
                    Find
                  </span>
                  <input
                    aria-label="Search overview pipelines"
                    className="min-w-0 grow"
                    onChange={(event) =>
                      setPipelineTableQuery(event.currentTarget.value)
                    }
                    placeholder="pipeline name"
                    type="search"
                    value={pipelineTableQuery}
                  />
                </label>
                {normalizedPipelineTableQuery ? (
                  <button
                    aria-label="Clear overview pipeline search"
                    className="btn btn-xs btn-ghost"
                    onClick={clearPipelineTableSearch}
                    type="button"
                  >
                    Clear search
                  </button>
                ) : null}
              </div>
              {normalizedPipelineTableQuery ? (
                <p
                  aria-live="polite"
                  className="text-base-content/55 px-1 text-xs tabular-nums"
                  role="status"
                >
                  {filteredPipelineRows.length}/{model.pipelines.length}{" "}
                  pipelines shown · "{pipelineTableQuery.trim()}"
                </p>
              ) : null}
            </div>
          ) : null}
        </div>
        <div className="space-y-2 p-4 md:hidden">
          {model.pipelines.length ? (
            filteredPipelineRows.length ? (
              filteredPipelineRows.map((pipeline) => (
                <article className="dashboard-card p-3" key={pipeline.id}>
                  <div className="flex min-w-0 flex-wrap items-start justify-between gap-2">
                    <button
                      aria-label={`Open pipeline ${pipeline.name}`}
                      className="group min-w-0 text-left"
                      onClick={() => actions.openPipeline(pipeline.id)}
                      type="button"
                    >
                      <span className="group-hover:text-accent block truncate font-semibold">
                        {pipeline.name}
                      </span>
                    </button>
                    <StatusBadge status={pipeline.health} />
                  </div>
                  <dl className="mt-3 grid gap-2">
                    {([
                      ["Input", pipeline.input],
                      ["Outputs", pipeline.outputs],
                      ["Input rate", pipeline.inputRate],
                      ["Output rate", pipeline.outputRate],
                      ["Recording", pipeline.recording],
                    ] satisfies ReadonlyArray<
                      readonly [string, OverviewStatus]
                    >).map(([label, status]) => (
                      <div
                        className="flex min-w-0 items-center justify-between gap-2"
                        key={label}
                      >
                        <dt className="text-base-content/60 text-xs font-semibold uppercase">
                          {label}
                        </dt>
                        <dd className="min-w-0">
                          <StatusBadge status={status} />
                        </dd>
                      </div>
                    ))}
                  </dl>
                </article>
              ))
            ) : (
              <div className="dashboard-empty">
                <p className="font-semibold">No pipelines match.</p>
                <p
                  aria-live="polite"
                  className="text-base-content/60 mt-1 text-sm"
                  role="status"
                >
                  No overview pipelines match "{pipelineTableQuery.trim()}".
                  Clear search to show all.
                </p>
              </div>
            )
          ) : (
            <p className="text-base-content/70 text-sm">
              No pipelines configured.
            </p>
          )}
        </div>
        <div className="hidden overflow-x-auto md:block">
          <table className="table table-sm">
            <thead className="text-base-content/70 bg-base-100/50 text-xs uppercase">
              <tr>
                <th>Pipeline</th>
                <th>State</th>
                <th>Input</th>
                <th>Outputs</th>
                <th>Input Rate</th>
                <th>Output Rate</th>
                <th>Recording</th>
              </tr>
            </thead>
            <tbody>
              {model.pipelines.length ? (
                filteredPipelineRows.length ? (
                  filteredPipelineRows.map((pipeline) => (
                  <tr
                    className="border-base-content/5 hover:bg-base-100/60 border-t"
                    key={pipeline.id}
                  >
                    <td className="min-w-56 py-3">
                      <button
                        aria-label={`Open pipeline ${pipeline.name}`}
                        className="group flex max-w-xs text-left"
                        onClick={() => actions.openPipeline(pipeline.id)}
                        type="button"
                      >
                        <span className="group-hover:text-accent truncate font-semibold">
                          {pipeline.name}
                        </span>
                      </button>
                    </td>
                    <td>
                      <StatusBadge status={pipeline.health} />
                    </td>
                    <td>
                      <StatusBadge status={pipeline.input} />
                    </td>
                    <td>
                      <StatusBadge status={pipeline.outputs} />
                    </td>
                    <td>
                      <StatusBadge status={pipeline.inputRate} />
                    </td>
                    <td>
                      <StatusBadge status={pipeline.outputRate} />
                    </td>
                    <td>
                      <StatusBadge status={pipeline.recording} />
                    </td>
                  </tr>
                  ))
                ) : (
                  <tr>
                    <td className="px-4 py-6" colSpan={7}>
                      <div className="max-w-xl">
                        <p className="font-semibold">No pipelines match.</p>
                        <p
                          aria-live="polite"
                          className="text-base-content/60 mt-1 text-sm"
                          role="status"
                        >
                          No overview pipelines match "
                          {pipelineTableQuery.trim()}". Clear search to show
                          all.
                        </p>
                      </div>
                    </td>
                  </tr>
                )
              ) : (
                <tr>
                  <td className="text-base-content/70 px-4 py-6" colSpan={7}>
                    No pipelines configured.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </Panel>

      <Panel labelledBy="dashboard-v2-activity-title">
        <div className="dashboard-section-header items-start">
          <div>
            <h2
              className="dashboard-section-title"
              id="dashboard-v2-activity-title"
            >
              Restream Activity
            </h2>
            <p className="dashboard-subtitle">
              Recent restream-wide event bursts, grouped for operator-friendly
              review.
            </p>
          </div>
          <div className="flex w-full flex-col items-stretch gap-2 sm:w-auto sm:items-end">
            <button
              aria-label="Open restream status"
              className="btn btn-sm btn-outline dashboard-sturdy-control"
              onClick={actions.openStatus}
              type="button"
            >
              Open Status
            </button>
            {showActivitySearch ? (
              <div className="w-full max-w-sm min-w-0 space-y-2 sm:w-80">
                <div className="flex flex-wrap items-center gap-2">
                  <label className="input input-bordered input-sm flex min-h-10 w-full min-w-0 items-center gap-2 sm:flex-1">
                    <span className="text-base-content/55 text-xs font-semibold uppercase">
                      Find
                    </span>
                    <input
                      aria-label="Search restream activity"
                      className="min-w-0 grow"
                      onChange={(event) =>
                        setActivityQuery(event.currentTarget.value)
                      }
                      placeholder="event, status, detail"
                      type="search"
                      value={activityQuery}
                    />
                  </label>
                  {normalizedActivityQuery ? (
                    <button
                      aria-label="Clear restream activity search"
                      className="btn btn-xs btn-ghost w-full justify-start sm:w-auto"
                      onClick={clearActivitySearch}
                      type="button"
                    >
                      Clear activity search
                    </button>
                  ) : null}
                </div>
                {normalizedActivityQuery ? (
                  <p
                    aria-live="polite"
                    className="text-base-content/55 px-1 text-xs tabular-nums"
                    id="dashboard-v2-activity-search-summary"
                    role="status"
                  >
                    {filteredActivity.length}/{model.activity.length} bursts
                    shown · "{activityQuery.trim()}"
                  </p>
                ) : null}
              </div>
            ) : null}
          </div>
        </div>
        <div className="space-y-2 p-4">
          {model.activityLoading ? (
            <p className="text-base-content/70 text-sm">
              Loading recent restream activity...
            </p>
          ) : model.activity.length ? (
            filteredActivity.length ? (
              filteredActivity.map((item, index) => (
              <article
                className="dashboard-card p-3"
                key={`${item.startedAt || "activity"}-${index}`}
              >
                <div className="flex flex-wrap items-start justify-between gap-2">
                  <div>
                    <h3 className="font-semibold">{item.headline}</h3>
                    <p className="text-base-content/70 mt-1 text-sm">
                      {item.summary}
                    </p>
                    <p className="text-base-content/60 mt-1 text-xs tabular-nums">
                      {item.evidence}
                    </p>
                  </div>
                  <span className={`${toneClasses[item.tone]} badge border`}>
                    {formatActivityTime(item.endedAt)}
                  </span>
                </div>
                <div className="mt-2 flex flex-wrap gap-1.5">
                  {item.details.map((detail) => (
                    <span className="badge badge-outline badge-sm" key={detail}>
                      {detail}
                    </span>
                  ))}
                  <span className="badge badge-ghost badge-sm">
                    {item.eventCount} event{item.eventCount === 1 ? "" : "s"}
                  </span>
                </div>
              </article>
              ))
            ) : (
              <div className="dashboard-empty">
                <p className="font-semibold">No activity matches.</p>
                <p
                  aria-live="polite"
                  className="text-base-content/60 mt-1 text-sm"
                  role="status"
                >
                  No restream activity matches "{activityQuery.trim()}". Clear
                  activity search to show all.
                </p>
              </div>
            )
          ) : (
            <p className="text-base-content/70 text-sm">
              No recent restream-wide activity yet.
            </p>
          )}
        </div>
      </Panel>
    </div>
  );
}
