function syncContainer(id, model) {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing dashboard v2 test host: ${id}`);
  element.hidden = model === null;
  element.dataset.nodeTestV2Bundle = "mounted";
}

export function renderDashboardV2Overview(model) {
  syncContainer("dashboard-v2-root", model);
}

export function renderDashboardV2PipelineSelector(model) {
  syncContainer("dashboard-v2-pipeline-selector-root", model);
}

export function renderDashboardV2PipelineHeader(model) {
  syncContainer("dashboard-v2-pipeline-header-root", model);
}

export function renderDashboardV2PipelineInputStatus(model) {
  syncContainer("dashboard-v2-pipeline-input-status-root", model);
}

export function renderDashboardV2PipelineOutputOverview(model) {
  syncContainer("dashboard-v2-pipeline-output-overview-root", model);
}
