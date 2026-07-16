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
  initDashboardModes,
  openInspectGraph,
  renderDashboardModes,
  setDashboardMode,
  setPipelineWorkspaceView,
} from "../features/modes.js";
import { setPipelineInspectorDependencies } from "../features/pipeline-inspector.js";
import {
  configurePipelineSelectorPresentation,
  selectPipeline,
} from "../features/render.js";
import { getUrlParam } from "../core/utils.js";
import {
  openOutputMonitoringUrl,
  setControlRoomWorkspaceDependencies,
} from "../features/control-room.js";
import { state } from "../core/state.js";
import { buildOverviewViewModel } from "../features/overview-view-model.js";
import {
  configurePipelineOutputOverviewPresentation,
  renderOutsColumn,
  togglePipelineOutputList,
} from "../features/pipeline-output-list.js";
import {
  dashboardV2ExperimentEnabled,
  setDashboardV2OverviewActions,
  setDashboardV2PipelineHeaderActions,
  setDashboardV2PipelineInputStatusActions,
  setDashboardV2PipelineOutputOverviewActions,
  setDashboardV2PipelineSelectorActions,
  updateDashboardV2Overview,
  updateDashboardV2PipelineHeader,
  updateDashboardV2PipelineInputStatus,
  updateDashboardV2PipelineOutputOverview,
  updateDashboardV2PipelineSelector,
} from "./dashboard-v2-loader.js";

let dashboardAppInitialized = false;

export function initDashboardApp(): void {
  if (dashboardAppInitialized) return;
  dashboardAppInitialized = true;
  const dashboardV2Enabled = dashboardV2ExperimentEnabled();
  if (dashboardV2Enabled) {
    configureOverviewPresentation({
      legacyRenderEnabled: false,
      onPresentation: (presentation) => {
        updateDashboardV2Overview(
          buildOverviewViewModel(state.pipelines, state.metrics, presentation),
        );
      },
    });
    setDashboardV2OverviewActions({
      addPipeline: () => void window.addPipeBtn(),
      inspectPipeline: openInspectGraph,
      openPipeline: (pipelineId) => {
        selectPipeline(pipelineId);
        setDashboardMode("pipeline");
      },
      openStatus: () => setDashboardMode("status"),
    });
    configurePipelineSelectorPresentation({
      legacyRenderEnabled: false,
      onPresentation: updateDashboardV2PipelineSelector,
    });
    setDashboardV2PipelineSelectorActions({
      addPipeline: () => void window.addPipeBtn(),
      selectPipeline,
    });
    configurePipelineHeaderPresentation({
      legacyLifecycleControlsEnabled: false,
      legacyRenderEnabled: false,
      onPresentation: updateDashboardV2PipelineHeader,
    });
    setDashboardV2PipelineHeaderActions({
      diagnosePipeline: openDiagnosticsModal,
      editPipeline: (pipelineId) => {
        selectPipeline(pipelineId);
        void editPipeBtn();
      },
      inspectPipeline: openInspectGraph,
      toggleFileIngest: togglePipelineFileIngest,
      toggleRecording: togglePipelineRecording,
    });
    configurePipelineInputStatusPresentation({
      legacyRenderEnabled: false,
      onPresentation: updateDashboardV2PipelineInputStatus,
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
    configurePipelineOutputOverviewPresentation({
      legacyAddActionEnabled: false,
      legacyCardsEnabled: false,
      legacyRenderEnabled: false,
      onPresentation: updateDashboardV2PipelineOutputOverview,
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
      setPipelineWorkspaceView("operate", pipelineId);
    },
  });

  setControlRoomWorkspaceDependencies({
    selectedPipelineId: () => getUrlParam("p"),
    selectPipeline,
    openMonitorView: (pipelineId) => {
      if (pipelineId !== null) selectPipeline(pipelineId);
      setPipelineWorkspaceView("monitor", pipelineId);
    },
  });

  initDashboardModes();
}
