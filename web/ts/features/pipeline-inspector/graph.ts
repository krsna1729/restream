import { outputViewEncodingLabel } from "../../core/output-config.js";
import { escapeHtml } from "../../core/utils.js";
import { RenderScope } from "../../core/render-scope.js";
import type { RenderScopeToken } from "../../core/render-scope.js";
import type { PipelineView } from "../../types.js";
import { getResourceMap } from "../../core/api.js";
import type { ResourceMapSnapshot } from "../../core/api.js";
import { fetchProcessingGraph, renderGraphInto } from "../graph.js";
import { renderResourceMapInto } from "./resource-view.js";

// ---------------------------------------------------------------------------
// State — graph fetching / rendering lifecycle
// ---------------------------------------------------------------------------

let graphPipelineId: string | null = null;
let graphInFlight: Promise<void> | null = null;
let graphRequestSeq = 0;
let graphRenderedStateKey: string | null = null;
let graphAutoRefresh = true;

/** Cache populated by the graph refresh flow; read by pipeline-inspector/index. */
const pipelineResourceMapCache = new Map<string, ResourceMapSnapshot>();

// ---------------------------------------------------------------------------
// Accessors for consumers that stay in pipeline-inspector/index.ts
// ---------------------------------------------------------------------------

export function getGraphInFlight(): Promise<void> | null {
  return graphInFlight;
}

export function getGraphAutoRefresh(): boolean {
  return graphAutoRefresh;
}

export function setGraphAutoRefresh(v: boolean): void {
  graphAutoRefresh = v;
}

export function getGraphPipelineId(): string | null {
  return graphPipelineId;
}

export function getGraphRenderedStateKey(): string | null {
  return graphRenderedStateKey;
}

export function resetGraphState(pipelineId: string | null): void {
  graphRequestSeq++;
  graphPipelineId = pipelineId;
  graphRenderedStateKey = null;
}

export function getCachedResourceMap(
  pipelineId: string,
): ResourceMapSnapshot | undefined {
  return pipelineResourceMapCache.get(pipelineId);
}

// ---------------------------------------------------------------------------
// Dependency injection — the graph functions call several functions defined
// in index.ts.  To avoid circular imports we register them here once at
// module init time.
// ---------------------------------------------------------------------------

interface GraphDeps {
  selectedPipeline: () => PipelineView | null;
  hasInvalidPipelineSelection: () => boolean;
  withPreservedScroll: (container: HTMLElement, fn: () => void) => void;
  renderSummary: (
    pipe: PipelineView | null,
    invalidPipelineSelection?: boolean,
  ) => void;
  renderInspectorResourceDetails: (
    pipe: PipelineView | null,
    resourceMap: ResourceMapSnapshot | null,
  ) => void;
  renderInspectCheckpointPresentation: (
    pipe: PipelineView | null,
    invalidPipelineSelection?: boolean,
  ) => void;
  clearPipelineUrlSelection: () => void;
  pipelineInspectorScope: RenderScope;
}

const deps: GraphDeps = {
  selectedPipeline: () => null,
  hasInvalidPipelineSelection: () => false,
  withPreservedScroll: (_container: HTMLElement, fn: () => void) => fn(),
  renderSummary: () => {},
  renderInspectorResourceDetails: () => {},
  renderInspectCheckpointPresentation: () => {},
  clearPipelineUrlSelection: () => {},
  pipelineInspectorScope: new RenderScope(""),
};

export function setGraphDeps(next: Partial<GraphDeps>): void {
  Object.assign(deps, next || {});
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

export function graphStateKey(pipe: PipelineView | null): string | null {
  if (!pipe) return null;
  const outputs = pipe.outs
    .map((out) =>
      [
        out.id,
        out.status,
        out.desiredState,
        outputViewEncodingLabel(out),
        out.phase || "",
        out.retrying ? "1" : "0",
        out.flapping ? "1" : "0",
        out.lastError || "",
      ].join(":"),
    )
    .join("|");
  return [
    pipe.id,
    pipe.name,
    pipe.input.status,
    pipe.input.probeStatus,
    pipe.input.readers,
    pipe.input.audioTracks.length,
    pipe.input.video?.codec || "",
    pipe.hlsPreview?.active ? "1" : "0",
    outputs,
  ].join("::");
}

export function shouldAutoRefreshGraph(): boolean {
  return graphAutoRefresh && !document.hidden && !graphInFlight;
}

export function syncPipelineInspectorVisibility(): void {
  if (graphAutoRefresh && !document.hidden && !graphInFlight) {
    void refreshPipelineInspectorGraph();
  }
}

function graphExplorerShellHtml({
  graphSlotId,
  title,
  scopeLabel,
  footerHtml = "",
}: {
  graphSlotId: string;
  title: string;
  scopeLabel: string;
  footerHtml?: string;
}): string {
  return `<div class="space-y-3 p-3">
    <section class="dashboard-section p-3">
      <div class="mb-2 flex items-center justify-between gap-2">
        <h3 class="dashboard-section-title text-sm">${escapeHtml(title)}</h3>
        <span class="text-base-content/50 text-xs">${escapeHtml(scopeLabel)}</span>
      </div>
      <div id="${escapeHtml(graphSlotId)}" class="bg-base-100 h-[460px] overflow-auto rounded-lg"></div>
    </section>
    ${footerHtml}
  </div>`;
}

export function renderGraphIntoShellSlot(
  container: HTMLElement,
  slot: HTMLElement | null,
  slotId: string,
  graph: Parameters<typeof renderGraphInto>[1],
): void {
  if (slot) {
    renderGraphInto(slot, graph);
    return;
  }
  const fallback = document.createElement("div");
  renderGraphInto(fallback, graph);
  const slotPattern = new RegExp(
    `(<div id="${slotId}"[^>]*>)(</div>)`,
  );
  container.innerHTML = container.innerHTML.replace(
    slotPattern,
    `$1${fallback.innerHTML}$2`,
  );
}

function renderProcessingGraphExplorer(
  container: HTMLElement,
  graph: Parameters<typeof renderGraphInto>[1],
): void {
  container.innerHTML = "";
  renderGraphInto(container, graph);
}

export async function refreshPipelineInspectorGraph(): Promise<void> {
  const pipe = deps.selectedPipeline();
  const requestStateKey = graphStateKey(pipe);
  const status = document.getElementById("inspect-graph-status");
  const container = document.getElementById("inspect-graph-container");
  if (!container) return;
  const requestSeq = ++graphRequestSeq;
  const scopeToken = deps.pipelineInspectorScope.token();
  if (!pipe && deps.hasInvalidPipelineSelection()) {
    graphPipelineId = null;
    if (status) status.textContent = "Select a pipeline.";
    deps.withPreservedScroll(container, () => {
      container.innerHTML =
        '<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">Select a pipeline to inspect its graph.</div>';
    });
    return;
  }
  if (!pipe) {
    const canRefreshInPlace =
      graphPipelineId === null && graphRenderedStateKey === "runtime";
    graphPipelineId = null;
    if (status && !canRefreshInPlace)
      status.textContent = "Loading runtime resources...";
    if (!canRefreshInPlace) {
      deps.withPreservedScroll(container, () => {
        container.innerHTML = `<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">
        Loading runtime resources...
    </div>`;
      });
    }
    graphInFlight = (async () => {
      const resourceMap = await getResourceMap(null, {
        view: "detail",
        topN: 200,
      });
      if (
        requestSeq !== graphRequestSeq ||
        !deps.pipelineInspectorScope.isCurrent(scopeToken) ||
        deps.selectedPipeline()
      )
        return;
      if (!resourceMap) {
        if (status) status.textContent = "Runtime resources unavailable.";
        deps.withPreservedScroll(container, () => {
          container.innerHTML =
            '<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">Runtime resources unavailable.</div>';
        });
        return;
      }
      deps.withPreservedScroll(container, () => {
        renderResourceMapInto(container, resourceMap);
      });
      graphRenderedStateKey = "runtime";
      if (status) status.textContent = "Whole Runtime / resource overview";
    })();
    try {
      await graphInFlight;
    } finally {
      if (requestSeq === graphRequestSeq) graphInFlight = null;
    }
    return;
  }
  const requestPipelineId = pipe.id;
  const canRefreshInPlace =
    graphPipelineId === requestPipelineId && graphRenderedStateKey !== null;
  graphPipelineId = requestPipelineId;
  if (status && !canRefreshInPlace) status.textContent = "Loading graph...";
  if (!canRefreshInPlace) {
    deps.withPreservedScroll(container, () => {
      container.innerHTML = `<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">
        Loading graph...
    </div>`;
    });
  }
  graphInFlight = (async () => {
    const [graph, resourceMap] = await Promise.all([
      fetchProcessingGraph(requestPipelineId),
      getResourceMap(requestPipelineId, {
        view: "detail",
        topN: 50,
      }),
    ]);
    if (
      requestSeq !== graphRequestSeq ||
      !deps.pipelineInspectorScope.isCurrent(scopeToken) ||
      deps.selectedPipeline()?.id !== requestPipelineId
    ) {
      return;
    }
    graphPipelineId = requestPipelineId;
    if (!graph || graph.pipelineId !== requestPipelineId) {
      if (status) status.textContent = "Graph unavailable.";
      deps.withPreservedScroll(container, () => {
        container.innerHTML =
          '<div class="text-base-content/60 flex h-full min-h-72 items-center justify-center text-sm">Graph unavailable.</div>';
      });
      return;
    }
    deps.withPreservedScroll(container, () => {
      renderProcessingGraphExplorer(
        container,
        graph as Parameters<typeof renderGraphInto>[1],
      );
    });
    if (resourceMap) {
      pipelineResourceMapCache.set(requestPipelineId, resourceMap);
      const currentPipe = deps.selectedPipeline();
      if (currentPipe?.id === requestPipelineId) {
        deps.withPreservedScroll(container, () =>
          deps.renderSummary(currentPipe),
        );
        deps.renderInspectorResourceDetails(currentPipe, resourceMap);
        deps.renderInspectCheckpointPresentation(currentPipe);
      }
    }
    graphRenderedStateKey = requestStateKey;
    if (status) {
      const inputState =
        pipe.input.status === "on" ? "live" : pipe.input.status;
      status.textContent = `${pipe.name} / processing graph / ${pipe.outs.length} outputs / input ${inputState}`;
    }
  })();
  try {
    await graphInFlight;
  } finally {
    if (requestSeq === graphRequestSeq) graphInFlight = null;
  }
}
