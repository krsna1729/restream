import {
  clearEngineerTelemetryMode,
  renderEngineerTelemetryMode,
  type TelemetryPipelineOption,
} from "./engineer-telemetry.js";

interface DashboardV2TelemetryBodyOptions {
  readonly pipelines: TelemetryPipelineOption[];
}

export function renderDashboardV2TelemetryBody(
  containerId: string,
  options: DashboardV2TelemetryBodyOptions,
): void {
  renderEngineerTelemetryMode({
    active: true,
    containerId,
    pipelines: options.pipelines,
    routeChrome: false,
  });
  const container = document.getElementById(containerId);
  if (container) container.dataset.telemetryRouteBody = "v2";
}

export function clearDashboardV2TelemetryBody(): void {
  clearEngineerTelemetryMode();
}
