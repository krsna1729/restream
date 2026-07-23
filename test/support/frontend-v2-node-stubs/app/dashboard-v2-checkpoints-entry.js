function syncContainer(id, model) {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing dashboard v2 test host: ${id}`);
  element.hidden = model === null;
  element.dataset.nodeTestV2Bundle = "mounted";
}

export function renderDashboardV2PipelineInspectCheckpoint(model) {
  syncContainer("dashboard-v2-pipeline-inspect-root", model);
}

export function renderDashboardV2ControlRoomCheckpoint(model) {
  syncContainer("dashboard-v2-control-room-root", model);
}

export function renderDashboardV2IncidentsCheckpoint(model) {
  syncContainer("dashboard-v2-incidents-root", model);
}

export function renderDashboardV2TelemetryCheckpoint(model) {
  syncContainer("dashboard-v2-telemetry-root", model);
}

export function renderDashboardV2StatusCheckpoint(model) {
  syncContainer("dashboard-v2-status-root", model);
}

export function renderDashboardV2MediaCheckpoint(model) {
  syncContainer("dashboard-v2-media-root", model);
}

export function renderDashboardV2SettingsCheckpoint(model) {
  syncContainer("dashboard-v2-settings-root", model);
}
