import type {
  PipelineOperateHeaderModel,
  PipelineOperateInputStatusModel,
} from "../pipeline-operate-view-model.js";

// ── Legacy rendering flags / presentation hooks ────────────────────────

export let pipelineHeaderPresentationHook:
  ((model: PipelineOperateHeaderModel | null) => void) | null = null;
export let pipelineInputStatusPresentationHook:
  ((model: PipelineOperateInputStatusModel | null) => void) | null = null;

// ── Configure functions ────────────────────────────────────────────────

export function configurePipelineHeaderPresentation(options: {
  onPresentation?: (model: PipelineOperateHeaderModel | null) => void;
}): void {
  pipelineHeaderPresentationHook = options.onPresentation || null;
}

export function configurePipelineInputStatusPresentation(options: {
  onPresentation?: (model: PipelineOperateInputStatusModel | null) => void;
}): void {
  pipelineInputStatusPresentationHook = options.onPresentation || null;
}
