import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";

import type {
  DashboardV2PipelineDetailsPlaceholder,
  DashboardV2OverviewActions,
  DashboardV2PipelineHeaderActions,
  DashboardV2PipelineInputStatusActions,
  DashboardV2PipelineOutputOverviewActions,
  DashboardV2PipelineSelectorActions,
} from "./dashboard-v2-loader.js";
import type {
  OverviewViewModel,
} from "../features/overview-view-model.js";
import type {
  PipelineOperateHeaderModel,
  PipelineOperateInputStatusModel,
  PipelineOperateSelectorModel,
  PipelineOutputOverviewModel,
} from "../features/pipeline-operate-view-model.js";

import { DashboardV2Overview } from "./dashboard-v2/overview.js";
import { DashboardV2PipelineSelector } from "./dashboard-v2/pipeline-selector.js";
import {
  DashboardV2PipelineHeader,
  DashboardV2PipelineDetailsPlaceholderCard,
} from "./dashboard-v2/pipeline-header.js";
import { DashboardV2PipelineInputStatus } from "./dashboard-v2/pipeline-input-status.js";
import { DashboardV2PipelineOutputOverview } from "./dashboard-v2/pipeline-output-overview.js";

const dashboardV2Container = document.getElementById("dashboard-v2-root");
if (!dashboardV2Container)
  throw new Error("Dashboard v2 experiment root is missing");
const container: HTMLElement = dashboardV2Container;
container.dataset.uiV2Seam = "UI v2 seam active";
let root: Root | null = null;
const pipelineSelectorContainer = document.getElementById(
  "dashboard-v2-pipeline-selector-root",
);
if (!pipelineSelectorContainer) {
  throw new Error("Dashboard v2 pipeline selector root is missing");
}
const selectorContainer: HTMLElement = pipelineSelectorContainer;
let selectorRoot: Root | null = null;
const pipelineHeaderContainer = document.getElementById(
  "dashboard-v2-pipeline-header-root",
);
if (!pipelineHeaderContainer) {
  throw new Error("Dashboard v2 pipeline header root is missing");
}
const headerContainer: HTMLElement = pipelineHeaderContainer;
let headerRoot: Root | null = null;
const pipelineInputStatusContainer = document.getElementById(
  "dashboard-v2-pipeline-input-status-root",
);
if (!pipelineInputStatusContainer) {
  throw new Error("Dashboard v2 pipeline input status root is missing");
}
const inputStatusContainer: HTMLElement = pipelineInputStatusContainer;
let inputStatusRoot: Root | null = null;
const pipelineOutputOverviewContainer = document.getElementById(
  "dashboard-v2-pipeline-output-overview-root",
);
if (!pipelineOutputOverviewContainer) {
  throw new Error("Dashboard v2 pipeline output overview root is missing");
}
const outputOverviewContainer: HTMLElement = pipelineOutputOverviewContainer;
let outputOverviewRoot: Root | null = null;

export function renderDashboardV2Overview(
  model: OverviewViewModel,
  actions: DashboardV2OverviewActions,
): void {
  container.hidden = false;
  root ??= createRoot(container);
  root.render(<DashboardV2Overview actions={actions} model={model} />);
}

export function renderDashboardV2PipelineSelector(
  model: PipelineOperateSelectorModel,
  actions: DashboardV2PipelineSelectorActions,
): void {
  selectorContainer.hidden = false;
  selectorRoot ??= createRoot(selectorContainer);
  selectorRoot.render(
    <DashboardV2PipelineSelector actions={actions} model={model} />,
  );
}

export function renderDashboardV2PipelineHeader(
  model: PipelineOperateHeaderModel | null,
  actions: DashboardV2PipelineHeaderActions,
  placeholder: DashboardV2PipelineDetailsPlaceholder | null = null,
): void {
  headerRoot ??= createRoot(headerContainer);
  headerContainer.hidden = model === null && placeholder === null;
  headerRoot.render(
    model ? (
      <DashboardV2PipelineHeader actions={actions} model={model} />
    ) : placeholder ? (
      <DashboardV2PipelineDetailsPlaceholderCard model={placeholder} />
    ) : null,
  );
}

export function renderDashboardV2PipelineInputStatus(
  model: PipelineOperateInputStatusModel | null,
  actions: DashboardV2PipelineInputStatusActions,
): void {
  inputStatusRoot ??= createRoot(inputStatusContainer);
  inputStatusContainer.hidden = model === null;
  inputStatusRoot.render(
    model ? (
      <DashboardV2PipelineInputStatus actions={actions} model={model} />
    ) : null,
  );
}

export function renderDashboardV2PipelineOutputOverview(
  model: PipelineOutputOverviewModel | null,
  actions: DashboardV2PipelineOutputOverviewActions,
): void {
  outputOverviewRoot ??= createRoot(outputOverviewContainer);
  outputOverviewContainer.hidden = model === null;
  outputOverviewRoot.render(
    model ? (
      <DashboardV2PipelineOutputOverview actions={actions} model={model} />
    ) : null,
  );
}

export function clearDashboardV2PipelineOperate(): void {
  selectorRoot?.render(null);
  headerRoot?.render(null);
  inputStatusRoot?.render(null);
  outputOverviewRoot?.render(null);
  selectorContainer.hidden = true;
  headerContainer.hidden = true;
  inputStatusContainer.hidden = true;
  outputOverviewContainer.hidden = true;
}
