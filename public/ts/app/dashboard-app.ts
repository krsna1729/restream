import {
  refreshDashboard,
  refreshDashboardRuntime,
  setDashboardHooks,
} from "../features/dashboard.js";
import {
  deleteOutBtn,
  editOutBtn,
  isOutputToggleBusy,
  startOutBtn,
  stopOutBtn,
} from "../features/editor.js";
import {
  openOutputHistoryModal,
  openPipelineHistoryModal,
} from "../history/controller.js";
import { setPipelineViewDependencies } from "../features/pipeline-view.js";
import { openDiagnosticsModal } from "../features/diagnostics.js";
import {
  openPublisherHealthModal,
  renderPublisherHealthModal,
} from "../features/publisher-health.js";
import {
  initDashboardModes,
  openInspectGraph,
  renderDashboardModes,
  setPipelineWorkspaceView,
} from "../features/modes.js";
import { setPipelineInspectorDependencies } from "../features/pipeline-inspector.js";
import { selectPipeline } from "../features/render.js";

let dashboardAppInitialized = false;

export function initDashboardApp(): void {
  if (dashboardAppInitialized) return;
  dashboardAppInitialized = true;

  setDashboardHooks({
    afterRender: () => {
      renderPublisherHealthModal();
      renderDashboardModes();
    },
  });

  setPipelineViewDependencies({
    openPipelineHistoryModal,
    openPublisherHealthModal,
    isOutputToggleBusy,
    startOutBtn,
    stopOutBtn,
    openOutputHistoryModal,
    editOutBtn,
    deleteOutBtn,
    refreshDashboard,
    refreshDashboardRuntime,
    openDiagnosticsModal,
    openGraphExplorer: openInspectGraph,
  });

  setPipelineInspectorDependencies({
    selectPipeline,
    openOperateView: (pipelineId) => {
      selectPipeline(pipelineId);
      setPipelineWorkspaceView("operate", pipelineId);
    },
  });

  initDashboardModes();
}
