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

function setHtmlIfChanged(target: HTMLElement | null, html: string): boolean {
  if (!target || target.innerHTML === html) return false;
  target.innerHTML = html;
  return true;
}

function renderStatsColumn(selectedPipe: string | null): void {
  const statsCol = document.getElementById("stats-col") as HTMLElement | null;
  if (selectedPipe) {
    statsCol?.classList.add("hidden");
    return;
  } else {
    statsCol?.classList.remove("hidden");
  }

  if (statsCol) {
    const nextHtml = `<section class="flex min-h-[22rem] items-center justify-center">
            <div class="max-w-md text-center">
                <h2 class="text-lg font-semibold">${state.pipelines.length ? "No pipeline selected" : "No pipelines configured"}</h2>
                <p class="text-base-content/60 mt-2 text-sm">${state.pipelines.length ? "Pipeline details, ingest preview, outputs, and controls appear here." : "Create a pipeline to start configuring ingest and outputs."}</p>
                <button type="button" class="btn btn-sm btn-accent btn-outline mt-4" id="pipeline-empty-add-btn">Add Pipeline</button>
            </div>
        </section>`;
    if (setHtmlIfChanged(statsCol, nextHtml)) {
      const addBtn = document.getElementById("pipeline-empty-add-btn");
      if (addBtn) addBtn.onclick = () => void window.addPipeBtn();
    }
  }
  return;
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

  const gridElem = document.getElementById("dashboard-grid");
  if (!gridElem) {
    return;
  }
  gridElem.classList.toggle("has-selected-pipeline", Boolean(selectedPipe));
  gridElem.style.gridTemplateColumns = "";

  pipelineSelectorPresentationHook?.(
    buildPipelineOperateSelectorModel(state.pipelines, selectedPipe),
  );
  renderPipelineInfoColumn(selectedPipe);
  renderOutsColumn(selectedPipe);
  renderStatsColumn(selectedPipe);
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

export { renderStatsColumn, renderPipelines, renderMetrics, selectPipeline };
