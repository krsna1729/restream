import { clearInputPreview } from "../input-preview.js";
import type {
  PipelineOperateHeaderModel,
  PipelineOperateInputStatusModel,
} from "../pipeline-operate-view-model.js";

// ── Legacy rendering flags / presentation hooks ────────────────────────

export let pipelineHeaderPresentationHook:
  ((model: PipelineOperateHeaderModel | null) => void) | null = null;
export let legacyPipelineInputStatusRenderEnabled = true;
export let legacyPipelineAudioTracksRenderEnabled = true;
export let legacyPipelinePreviewRenderEnabled = true;
export let pipelineInputStatusPresentationHook:
  ((model: PipelineOperateInputStatusModel | null) => void) | null = null;

// ── Configure functions ────────────────────────────────────────────────

export function configurePipelineHeaderPresentation(options: {
  onPresentation?: (model: PipelineOperateHeaderModel | null) => void;
}): void {
  pipelineHeaderPresentationHook = options.onPresentation || null;
}

export function configurePipelineInputStatusPresentation(options: {
  legacyRenderEnabled: boolean;
  onPresentation?: (model: PipelineOperateInputStatusModel | null) => void;
}): void {
  legacyPipelineInputStatusRenderEnabled = options.legacyRenderEnabled;
  legacyPipelineAudioTracksRenderEnabled = options.legacyRenderEnabled;
  legacyPipelinePreviewRenderEnabled = options.legacyRenderEnabled;
  pipelineInputStatusPresentationHook = options.onPresentation || null;
  const publisherMeta = document.getElementById("publisher-meta");
  if (publisherMeta)
    publisherMeta.hidden = !legacyPipelineInputStatusRenderEnabled;
  for (const id of [
    "pipeline-input-legacy-traffic-heading",
    "pipeline-input-legacy-traffic",
    "pipeline-input-legacy-video-heading",
    "pipeline-input-legacy-video",
    "pipeline-input-legacy-audio-heading",
    "input-audio-tracks",
    "video-player",
  ]) {
    const element = document.getElementById(id);
    if (element) element.hidden = !legacyPipelineInputStatusRenderEnabled;
  }
  for (const id of [
    "stream-key-section",
    "ingest-url-section",
    "file-source-section",
  ]) {
    const element = document.getElementById(id);
    if (element) element.hidden = !legacyPipelineInputStatusRenderEnabled;
  }
  if (!legacyPipelineAudioTracksRenderEnabled) {
    document.getElementById("input-audio-tracks")?.replaceChildren();
  }
  if (!legacyPipelinePreviewRenderEnabled) {
    clearInputPreview(document.getElementById("video-player"));
  }
}
