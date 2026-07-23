import {
  clearIncidentsMode,
  renderIncidentsMode,
  type IncidentPipelineOption,
} from "./incidents.js";

interface DashboardV2IncidentsBodyOptions {
  readonly pipelines: IncidentPipelineOption[];
  readonly navigateToPipeline: (pipelineId: string) => void;
}

export function renderDashboardV2IncidentsBody(
  containerId: string,
  options: DashboardV2IncidentsBodyOptions,
): void {
  renderIncidentsMode({
    active: true,
    containerId,
    navigateToPipeline: options.navigateToPipeline,
    pipelines: options.pipelines,
    routeChrome: false,
  });
  const container = document.getElementById(containerId);
  if (container) container.dataset.incidentsRouteBody = "v2";
}

export function clearDashboardV2IncidentsBody(): void {
  clearIncidentsMode();
}
