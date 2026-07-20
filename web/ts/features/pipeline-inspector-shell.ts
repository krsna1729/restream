export function pipelineInspectorShellHtml(): string {
  return `
          <h1 class="dashboard-title">Pipeline inspect</h1>
          <p
            id="inspect-route-summary"
            class="text-base-content/60 text-sm"
            role="status"
            aria-live="polite"
          ></p>
          <div
            class="border-base-content/10 bg-base-200 flex flex-wrap items-center gap-2 rounded-lg border p-3"
          >
            <select
              id="inspect-pipeline-select"
              class="select select-sm min-w-0 flex-1"
              aria-label="Inspect pipeline"
            ></select>
            <button
              type="button"
              id="inspect-open-pipeline-btn"
              class="btn btn-sm btn-outline min-h-10"
              aria-label="Operate selected pipeline"
            >
              Operate
            </button>
          </div>
          <section
            class="border-base-content/10 bg-base-200 min-h-[28rem] min-w-0 rounded-lg border p-3"
          >
            <div class="mb-3 flex flex-wrap items-center justify-between gap-2">
              <h2 id="inspect-graph-heading" class="text-base font-semibold">
                Graph Explorer
              </h2>
              <div class="flex gap-2">
                <button
                  type="button"
                  id="inspect-refresh-graph-btn"
                  class="btn btn-xs btn-accent"
                >
                  Stop Refresh
                </button>
              </div>
            </div>
            <div
              id="inspect-graph-status"
              class="text-base-content/60 mb-2 text-sm"
            ></div>
            <div
              id="inspect-graph-container"
              class="bg-base-100 min-h-[24rem] min-w-0 overflow-auto rounded-lg"
            ></div>
          </section>
          <div class="grid min-w-0 gap-4 xl:grid-cols-2">
            <section
              class="border-base-content/10 bg-base-200 min-w-0 rounded-lg border p-3"
              aria-label="Pipeline overview"
            >
              <div class="mb-3 flex items-center justify-between gap-2">
                <h2 class="text-base font-semibold">Pipeline Overview</h2>
              </div>
              <div id="inspect-pipeline-summary" class="space-y-3"></div>
            </section>
            <section
              class="border-base-content/10 bg-base-200 min-w-0 rounded-lg border p-3"
            >
              <div class="mb-3 flex items-center justify-between gap-2">
                <h2 class="text-base font-semibold">Resource Details</h2>
              </div>
              <div id="inspect-resource-details"></div>
            </section>
            <section
              class="border-base-content/10 bg-base-200 min-w-0 rounded-lg border p-3 xl:col-span-2"
            >
              <div
                class="mb-3 flex flex-wrap items-center justify-between gap-2"
              >
                <h2 class="text-base font-semibold">Diagnostics Deep Dive</h2>
                <button
                  type="button"
                  id="inspect-open-diagnostics-btn"
                  class="btn btn-xs btn-outline"
                >
                  Run Diagnostics
                </button>
              </div>
              <p
                id="inspect-focus-summary"
                class="text-base-content/60 mb-3 text-sm"
                role="status"
                aria-live="polite"
              ></p>
              <div id="inspect-diagnostics-summary"></div>
            </section>
          </div>`;
}
