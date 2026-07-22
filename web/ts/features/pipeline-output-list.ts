import { state } from "../core/state.js";
import { pipelineViewDependencies } from "./pipeline-dependencies.js";
import {
  getOutputControlError,
  getOutputControlIntent,
} from "./output-control-state.js";
import {
  buildPipelineOutputOverviewModel,
} from "./pipeline-operate-view-model.js";
import type { PipelineOutputOverviewModel } from "./pipeline-operate-view-model.js";

const expandedOutputLists = new Set<string>();
let outputOverviewPresentationHook:
  ((model: PipelineOutputOverviewModel | null) => void) | null = null;

export function configurePipelineOutputOverviewPresentation(options: {
  onPresentation?: (model: PipelineOutputOverviewModel | null) => void;
}): void {
  outputOverviewPresentationHook = options.onPresentation || null;
}

export function togglePipelineOutputList(pipeId: string): void {
  if (expandedOutputLists.has(pipeId)) {
    expandedOutputLists.delete(pipeId);
  } else {
    expandedOutputLists.add(pipeId);
  }
  renderOutsColumn(pipeId);
}

export function renderOutsColumn(selectedPipe: string | null): void {
  if (!selectedPipe) {
    outputOverviewPresentationHook?.(null);
    document.getElementById("outs-col")?.classList.add("hidden");
    return;
  }

  document.getElementById("outs-col")?.classList.remove("hidden");

  const pipe = state.pipelines.find((candidate) => candidate.id === selectedPipe);
  if (!pipe) {
    outputOverviewPresentationHook?.(null);
    console.error("Pipeline not found:", selectedPipe);
    return;
  }

  outputOverviewPresentationHook?.(
    buildPipelineOutputOverviewModel(
      state.pipelines,
      selectedPipe,
      pipe.outs.map((output) => ({
        outputId: output.id,
        intent: getOutputControlIntent(pipe.id, output.id),
        error: getOutputControlError(pipe.id, output.id),
        busy: Boolean(
          pipelineViewDependencies.isOutputToggleBusy?.(pipe.id, output.id),
        ),
      })),
      expandedOutputLists.has(pipe.id),
    ),
  );
}
