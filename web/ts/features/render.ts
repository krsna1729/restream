import {
  getUrlParam,
  setServerConfig,
  setUrlParam,
  writeSelectedPipelineHint,
} from "../core/utils.js";
import { renderPipelineInfoColumn, renderOutsColumn } from "./pipeline-view/index.js";
import { renderHealthBanner, renderServerMetrics } from "./metrics.js";
import { state } from "../core/state.js";
import { buildPipelineOperateSelectorModel } from "./pipeline-operate-view-model.js";
import type { PipelineOperateSelectorModel } from "./pipeline-operate-view-model.js";

let pipelineSelectorPresentationHook:
  ((model: PipelineOperateSelectorModel) => void) | null = null;

export function configurePipelineSelectorPresentation(options: {
  onPresentation?: (model: PipelineOperateSelectorModel) => void;
}): void {
  pipelineSelectorPresentationHook = options.onPresentation || null;
}

function getRenderableSelectedPipe(): string | null {
  const selectedPipe = getUrlParam("p");
  if (!selectedPipe) return null;
  return state.pipelines.some((pipe) => pipe.id === selectedPipe)
    ? selectedPipe
    : null;
}

function currentDashboardMode(): string {
  return new URL(window.location.href).searchParams.get("mode") || "overview";
}

function selectOnlyPipelineWhenUseful(): string | null {
  if (getUrlParam("p")) return null;
  if (currentDashboardMode() !== "pipeline") return null;
  if (state.pipelines.length !== 1) return null;
  const onlyPipeline = state.pipelines[0];
  setUrlParam("p", onlyPipeline.id);
  return onlyPipeline.id;
}

function renderPipelines(): void {
  const selectedPipe =
    getRenderableSelectedPipe() || selectOnlyPipelineWhenUseful();
  writeSelectedPipelineHint(
    selectedPipe
      ? state.pipelines.find((pipe) => pipe.id === selectedPipe) || null
      : null,
  );

  pipelineSelectorPresentationHook?.(
    buildPipelineOperateSelectorModel(state.pipelines, selectedPipe),
  );
  renderPipelineInfoColumn(selectedPipe);
  renderOutsColumn(selectedPipe);
}

function renderMetrics(): void {
  renderHealthBanner();
  renderServerMetrics();
}

function selectPipeline(id: string | null): void {
  setUrlParam("p", id);
  renderPipelines();
  setServerConfig(state.config?.serverName);
}

// HTML-bound handler — keep accessible as a global
window.selectPipeline = selectPipeline;

export { renderPipelines, renderMetrics, selectPipeline };
