import {
  awaitDashboardRuntimeMutationConvergence,
  refreshDashboard,
  refreshDashboardRuntime,
  setDashboardHooks,
  updateDashboardPipelineFileIngestState,
  updateDashboardPipelineRecordingState,
} from "../features/dashboard.js";
import {
  addOutBtn,
  deleteOutBtn,
  editPipeBtn,
  editOutBtn,
  isOutputToggleBusy,
  startOutBtn,
  stopOutBtn,
} from "../features/editor.js";
import {
  openOutputHistoryModal,
  openPipelineHistoryModal,
} from "../history/controller.js";
import {
  configurePipelineHeaderPresentation,
  configurePipelineInputStatusPresentation,
  cancelPipelineAudioTrackEdit,
  clearPipelineInputPreview,
  copyPipelineIngestUrl,
  copyPipelineStreamKey,
  editPipelineAudioTrack,
  mountPipelineInputPreview,
  savePipelineAudioTrack,
  selectPipelineIngestProtocol,
  setPipelineViewDependencies,
  togglePipelineFileIngest,
  togglePipelineRecording,
  updatePipelineAudioTrackDraft,
} from "../features/pipeline-view.js";
import { openDiagnosticsModal } from "../features/diagnostics.js";
import {
  openPublisherHealthModal,
  renderPublisherHealthModal,
} from "../features/publisher-health.js";
import {
  configureOverviewPresentation,
  configureDashboardModePresentationSync,
  initDashboardModes,
  openInspectGraph,
  renderDashboardModes,
  setDashboardMode,
  setPipelineWorkspaceView,
} from "../features/modes.js";
import {
  configurePipelineInspectCheckpointPresentation,
  setPipelineInspectorDependencies,
} from "../features/pipeline-inspector.js";
import {
  configurePipelineSelectorPresentation,
  renderPipelines,
  selectPipeline,
} from "../features/render.js";
import { getUrlParam } from "../core/utils.js";
import {
  configureControlRoomCheckpointPresentation,
  openOutputMonitoringUrl,
  setControlRoomWorkspaceDependencies,
} from "../features/control-room.js";
import { state } from "../core/state.js";
import type { DashboardLocation } from "../core/pipeline-workspace.js";
import { buildOverviewViewModel } from "../features/overview-view-model.js";
import {
  configurePipelineOutputOverviewPresentation,
  renderOutsColumn,
  togglePipelineOutputList,
} from "../features/pipeline-output-list.js";
import {
  dashboardV2ExperimentEnabled,
  setDashboardV2PresentationScope,
  setDashboardV2OverviewActions,
  setDashboardV2ControlRoomActions,
  setDashboardV2PipelineHeaderActions,
  setDashboardV2PipelineInspectActions,
  setDashboardV2PipelineInputStatusActions,
  setDashboardV2PipelineOutputOverviewActions,
  setDashboardV2PipelineSelectorActions,
  updateDashboardV2Overview,
  updateDashboardV2ControlRoomCheckpoint,
  updateDashboardV2PipelineHeader,
  updateDashboardV2PipelineInspectCheckpoint,
  updateDashboardV2PipelineInputStatus,
  updateDashboardV2PipelineOutputOverview,
  updateDashboardV2PipelineSelector,
} from "./dashboard-v2-loader.js";

let dashboardAppInitialized = false;

function syncDashboardV2Presentation(location: DashboardLocation): void {
  const dashboardV2Enabled = dashboardV2ExperimentEnabled();
  const overviewV2Active = dashboardV2Enabled && location.mode === "overview";
  const pipelineV2Active =
    dashboardV2Enabled &&
    location.mode === "pipeline" &&
    location.pipelineView === "operate";
  const pipelineInspectV2Active =
    dashboardV2Enabled &&
    location.mode === "pipeline" &&
    location.pipelineView === "inspect";
  const controlRoomV2Active =
    dashboardV2Enabled &&
    location.mode === "pipeline" &&
    location.pipelineView === "monitor";

  setDashboardV2PresentationScope({
    overviewActive: overviewV2Active,
    pipelineActive: pipelineV2Active,
    pipelineInspectActive: pipelineInspectV2Active,
    controlRoomActive: controlRoomV2Active,
  });

  configureOverviewPresentation({
    legacyRenderEnabled: !overviewV2Active,
    onPresentation: overviewV2Active
      ? (presentation) => {
          updateDashboardV2Overview(
            buildOverviewViewModel(
              state.pipelines,
              state.metrics,
              presentation,
            ),
          );
        }
      : undefined,
  });
  configurePipelineSelectorPresentation({
    legacyRenderEnabled: !pipelineV2Active,
    onPresentation: pipelineV2Active
      ? updateDashboardV2PipelineSelector
      : undefined,
  });
  configurePipelineHeaderPresentation({
    legacyLifecycleControlsEnabled: !pipelineV2Active,
    legacyRenderEnabled: !pipelineV2Active,
    onPresentation: pipelineV2Active
      ? updateDashboardV2PipelineHeader
      : undefined,
  });
  configurePipelineInputStatusPresentation({
    legacyRenderEnabled: !pipelineV2Active,
    onPresentation: pipelineV2Active
      ? updateDashboardV2PipelineInputStatus
      : undefined,
  });
  configurePipelineOutputOverviewPresentation({
    legacyAddActionEnabled: !pipelineV2Active,
    legacyCardsEnabled: !pipelineV2Active,
    legacyRenderEnabled: !pipelineV2Active,
    onPresentation: pipelineV2Active
      ? updateDashboardV2PipelineOutputOverview
      : undefined,
  });
  configurePipelineInspectCheckpointPresentation({
    onPresentation: pipelineInspectV2Active
      ? updateDashboardV2PipelineInspectCheckpoint
      : undefined,
  });
  configureControlRoomCheckpointPresentation({
    onPresentation: controlRoomV2Active
      ? updateDashboardV2ControlRoomCheckpoint
      : undefined,
  });
}

export function initDashboardApp(): void {
  if (dashboardAppInitialized) return;
  dashboardAppInitialized = true;
  const dashboardV2Enabled = dashboardV2ExperimentEnabled();
  if (dashboardV2Enabled) {
    configureDashboardModePresentationSync(syncDashboardV2Presentation);
    setDashboardV2OverviewActions({
      addPipeline: () => void window.addPipeBtn(),
      inspectPipeline: (pipelineId) =>
        openInspectGraph(pipelineId, { focus: "panel" }),
      openPipeline: (pipelineId) => {
        setPipelineWorkspaceView("operate", pipelineId, { focus: "panel" });
        renderPipelines();
      },
      openStatus: () => setDashboardMode("status", { focus: "panel" }),
    });
    setDashboardV2PipelineSelectorActions({
      addPipeline: () => void window.addPipeBtn(),
      selectPipeline,
    });
    setDashboardV2PipelineHeaderActions({
      diagnosePipeline: openDiagnosticsModal,
      editPipeline: (pipelineId) => {
        selectPipeline(pipelineId);
        void editPipeBtn();
      },
      inspectPipeline: (pipelineId) =>
        openInspectGraph(pipelineId, { focus: "panel" }),
      toggleFileIngest: togglePipelineFileIngest,
      toggleRecording: togglePipelineRecording,
    });
    setDashboardV2PipelineInspectActions({
      openPipeline: (pipelineId) => {
        selectPipeline(pipelineId);
        setPipelineWorkspaceView("operate", pipelineId, { focus: "panel" });
      },
      runDiagnostics: openDiagnosticsModal,
    });
    setDashboardV2ControlRoomActions({
      openPipeline: (pipelineId) => {
        selectPipeline(pipelineId);
        setPipelineWorkspaceView("operate", pipelineId, { focus: "panel" });
      },
    });
    setDashboardV2PipelineInputStatusActions({
      cancelAudioTrackEdit: cancelPipelineAudioTrackEdit,
      clearPreview: clearPipelineInputPreview,
      copyIngestUrl: copyPipelineIngestUrl,
      copyStreamKey: copyPipelineStreamKey,
      editAudioTrack: editPipelineAudioTrack,
      mountPreview: mountPipelineInputPreview,
      saveAudioTrack: savePipelineAudioTrack,
      selectProtocol: selectPipelineIngestProtocol,
      updateAudioTrackDraft: updatePipelineAudioTrackDraft,
    });
    setDashboardV2PipelineOutputOverviewActions({
      addOutput: (pipelineId) => {
        selectPipeline(pipelineId);
        void addOutBtn();
      },
      deleteOutput: async (pipelineId, outputId) => {
        await deleteOutBtn(pipelineId, outputId);
        renderOutsColumn(pipelineId);
      },
      editOutput: (pipelineId, outputId) => {
        void editOutBtn(pipelineId, outputId);
      },
      monitorOutput: (pipelineId, outputId) => {
        const monitoringUrl = state.pipelines
          .find((pipeline) => pipeline.id === pipelineId)
          ?.outs.find((output) => output.id === outputId)?.monitoringUrl;
        openOutputMonitoringUrl(monitoringUrl);
      },
      openOutputHistory: (pipelineId, outputId, outputName) => {
        void openOutputHistoryModal(pipelineId, outputId, outputName);
      },
      toggleOutput: async (pipelineId, outputId) => {
        const output = state.pipelines
          .find((pipeline) => pipeline.id === pipelineId)
          ?.outs.find((candidate) => candidate.id === outputId);
        if (!output) return;
        const mutation =
          output.desiredState === "stopped"
            ? startOutBtn(pipelineId, outputId)
            : stopOutBtn(pipelineId, outputId);
        renderOutsColumn(pipelineId);
        try {
          await mutation;
        } finally {
          renderOutsColumn(pipelineId);
        }
      },
      toggleOutputList: togglePipelineOutputList,
    });
  } else {
    configureDashboardModePresentationSync(null);
  }

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
    awaitDashboardRuntimeMutationConvergence,
    updateDashboardPipelineFileIngestState,
    updateDashboardPipelineRecordingState,
    openDiagnosticsModal,
    openGraphExplorer: openInspectGraph,
    openOutputMonitoringUrl,
  });

  setPipelineInspectorDependencies({
    selectPipeline,
    openOperateView: (pipelineId) => {
      selectPipeline(pipelineId);
      setPipelineWorkspaceView("operate", pipelineId, { focus: "panel" });
    },
  });

  setControlRoomWorkspaceDependencies({
    selectedPipelineId: () => getUrlParam("p"),
    selectPipeline,
    openMonitorView: (pipelineId) => {
      if (pipelineId !== null) selectPipeline(pipelineId);
      setPipelineWorkspaceView("monitor", pipelineId, { focus: "panel" });
    },
  });

  initDashboardModes();
}
