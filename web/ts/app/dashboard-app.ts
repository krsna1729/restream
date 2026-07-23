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
  deletePipeBtn,
  deleteOutBtn,
  editPipeBtn,
  editOutBtn,
  isOutputToggleBusy,
  startOutBtn,
  stopOutBtn,
} from "../features/editor/index.js";
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
} from "../features/pipeline-view/index.js";
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
} from "./modes.js";
import {
  configurePipelineInspectCheckpointPresentation,
  setPipelineInspectorDependencies,
} from "../features/pipeline-inspector/index.js";
import {
  configurePipelineSelectorPresentation,
  renderPipelines,
  selectPipeline,
} from "../features/render.js";
import {
  createPipelineInput,
  deletePipelineInput,
  getPipelineInputs,
  promotePipelineInput,
  updatePipelineInput,
} from "../core/api.js";
import {
  copyText,
  getUrlParam,
  showCopiedNotification,
} from "../core/utils.js";
import {
  configureControlRoomCheckpointPresentation,
  openOutputMonitoringUrl,
  setControlRoomWorkspaceDependencies,
} from "../features/control-room/index.js";
import { configureIncidentsCheckpointPresentation } from "../features/incidents.js";
import { configureMediaCheckpointPresentation } from "../features/media-library.js";
import { configureTelemetryCheckpointPresentation } from "../features/engineer-telemetry.js";
import { configureSettingsCheckpointPresentation } from "../features/settings.js";
import { configureStatusCheckpointPresentation } from "../features/status.js";
import { state } from "../core/state.js";
import type { DashboardLocation } from "../core/pipeline-workspace.js";
import { buildOverviewViewModel } from "../features/overview-view-model.js";
import {
  configurePipelineOutputOverviewPresentation,
  renderOutsColumn,
  togglePipelineOutputList,
} from "../features/pipeline-output-list.js";
import {
  setDashboardV2PresentationScope,
  setDashboardV2OverviewActions,
  setDashboardV2ControlRoomActions,
  setDashboardV2IncidentsActions,
  setDashboardV2MediaActions,
  setDashboardV2PipelineHeaderActions,
  setDashboardV2PipelineInspectActions,
  setDashboardV2PipelineInputStatusActions,
  setDashboardV2PipelineOutputOverviewActions,
  setDashboardV2PipelineSelectorActions,
  setDashboardV2SettingsActions,
  setDashboardV2StatusActions,
  setDashboardV2TelemetryActions,
  updateDashboardV2Overview,
  updateDashboardV2ControlRoomCheckpoint,
  updateDashboardV2IncidentsCheckpoint,
  updateDashboardV2MediaCheckpoint,
  updateDashboardV2PipelineHeader,
  updateDashboardV2PipelineInspectCheckpoint,
  updateDashboardV2PipelineInputStatus,
  updateDashboardV2PipelineOutputOverview,
  updateDashboardV2PipelineSelector,
  updateDashboardV2SettingsCheckpoint,
  updateDashboardV2StatusCheckpoint,
  updateDashboardV2TelemetryCheckpoint,
} from "./dashboard-v2-loader.js";

let dashboardAppInitialized = false;

function syncDashboardV2Presentation(location: DashboardLocation): void {
  const overviewV2Active = location.mode === "overview";
  const pipelineV2Active =
    location.mode === "pipeline" &&
    location.pipelineView === "operate";
  const pipelineInspectV2Active =
    location.mode === "pipeline" &&
    location.pipelineView === "inspect";
  const controlRoomV2Active =
    location.mode === "pipeline" &&
    location.pipelineView === "monitor";
  const incidentsV2Active = location.mode === "incidents";
  const telemetryV2Active = location.mode === "telemetry";
  const statusV2Active = location.mode === "status";
  const mediaV2Active = location.mode === "media";
  const settingsV2Active = location.mode === "settings";

  setDashboardV2PresentationScope({
    overviewActive: overviewV2Active,
    pipelineActive: pipelineV2Active,
    pipelineInspectActive: pipelineInspectV2Active,
    controlRoomActive: controlRoomV2Active,
    incidentsActive: incidentsV2Active,
    telemetryActive: telemetryV2Active,
    statusActive: statusV2Active,
    mediaActive: mediaV2Active,
    settingsActive: settingsV2Active,
  });

  configureOverviewPresentation({
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
    onPresentation: pipelineV2Active
      ? updateDashboardV2PipelineSelector
      : undefined,
  });
  configurePipelineHeaderPresentation({
    onPresentation: pipelineV2Active
      ? updateDashboardV2PipelineHeader
      : undefined,
  });
  configurePipelineInputStatusPresentation({
    onPresentation: pipelineV2Active
      ? updateDashboardV2PipelineInputStatus
      : undefined,
  });
  configurePipelineOutputOverviewPresentation({
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
  configureIncidentsCheckpointPresentation({
    onPresentation: incidentsV2Active
      ? updateDashboardV2IncidentsCheckpoint
      : undefined,
  });
  configureTelemetryCheckpointPresentation({
    onPresentation: telemetryV2Active
      ? updateDashboardV2TelemetryCheckpoint
      : undefined,
  });
  configureStatusCheckpointPresentation({
    onPresentation: statusV2Active ? updateDashboardV2StatusCheckpoint : undefined,
    v2Active: statusV2Active,
  });
  configureMediaCheckpointPresentation({
    onPresentation: mediaV2Active ? updateDashboardV2MediaCheckpoint : undefined,
    v2Active: mediaV2Active,
  });
  configureSettingsCheckpointPresentation({
    onPresentation: settingsV2Active
      ? updateDashboardV2SettingsCheckpoint
      : undefined,
    v2Active: settingsV2Active,
  });
}

export function initDashboardApp(): void {
  if (dashboardAppInitialized) return;
  dashboardAppInitialized = true;
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
    addPipeline: () => void window.addPipeBtn(),
    deletePipeline: (pipelineId) => {
      selectPipeline(pipelineId);
      void deletePipeBtn();
    },
    diagnosePipeline: openDiagnosticsModal,
    editPipeline: (pipelineId) => {
      selectPipeline(pipelineId);
      void editPipeBtn();
    },
    inspectPipeline: (pipelineId) =>
      openInspectGraph(pipelineId, { focus: "panel" }),
    openHistory: openPipelineHistoryModal,
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
  setDashboardV2IncidentsActions({
    openTelemetry: () => setDashboardMode("telemetry", { focus: "panel" }),
  });
  setDashboardV2TelemetryActions({
    openStatus: () => setDashboardMode("status", { focus: "panel" }),
  });
  setDashboardV2StatusActions({
    openTelemetry: () => setDashboardMode("telemetry", { focus: "panel" }),
  });
  setDashboardV2MediaActions({
    openOverview: () => setDashboardMode("overview", { focus: "panel" }),
  });
  setDashboardV2SettingsActions({
    openStatus: () => setDashboardMode("status", { focus: "panel" }),
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
    copyValue: async (value) => {
      if (await copyText(value)) showCopiedNotification();
    },
    createInput: createPipelineInput,
    deleteInput: deletePipelineInput,
    listInputs: getPipelineInputs,
    promoteInput: promotePipelineInput,
    updateInput: updatePipelineInput,
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

  setDashboardHooks({
    afterRender: () => {
      renderPublisherHealthModal();
      renderDashboardModes();
    },
  });

  setPipelineViewDependencies({
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
