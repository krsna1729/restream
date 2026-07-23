import {
  renderPipelineInspector,
  setPipelineInspectorContainerId,
} from "./pipeline-inspector/index.js";

export function renderDashboardV2PipelineInspectBody(
  containerId: string,
): void {
  setPipelineInspectorContainerId(containerId);
  renderPipelineInspector();
  const container = document.getElementById(containerId);
  if (container) container.dataset.pipelineInspectRouteBody = "v2";
}
