import { useRef, useState } from "react";

import type { DashboardV2PipelineOutputOverviewActions } from "../dashboard-v2-loader.js";
import type { PipelineOutputOverviewModel } from "../../features/pipeline-operate-view-model.js";

import {
  StatusBadge,
  toneTextClasses,
  outputFilters,
  outputMatchesFilter,
  outputMatchesSearch,
  outputExpansionLabel,
  outputStatusDetail,
} from "./common.js";
import type { OutputFilter } from "./common.js";

export function DashboardV2PipelineOutputOverview({
  actions,
  model,
}: {
  actions: DashboardV2PipelineOutputOverviewActions;
  model: PipelineOutputOverviewModel;
}): React.JSX.Element {
  const [openActionsFor, setOpenActionsFor] = useState<string | null>(null);
  const [outputFilter, setOutputFilter] = useState<OutputFilter>("all");
  const [outputQuery, setOutputQuery] = useState("");
  const actionButtonRefs = useRef(new Map<string, HTMLButtonElement>());
  const normalizedOutputQuery = outputQuery.trim().toLowerCase();
  const filteredCards = model.cards.filter(
    (output) =>
      outputMatchesFilter(output, outputFilter) &&
      outputMatchesSearch(output, normalizedOutputQuery),
  );
  const filtersActive = outputFilter !== "all" || normalizedOutputQuery !== "";
  const showOutputTools = model.cards.length > 4 || filtersActive;
  const activeFilterLabel =
    outputFilters.find((filter) => filter.id === outputFilter)?.label ?? "All";
  const outputResultSummary = filtersActive
    ? `${filteredCards.length}/${model.cards.length} shown · ${activeFilterLabel}${
        normalizedOutputQuery ? ` · "${outputQuery.trim()}"` : ""
      }`
    : null;
  const outputEmptyDetail =
    outputFilter === "all"
      ? `No output destinations match "${outputQuery.trim()}".`
      : `No ${activeFilterLabel.toLowerCase()} output destinations match${
          normalizedOutputQuery ? ` "${outputQuery.trim()}"` : ""
        }.`;
  const clearOutputFilters = (): void => {
    setOutputFilter("all");
    setOutputQuery("");
  };
  const closeActionsMenu = (outputId: string, restoreFocus = false): void => {
    setOpenActionsFor(null);
    if (restoreFocus) {
      window.requestAnimationFrame(() =>
        actionButtonRefs.current.get(outputId)?.focus(),
      );
    }
  };

  return (
    <section
      aria-labelledby="dashboard-v2-output-overview-title"
      className="mb-3"
    >
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <h2
            className="text-base-content/70 text-xs font-semibold uppercase tracking-wide"
            id="dashboard-v2-output-overview-title"
          >
            Output overview
          </h2>
          <p className="text-base-content/55 mt-1 text-xs tabular-nums">
            {model.activeLabel} · {model.aggregateRate} aggregate
          </p>
        </div>
        <button
          aria-label={`Add output for ${model.pipelineName}`}
          className="btn btn-sm btn-accent btn-outline"
          onClick={() => actions.addOutput(model.pipelineId)}
          type="button"
        >
          Add Output
        </button>
      </div>
      {model.counts.length ? (
        <dl className="border-base-content/10 mt-3 flex flex-wrap gap-x-5 gap-y-2 border-y py-2.5">
          {model.counts.map((count) => (
            <div className="flex items-baseline gap-2" key={count.key}>
              <dt className="text-base-content/60 text-[0.65rem] font-semibold uppercase">
                {count.label}
              </dt>
              <dd
                className={`${toneTextClasses[count.tone]} text-lg font-semibold tabular-nums`}
              >
                {count.count}
              </dd>
            </div>
          ))}
        </dl>
      ) : (
        <p className="text-base-content/55 mt-3 text-sm">
          No outputs configured.
        </p>
      )}
      {model.attention.length ? (
        <div className="border-warning/30 mt-3 border-l-2 pl-3">
          <h3 className="text-warning text-xs font-semibold uppercase">
            Needs attention
          </h3>
          <div className="mt-2 space-y-2">
            {model.attention.map((output) => (
              <div
                className="border-base-content/10 border-t py-2 first:border-t-0"
                key={output.id}
              >
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <span className="min-w-0 truncate text-sm font-semibold">
                    {output.name}
                  </span>
                  <StatusBadge showDetail={false} status={output.status} />
                </div>
                <p className="text-base-content/55 mt-1 text-xs tabular-nums">
                  {output.encodingLabel} · {output.rateLabel}
                </p>
              </div>
            ))}
          </div>
        </div>
      ) : model.counts.length ? (
        <p className="text-success mt-3 text-xs font-medium">
          No outputs need attention.
        </p>
      ) : null}
      {model.cards.length ? (
        <div className="mt-3 space-y-2">
          <div className="space-y-2">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h3 className="text-base-content/70 text-xs font-semibold uppercase">
                Output destinations
              </h3>
              {filtersActive ? (
                <span
                  aria-live="polite"
                  className="text-base-content/55 text-xs tabular-nums"
                  role="status"
                >
                  {outputResultSummary}
                </span>
              ) : null}
            </div>
            {showOutputTools ? (
              <>
                <label className="input input-bordered input-sm flex min-h-10 items-center gap-2">
                  <span className="text-base-content/55 text-xs font-semibold uppercase">
                    Find
                  </span>
                  <input
                    aria-label="Search output destinations"
                    className="min-w-0 grow"
                    onChange={(event) =>
                      setOutputQuery(event.currentTarget.value)
                    }
                    placeholder="name, URL, state"
                    type="search"
                    value={outputQuery}
                  />
                </label>
                <div
                  className="flex flex-wrap items-center justify-between gap-2"
                >
                  <div
                    aria-label="Filter output destinations by state"
                    className="flex flex-wrap gap-1"
                    role="group"
                  >
                    {outputFilters.map((filter) => (
                      <button
                        aria-label={
                          filter.id === "all"
                            ? "Show all output destinations"
                            : `Show ${filter.label.toLowerCase()} output destinations`
                        }
                        aria-pressed={outputFilter === filter.id}
                        className={`btn btn-sm min-h-9 min-w-11 ${
                          outputFilter === filter.id
                            ? "btn-accent"
                            : "btn-outline btn-ghost"
                        }`}
                        key={filter.id}
                        onClick={() => setOutputFilter(filter.id)}
                        type="button"
                      >
                        {filter.label}
                      </button>
                    ))}
                  </div>
                  {filtersActive ? (
                    <button
                      aria-label="Clear output destination filters"
                      className="btn btn-sm btn-ghost min-h-9"
                      onClick={clearOutputFilters}
                      type="button"
                    >
                      Clear output filters
                    </button>
                  ) : null}
                </div>
              </>
            ) : null}
          </div>
          {filteredCards.length ? (
            filteredCards.map((output) => {
              const detail = outputStatusDetail(output.status);
              return (
                <article
                  className="border-base-content/10 border-t py-2.5"
                  key={output.id}
                >
              <div className="flex flex-wrap items-start justify-between gap-2">
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <h4 className="min-w-0 truncate text-sm font-semibold">
                      {output.name}
                    </h4>
                    <StatusBadge showDetail={false} status={output.status} />
                  </div>
                  <p className="text-base-content/60 mt-1 text-xs tabular-nums">
                    {output.encodingLabel} · {output.rateLabel}
                    {output.uptimeLabel
                      ? ` · ${output.uptimeLabel} uptime`
                      : ""}
                  </p>
                  <code className="text-base-content/50 mt-1 block truncate text-xs">
                    {output.urlLabel}
                  </code>
                  {detail ? (
                    <p className="text-base-content/55 mt-1 text-xs">
                      {detail}
                    </p>
                  ) : null}
                  {output.controlError ? (
                    <p
                      className="text-error mt-1 text-xs font-medium"
                      role="status"
                    >
                      Output request failed · {output.controlError}
                    </p>
                  ) : null}
                </div>
                <button
                  aria-label={`${output.controlLabel.replace("...", "")} ${output.name}`}
                  className="btn btn-sm btn-accent btn-outline"
                  disabled={output.controlDisabled}
                  onClick={() =>
                    void actions.toggleOutput(model.pipelineId, output.id)
                  }
                  type="button"
                >
                  {output.controlLabel}
                </button>
              </div>
              <div className="relative mt-2 flex justify-end">
                <button
                  aria-haspopup="menu"
                  aria-expanded={openActionsFor === output.id}
                  aria-label={`More output actions for ${output.name}`}
                  className="btn btn-sm btn-ghost min-h-9 min-w-11"
                  onClick={() =>
                    setOpenActionsFor((current) =>
                      current === output.id ? null : output.id,
                    )
                  }
                  ref={(element) => {
                    if (element) {
                      actionButtonRefs.current.set(output.id, element);
                    } else {
                      actionButtonRefs.current.delete(output.id);
                    }
                  }}
                  type="button"
                >
                  More
                </button>
                {openActionsFor === output.id ? (
                  <div
                    aria-label={`More output actions for ${output.name}`}
                    className="bg-base-100 border-base-content/10 absolute right-0 top-7 z-20 w-36 rounded-lg border p-1 shadow-xl"
                    onKeyDown={(event) => {
                      if (event.key === "Escape") {
                        event.stopPropagation();
                        closeActionsMenu(output.id, true);
                      }
                    }}
                    role="menu"
                  >
                    <button
                      aria-label={`History ${output.name}`}
                      className="btn btn-sm btn-ghost min-h-9 w-full justify-start"
                      onClick={() => {
                        closeActionsMenu(output.id);
                        actions.openOutputHistory(
                          model.pipelineId,
                          output.id,
                          output.name,
                        );
                      }}
                      role="menuitem"
                      type="button"
                    >
                      History
                    </button>
                    {output.monitorAvailable ? (
                      <button
                        aria-label={`Monitor ${output.name}`}
                        className="btn btn-sm btn-ghost min-h-9 w-full justify-start"
                        onClick={() => {
                          closeActionsMenu(output.id);
                          actions.monitorOutput(model.pipelineId, output.id);
                        }}
                        role="menuitem"
                        type="button"
                      >
                        Monitor
                      </button>
                    ) : null}
                    <button
                      aria-label={`Edit ${output.name}`}
                      className="btn btn-sm btn-ghost min-h-9 w-full justify-start"
                      onClick={() => {
                        closeActionsMenu(output.id);
                        actions.editOutput(model.pipelineId, output.id);
                      }}
                      role="menuitem"
                      type="button"
                    >
                      Edit
                    </button>
                    <button
                      aria-label={`Delete ${output.name}`}
                      className="btn btn-sm btn-ghost text-error min-h-9 w-full justify-start"
                      disabled={output.deleteDisabled}
                      onClick={() => {
                        closeActionsMenu(output.id);
                        void actions.deleteOutput(model.pipelineId, output.id);
                      }}
                      role="menuitem"
                      type="button"
                    >
                      Delete
                    </button>
                  </div>
                ) : null}
              </div>
            </article>
            );
          })
          ) : (
            <div className="border-base-content/10 rounded-lg border border-dashed px-3 py-4">
              <p className="text-sm font-semibold">No outputs match.</p>
              <p
                aria-live="polite"
                className="text-base-content/60 mt-1 text-xs"
                role="status"
              >
                {outputEmptyDetail} Clear filters to show all.
              </p>
              <button
                aria-label="Clear no-result output destination filters"
                className="btn btn-xs btn-ghost mt-3"
                onClick={clearOutputFilters}
                type="button"
              >
                Clear filters
              </button>
            </div>
          )}
          {model.canExpand ? (
            <div className="flex flex-wrap items-center justify-between gap-2 pt-1">
              <p className="text-base-content/55 text-xs">
                {model.listCaption}
              </p>
              <button
                aria-label={
                  model.expanded
                    ? `Show fewer output destinations for ${model.pipelineName}`
                    : `Show all output destinations for ${model.pipelineName}`
                }
                className="btn btn-xs btn-ghost"
                onClick={() => actions.toggleOutputList(model.pipelineId)}
                type="button"
              >
                {outputExpansionLabel(model)}
              </button>
            </div>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}
