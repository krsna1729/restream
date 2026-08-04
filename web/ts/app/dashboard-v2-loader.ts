import type { ControlRoomCheckpointModel } from "../features/control-room/view-model.js";
import type { IncidentsCheckpointModel } from "../features/incidents-view-model.js";
import type { MediaCheckpointModel } from "../features/media-view-model.js";
import type { OverviewViewModel } from "../features/overview-view-model.js";
import type { PipelineInspectCheckpointModel } from "../features/pipeline-inspect-view-model.js";
import type { SettingsCheckpointModel } from "../features/settings-view-model.js";
import type { StatusCheckpointModel } from "../features/status-view-model.js";
import type { TelemetryCheckpointModel } from "../features/telemetry-view-model.js";
import type {
  PipelineOperateHeaderModel,
  PipelineOperateInputStatusModel,
  PipelineOperateSelectorModel,
  PipelineOutputOverviewModel,
} from "../features/pipeline-operate-view-model.js";
import type { PipelineInputsPanelActions } from "../features/pipeline-inputs-contract.js";

const DASHBOARD_V2_BUNDLE = "./dashboard-v2-entry.js";
const DASHBOARD_V2_CHECKPOINTS_BUNDLE = "./dashboard-v2-checkpoints-entry.js";

export interface DashboardV2OverviewActions {
  readonly addPipeline: () => void;
  readonly inspectPipeline: (pipelineId: string) => void;
  readonly openPipeline: (pipelineId: string) => void;
  readonly openStatus: () => void;
}

export interface DashboardV2PipelineSelectorActions {
  readonly addPipeline: () => void;
  readonly selectPipeline: (pipelineId: string) => void;
}

export interface DashboardV2PipelineHeaderActions {
  readonly addPipeline: () => void;
  readonly deletePipeline: (pipelineId: string) => void;
  readonly diagnosePipeline: (pipelineId: string) => void;
  readonly editPipeline: (pipelineId: string) => void;
  readonly inspectPipeline: (pipelineId: string) => void;
  readonly openHistory: (pipelineId: string, pipelineName: string) => void;
  readonly toggleFileIngest: (pipelineId: string) => Promise<void>;
  readonly toggleRecording: (pipelineId: string) => Promise<void>;
}

export interface DashboardV2PipelineDetailsPlaceholder {
  readonly actionLabel?: string;
  readonly title: string;
  readonly message: string;
}

export interface DashboardV2PipelineInputStatusActions
  extends PipelineInputsPanelActions {
  readonly cancelAudioTrackEdit: (pipelineId: string, key: string) => void;
  readonly clearPreview: (container: HTMLElement) => void;
  readonly copyIngestUrl: (
    pipelineId: string,
    protocol: "rtmp" | "srt",
  ) => Promise<void>;
  readonly copyStreamKey: (pipelineId: string) => Promise<void>;
  readonly editAudioTrack: (pipelineId: string, key: string) => void;
  readonly mountPreview: (
    pipelineId: string,
    container: HTMLElement,
  ) => void;
  readonly saveAudioTrack: (pipelineId: string, key: string) => void;
  readonly selectProtocol: (
    pipelineId: string,
    protocol: "rtmp" | "srt",
  ) => void;
  readonly updateAudioTrackDraft: (
    pipelineId: string,
    key: string,
    value: string,
  ) => void;
}

export interface DashboardV2PipelineOutputOverviewActions {
  readonly addOutput: (pipelineId: string) => void;
  readonly deleteOutput: (pipelineId: string, outputId: string) => Promise<void>;
  readonly editOutput: (pipelineId: string, outputId: string) => void;
  readonly monitorOutput: (pipelineId: string, outputId: string) => void;
  readonly openOutputHistory: (
    pipelineId: string,
    outputId: string,
    outputName: string,
  ) => void;
  readonly toggleOutput: (pipelineId: string, outputId: string) => Promise<void>;
  readonly toggleOutputList: (pipelineId: string) => void;
}

export interface DashboardV2PipelineInspectActions {
  readonly openPipeline: (pipelineId: string) => void;
  readonly runDiagnostics: (pipelineId: string) => void;
}

export interface DashboardV2ControlRoomActions {
  readonly openPipeline: (pipelineId: string) => void;
}

export interface DashboardV2IncidentsActions {
  readonly openTelemetry: () => void;
}

export interface DashboardV2TelemetryActions {
  readonly openStatus: () => void;
}

export interface DashboardV2StatusActions {
  readonly openTelemetry: () => void;
}

export interface DashboardV2MediaActions {
  readonly openOverview: () => void;
}

export interface DashboardV2SettingsActions {
  readonly openStatus: () => void;
}

interface DashboardV2Module {
  renderDashboardV2Overview(
    model: OverviewViewModel,
    actions: DashboardV2OverviewActions,
  ): void;
  renderDashboardV2PipelineSelector(
    model: PipelineOperateSelectorModel,
    actions: DashboardV2PipelineSelectorActions,
  ): void;
  renderDashboardV2PipelineHeader(
    model: PipelineOperateHeaderModel | null,
    actions: DashboardV2PipelineHeaderActions,
    placeholder?: DashboardV2PipelineDetailsPlaceholder | null,
  ): void;
  renderDashboardV2PipelineInputStatus(
    model: PipelineOperateInputStatusModel | null,
    actions: DashboardV2PipelineInputStatusActions,
  ): void;
  renderDashboardV2PipelineOutputOverview(
    model: PipelineOutputOverviewModel | null,
    actions: DashboardV2PipelineOutputOverviewActions,
  ): void;
  clearDashboardV2PipelineOperate(): void;
}

interface DashboardV2CheckpointsModule {
  renderDashboardV2PipelineInspectCheckpoint(
    model: PipelineInspectCheckpointModel | null,
    actions: DashboardV2PipelineInspectActions,
  ): void;
  renderDashboardV2ControlRoomCheckpoint(
    model: ControlRoomCheckpointModel | null,
    actions: DashboardV2ControlRoomActions,
  ): void;
  renderDashboardV2IncidentsCheckpoint(
    model: IncidentsCheckpointModel | null,
    actions: DashboardV2IncidentsActions,
  ): void;
  renderDashboardV2TelemetryCheckpoint(
    model: TelemetryCheckpointModel | null,
    actions: DashboardV2TelemetryActions,
  ): void;
  renderDashboardV2StatusCheckpoint(
    model: StatusCheckpointModel | null,
    actions: DashboardV2StatusActions,
  ): void;
  renderDashboardV2MediaCheckpoint(
    model: MediaCheckpointModel | null,
    actions: DashboardV2MediaActions,
  ): void;
  renderDashboardV2SettingsCheckpoint(
    model: SettingsCheckpointModel | null,
    actions: DashboardV2SettingsActions,
  ): void;
}

let dashboardV2Module: DashboardV2Module | null = null;
let dashboardV2ModulePromise: Promise<void> | null = null;
let dashboardV2CheckpointsModule: DashboardV2CheckpointsModule | null = null;
let dashboardV2CheckpointsModulePromise: Promise<void> | null = null;
let dashboardV2OverviewActive = false;
let dashboardV2PipelineActive = false;
let latestOverviewModel: OverviewViewModel | null = null;
let overviewActions: DashboardV2OverviewActions | null = null;
let latestPipelineSelectorModel: PipelineOperateSelectorModel | null = null;
let pipelineSelectorActions: DashboardV2PipelineSelectorActions | null = null;
let latestPipelineHeaderModel: PipelineOperateHeaderModel | null | undefined;
let pipelineHeaderActions: DashboardV2PipelineHeaderActions | null = null;
let latestPipelineInputStatusModel:
  PipelineOperateInputStatusModel | null | undefined;
let pipelineInputStatusActions: DashboardV2PipelineInputStatusActions | null =
  null;
let latestPipelineOutputOverviewModel:
  PipelineOutputOverviewModel | null | undefined;
let pipelineOutputOverviewActions:
  | DashboardV2PipelineOutputOverviewActions
  | null = null;

const DASHBOARD_V2_CONTAINER_IDS = [
  "dashboard-v2-root",
  "dashboard-v2-pipeline-selector-root",
  "dashboard-v2-pipeline-header-root",
  "dashboard-v2-pipeline-input-status-root",
  "dashboard-v2-pipeline-output-overview-root",
  "dashboard-v2-pipeline-inspect-root",
  "dashboard-v2-control-room-root",
  "dashboard-v2-incidents-root",
  "dashboard-v2-telemetry-root",
  "dashboard-v2-status-root",
  "dashboard-v2-media-root",
  "dashboard-v2-settings-root",
] as const;

function dashboardV2BundleLoadError(message: string, error: unknown): Error {
  if (error instanceof Error) {
    return new Error(`${message}: ${error.message}`);
  }
  return new Error(`${message}: ${String(error)}`);
}

function setContainerHidden(id: string, hidden: boolean): void {
  const element = document.getElementById(id);
  if (element) element.hidden = hidden;
}

function hideDashboardV2Overview(): void {
  setContainerHidden("dashboard-v2-root", true);
}

function hideDashboardV2Pipeline(): void {
  dashboardV2Module?.clearDashboardV2PipelineOperate();
  for (const id of DASHBOARD_V2_CONTAINER_IDS.slice(1, 5)) {
    setContainerHidden(id, true);
  }
}

function ensureDashboardV2Module(): void {
  if (dashboardV2Module || dashboardV2ModulePromise) {
    return;
  }
  dashboardV2ModulePromise = import(DASHBOARD_V2_BUNDLE)
    .then((module) => {
      dashboardV2Module = module as DashboardV2Module;
      renderLatestOverview();
      renderLatestPipelineSelector();
      renderLatestPipelineHeader();
      renderLatestPipelineInputStatus();
      renderLatestPipelineOutputOverview();
    })
    .catch((error: unknown) => {
      dashboardV2ModulePromise = null;
      throw dashboardV2BundleLoadError(
        "Unable to start the dashboard v2 shell",
        error,
      );
    });
}

// The seven checkpoints rendered by DASHBOARD_V2_CHECKPOINTS_BUNDLE
// (pipeline inspect, control room, incidents, telemetry, status, media,
// settings) all follow the same lifecycle: an independent active flag, a
// latest model, latest actions, and a render call gated on the checkpoints
// module + model + actions all being ready. This factory owns that shared
// shape so each checkpoint only supplies its container id and render call.
interface DashboardV2CheckpointSlot<TModel, TActions> {
  readonly setActions: (actions: TActions) => void;
  readonly updateModel: (model: TModel | null) => void;
  readonly renderLatest: () => void;
  readonly hide: () => void;
  readonly isActive: () => boolean;
  readonly setActive: (active: boolean) => boolean;
}

function createDashboardV2CheckpointSlot<TModel, TActions>(options: {
  readonly containerId: string;
  readonly render: (
    module: DashboardV2CheckpointsModule,
    model: TModel | null,
    actions: TActions,
  ) => void;
}): DashboardV2CheckpointSlot<TModel, TActions> {
  let active = false;
  let latestModel: TModel | null | undefined;
  let actions: TActions | null = null;

  function hide(): void {
    if (dashboardV2CheckpointsModule && actions) {
      options.render(dashboardV2CheckpointsModule, null, actions);
    }
    setContainerHidden(options.containerId, true);
  }

  function renderLatest(): void {
    if (!active) {
      hide();
      return;
    }
    ensureDashboardV2CheckpointsModule();
    if (!dashboardV2CheckpointsModule || latestModel === undefined || !actions)
      return;
    options.render(dashboardV2CheckpointsModule, latestModel, actions);
  }

  return {
    setActions(next) {
      actions = next;
      renderLatest();
    },
    updateModel(next) {
      latestModel = next;
      renderLatest();
    },
    renderLatest,
    hide,
    isActive: () => active,
    setActive(next) {
      const changed = active !== next;
      active = next;
      return changed;
    },
  };
}

const pipelineInspectSlot = createDashboardV2CheckpointSlot<
  PipelineInspectCheckpointModel,
  DashboardV2PipelineInspectActions
>({
  containerId: "dashboard-v2-pipeline-inspect-root",
  render: (module, model, actions) =>
    module.renderDashboardV2PipelineInspectCheckpoint(model, actions),
});

const controlRoomSlot = createDashboardV2CheckpointSlot<
  ControlRoomCheckpointModel,
  DashboardV2ControlRoomActions
>({
  containerId: "dashboard-v2-control-room-root",
  render: (module, model, actions) =>
    module.renderDashboardV2ControlRoomCheckpoint(model, actions),
});

const incidentsSlot = createDashboardV2CheckpointSlot<
  IncidentsCheckpointModel,
  DashboardV2IncidentsActions
>({
  containerId: "dashboard-v2-incidents-root",
  render: (module, model, actions) =>
    module.renderDashboardV2IncidentsCheckpoint(model, actions),
});

const telemetrySlot = createDashboardV2CheckpointSlot<
  TelemetryCheckpointModel,
  DashboardV2TelemetryActions
>({
  containerId: "dashboard-v2-telemetry-root",
  render: (module, model, actions) =>
    module.renderDashboardV2TelemetryCheckpoint(model, actions),
});

const statusSlot = createDashboardV2CheckpointSlot<
  StatusCheckpointModel,
  DashboardV2StatusActions
>({
  containerId: "dashboard-v2-status-root",
  render: (module, model, actions) =>
    module.renderDashboardV2StatusCheckpoint(model, actions),
});

const mediaSlot = createDashboardV2CheckpointSlot<
  MediaCheckpointModel,
  DashboardV2MediaActions
>({
  containerId: "dashboard-v2-media-root",
  render: (module, model, actions) =>
    module.renderDashboardV2MediaCheckpoint(model, actions),
});

const settingsSlot = createDashboardV2CheckpointSlot<
  SettingsCheckpointModel,
  DashboardV2SettingsActions
>({
  containerId: "dashboard-v2-settings-root",
  render: (module, model, actions) =>
    module.renderDashboardV2SettingsCheckpoint(model, actions),
});

function ensureDashboardV2CheckpointsModule(): void {
  if (dashboardV2CheckpointsModule || dashboardV2CheckpointsModulePromise) {
    return;
  }
  dashboardV2CheckpointsModulePromise = import(DASHBOARD_V2_CHECKPOINTS_BUNDLE)
    .then((module) => {
      dashboardV2CheckpointsModule = module as DashboardV2CheckpointsModule;
      pipelineInspectSlot.renderLatest();
      controlRoomSlot.renderLatest();
      incidentsSlot.renderLatest();
      telemetrySlot.renderLatest();
      statusSlot.renderLatest();
      mediaSlot.renderLatest();
      settingsSlot.renderLatest();
      document.dispatchEvent(new CustomEvent("dashboard:v2-checkpoints-ready"));
    })
    .catch((error: unknown) => {
      dashboardV2CheckpointsModulePromise = null;
      throw dashboardV2BundleLoadError(
        "Unable to start the dashboard v2 checkpoints",
        error,
      );
    });
}

function renderLatestOverview(): void {
  if (!dashboardV2OverviewActive) {
    hideDashboardV2Overview();
    return;
  }
  ensureDashboardV2Module();
  if (!dashboardV2Module || !latestOverviewModel || !overviewActions) return;
  dashboardV2Module.renderDashboardV2Overview(
    latestOverviewModel,
    overviewActions,
  );
}

function renderLatestPipelineSelector(): void {
  if (!dashboardV2PipelineActive) {
    hideDashboardV2Pipeline();
    return;
  }
  ensureDashboardV2Module();
  if (
    !dashboardV2Module ||
    !latestPipelineSelectorModel ||
    !pipelineSelectorActions
  ) {
    return;
  }
  dashboardV2Module.renderDashboardV2PipelineSelector(
    latestPipelineSelectorModel,
    pipelineSelectorActions,
  );
}

function renderLatestPipelineHeader(): void {
  if (!dashboardV2PipelineActive) {
    hideDashboardV2Pipeline();
    return;
  }
  ensureDashboardV2Module();
  if (
    !dashboardV2Module ||
    latestPipelineHeaderModel === undefined ||
    !pipelineHeaderActions
  ) {
    return;
  }
  const placeholder =
    latestPipelineHeaderModel === null ? pipelineDetailsPlaceholder() : null;
  dashboardV2Module.renderDashboardV2PipelineHeader(
    latestPipelineHeaderModel,
    pipelineHeaderActions,
    placeholder,
  );
}

function pipelineDetailsPlaceholder(): DashboardV2PipelineDetailsPlaceholder {
  const selector = latestPipelineSelectorModel;
  if (!selector || selector.pipelines.length === 0) {
    return {
      actionLabel: "Add Pipeline",
      title: "No pipelines configured",
      message: "Create a pipeline to start configuring ingest and outputs.",
    };
  }
  if (selector.selectedPipelineId === null) {
    return {
      title: "Select a pipeline",
      message:
        "Pipeline details, ingest preview, outputs, and controls appear here.",
    };
  }
  return {
    title: "Loading pipeline details",
    message:
      "The selected pipeline is catching up with the latest runtime snapshot.",
  };
}

function renderLatestPipelineInputStatus(): void {
  if (!dashboardV2PipelineActive) {
    hideDashboardV2Pipeline();
    return;
  }
  ensureDashboardV2Module();
  if (
    !dashboardV2Module ||
    latestPipelineInputStatusModel === undefined ||
    !pipelineInputStatusActions
  )
    return;
  dashboardV2Module.renderDashboardV2PipelineInputStatus(
    latestPipelineInputStatusModel,
    pipelineInputStatusActions,
  );
}

function renderLatestPipelineOutputOverview(): void {
  if (!dashboardV2PipelineActive) {
    hideDashboardV2Pipeline();
    return;
  }
  ensureDashboardV2Module();
  if (
    !dashboardV2Module ||
    latestPipelineOutputOverviewModel === undefined ||
    !pipelineOutputOverviewActions
  )
    return;
  dashboardV2Module.renderDashboardV2PipelineOutputOverview(
    latestPipelineOutputOverviewModel,
    pipelineOutputOverviewActions,
  );
}

export function setDashboardV2PresentationScope(options: {
  readonly overviewActive: boolean;
  readonly pipelineActive: boolean;
  readonly pipelineInspectActive?: boolean;
  readonly controlRoomActive?: boolean;
  readonly incidentsActive?: boolean;
  readonly telemetryActive?: boolean;
  readonly statusActive?: boolean;
  readonly mediaActive?: boolean;
  readonly settingsActive?: boolean;
}): void {
  const nextOverviewActive = options.overviewActive;
  const nextPipelineActive = options.pipelineActive;
  const overviewChanged = dashboardV2OverviewActive !== nextOverviewActive;
  const pipelineChanged = dashboardV2PipelineActive !== nextPipelineActive;
  dashboardV2OverviewActive = nextOverviewActive;
  dashboardV2PipelineActive = nextPipelineActive;
  const pipelineInspectChanged = pipelineInspectSlot.setActive(
    Boolean(options.pipelineInspectActive),
  );
  const controlRoomChanged = controlRoomSlot.setActive(
    Boolean(options.controlRoomActive),
  );
  const incidentsChanged = incidentsSlot.setActive(
    Boolean(options.incidentsActive),
  );
  const telemetryChanged = telemetrySlot.setActive(
    Boolean(options.telemetryActive),
  );
  const statusChanged = statusSlot.setActive(Boolean(options.statusActive));
  const mediaChanged = mediaSlot.setActive(Boolean(options.mediaActive));
  const settingsChanged = settingsSlot.setActive(
    Boolean(options.settingsActive),
  );
  if (!dashboardV2OverviewActive) hideDashboardV2Overview();
  if (!dashboardV2PipelineActive) hideDashboardV2Pipeline();
  if (!pipelineInspectSlot.isActive()) pipelineInspectSlot.hide();
  if (!controlRoomSlot.isActive()) controlRoomSlot.hide();
  if (!incidentsSlot.isActive()) incidentsSlot.hide();
  if (!telemetrySlot.isActive()) telemetrySlot.hide();
  if (!statusSlot.isActive()) statusSlot.hide();
  if (!mediaSlot.isActive()) mediaSlot.hide();
  if (!settingsSlot.isActive()) settingsSlot.hide();
  if (overviewChanged && dashboardV2OverviewActive) renderLatestOverview();
  if (pipelineChanged && dashboardV2PipelineActive) {
    renderLatestPipelineSelector();
    renderLatestPipelineHeader();
    renderLatestPipelineInputStatus();
    renderLatestPipelineOutputOverview();
  }
  if (pipelineInspectChanged && pipelineInspectSlot.isActive()) {
    pipelineInspectSlot.renderLatest();
  }
  if (controlRoomChanged && controlRoomSlot.isActive()) {
    controlRoomSlot.renderLatest();
  }
  if (incidentsChanged && incidentsSlot.isActive()) {
    incidentsSlot.renderLatest();
  }
  if (telemetryChanged && telemetrySlot.isActive()) {
    telemetrySlot.renderLatest();
  }
  if (statusChanged && statusSlot.isActive()) {
    statusSlot.renderLatest();
  }
  if (mediaChanged && mediaSlot.isActive()) {
    mediaSlot.renderLatest();
  }
  if (settingsChanged && settingsSlot.isActive()) {
    settingsSlot.renderLatest();
  }
}

export function setDashboardV2OverviewActions(
  actions: DashboardV2OverviewActions,
): void {
  overviewActions = actions;
  renderLatestOverview();
}

export function updateDashboardV2Overview(model: OverviewViewModel): void {
  latestOverviewModel = model;
  renderLatestOverview();
}

export function setDashboardV2PipelineSelectorActions(
  actions: DashboardV2PipelineSelectorActions,
): void {
  pipelineSelectorActions = actions;
  renderLatestPipelineSelector();
}

export function updateDashboardV2PipelineSelector(
  model: PipelineOperateSelectorModel,
): void {
  latestPipelineSelectorModel = model;
  renderLatestPipelineSelector();
}

export function setDashboardV2PipelineHeaderActions(
  actions: DashboardV2PipelineHeaderActions,
): void {
  pipelineHeaderActions = actions;
  renderLatestPipelineHeader();
}

export function updateDashboardV2PipelineHeader(
  model: PipelineOperateHeaderModel | null,
): void {
  latestPipelineHeaderModel = model;
  renderLatestPipelineHeader();
}

export function updateDashboardV2PipelineInputStatus(
  model: PipelineOperateInputStatusModel | null,
): void {
  latestPipelineInputStatusModel = model;
  renderLatestPipelineInputStatus();
}

export function setDashboardV2PipelineInputStatusActions(
  actions: DashboardV2PipelineInputStatusActions,
): void {
  pipelineInputStatusActions = actions;
  renderLatestPipelineInputStatus();
}

export function setDashboardV2PipelineOutputOverviewActions(
  actions: DashboardV2PipelineOutputOverviewActions,
): void {
  pipelineOutputOverviewActions = actions;
  renderLatestPipelineOutputOverview();
}

export function updateDashboardV2PipelineOutputOverview(
  model: PipelineOutputOverviewModel | null,
): void {
  latestPipelineOutputOverviewModel = model;
  renderLatestPipelineOutputOverview();
}

export function setDashboardV2PipelineInspectActions(
  actions: DashboardV2PipelineInspectActions,
): void {
  pipelineInspectSlot.setActions(actions);
}

export function updateDashboardV2PipelineInspectCheckpoint(
  model: PipelineInspectCheckpointModel | null,
): void {
  pipelineInspectSlot.updateModel(model);
}

export function setDashboardV2ControlRoomActions(
  actions: DashboardV2ControlRoomActions,
): void {
  controlRoomSlot.setActions(actions);
}

export function updateDashboardV2ControlRoomCheckpoint(
  model: ControlRoomCheckpointModel | null,
): void {
  controlRoomSlot.updateModel(model);
}

export function setDashboardV2IncidentsActions(
  actions: DashboardV2IncidentsActions,
): void {
  incidentsSlot.setActions(actions);
}

export function updateDashboardV2IncidentsCheckpoint(
  model: IncidentsCheckpointModel | null,
): void {
  incidentsSlot.updateModel(model);
}

export function setDashboardV2TelemetryActions(
  actions: DashboardV2TelemetryActions,
): void {
  telemetrySlot.setActions(actions);
}

export function updateDashboardV2TelemetryCheckpoint(
  model: TelemetryCheckpointModel | null,
): void {
  telemetrySlot.updateModel(model);
}

export function setDashboardV2StatusActions(
  actions: DashboardV2StatusActions,
): void {
  statusSlot.setActions(actions);
}

export function updateDashboardV2StatusCheckpoint(
  model: StatusCheckpointModel | null,
): void {
  statusSlot.updateModel(model);
}

export function setDashboardV2MediaActions(actions: DashboardV2MediaActions): void {
  mediaSlot.setActions(actions);
}

export function updateDashboardV2MediaCheckpoint(
  model: MediaCheckpointModel | null,
): void {
  mediaSlot.updateModel(model);
}

export function setDashboardV2SettingsActions(
  actions: DashboardV2SettingsActions,
): void {
  settingsSlot.setActions(actions);
}

export function updateDashboardV2SettingsCheckpoint(
  model: SettingsCheckpointModel | null,
): void {
  settingsSlot.updateModel(model);
}
