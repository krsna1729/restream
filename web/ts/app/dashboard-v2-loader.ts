import type { ControlRoomCheckpointModel } from "../features/control-room-view-model.js";
import type { IncidentsCheckpointModel } from "../features/incidents-view-model.js";
import type { OverviewViewModel } from "../features/overview-view-model.js";
import type { PipelineInspectCheckpointModel } from "../features/pipeline-inspect-view-model.js";
import type { StatusCheckpointModel } from "../features/status-view-model.js";
import type { TelemetryCheckpointModel } from "../features/telemetry-view-model.js";
import type {
  PipelineOperateHeaderModel,
  PipelineOperateInputStatusModel,
  PipelineOperateSelectorModel,
  PipelineOutputOverviewModel,
} from "../features/pipeline-operate-view-model.js";

const DASHBOARD_V2_BUNDLE = "./dashboard-v2-entry.js";
const DASHBOARD_V2_CHECKPOINTS_BUNDLE = "./dashboard-v2-checkpoints-entry.js";
const DASHBOARD_UI_VERSION_STORAGE_KEY = "restream.dashboardUiVersion.v1";
const DASHBOARD_UI_VERSION_TOGGLE_ID = "dashboard-ui-v2-toggle";

export type DashboardUiVersion = "v1" | "v2";

interface DashboardUiVersionStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

interface DashboardUiVersionToggleOptions {
  readonly document?: Document;
  readonly history?: History;
  readonly location?: Location;
  readonly reload?: () => void;
  readonly search?: string;
  readonly storage?: DashboardUiVersionStorage | null;
}

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
  readonly diagnosePipeline: (pipelineId: string) => void;
  readonly editPipeline: (pipelineId: string) => void;
  readonly inspectPipeline: (pipelineId: string) => void;
  readonly toggleFileIngest: (pipelineId: string) => Promise<void>;
  readonly toggleRecording: (pipelineId: string) => Promise<void>;
}

export interface DashboardV2PipelineDetailsPlaceholder {
  readonly title: string;
  readonly message: string;
}

export interface DashboardV2PipelineInputStatusActions {
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
}

let dashboardV2Module: DashboardV2Module | null = null;
let dashboardV2ModulePromise: Promise<void> | null = null;
let dashboardV2CheckpointsModule: DashboardV2CheckpointsModule | null = null;
let dashboardV2CheckpointsModulePromise: Promise<void> | null = null;
let dashboardV2OverviewActive = false;
let dashboardV2PipelineActive = false;
let dashboardV2PipelineInspectActive = false;
let dashboardV2ControlRoomActive = false;
let dashboardV2IncidentsActive = false;
let dashboardV2TelemetryActive = false;
let dashboardV2StatusActive = false;
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
let latestPipelineInspectModel: PipelineInspectCheckpointModel | null | undefined;
let pipelineInspectActions: DashboardV2PipelineInspectActions | null = null;
let latestControlRoomModel: ControlRoomCheckpointModel | null | undefined;
let controlRoomActions: DashboardV2ControlRoomActions | null = null;
let latestIncidentsModel: IncidentsCheckpointModel | null | undefined;
let incidentsActions: DashboardV2IncidentsActions | null = null;
let latestTelemetryModel: TelemetryCheckpointModel | null | undefined;
let telemetryActions: DashboardV2TelemetryActions | null = null;
let latestStatusModel: StatusCheckpointModel | null | undefined;
let statusActions: DashboardV2StatusActions | null = null;

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
] as const;

function setContainerHidden(id: string, hidden: boolean): void {
  const element = document.getElementById(id);
  if (element) element.hidden = hidden;
}

function hideDashboardV2Overview(): void {
  setContainerHidden("dashboard-v2-root", true);
}

function hideDashboardV2Pipeline(): void {
  for (const id of DASHBOARD_V2_CONTAINER_IDS.slice(1, 5)) {
    setContainerHidden(id, true);
  }
}

function hideDashboardV2PipelineInspect(): void {
  setContainerHidden("dashboard-v2-pipeline-inspect-root", true);
}

function hideDashboardV2ControlRoom(): void {
  setContainerHidden("dashboard-v2-control-room-root", true);
}

function hideDashboardV2Incidents(): void {
  setContainerHidden("dashboard-v2-incidents-root", true);
}

function hideDashboardV2Telemetry(): void {
  setContainerHidden("dashboard-v2-telemetry-root", true);
}

function hideDashboardV2Status(): void {
  setContainerHidden("dashboard-v2-status-root", true);
}

function ensureDashboardV2Module(): void {
  if (
    dashboardV2Module ||
    dashboardV2ModulePromise ||
    !dashboardV2ExperimentEnabled()
  ) {
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
      console.error("Unable to start the dashboard v2 experiment", error);
    });
}

function ensureDashboardV2CheckpointsModule(): void {
  if (
    dashboardV2CheckpointsModule ||
    dashboardV2CheckpointsModulePromise ||
    !dashboardV2ExperimentEnabled()
  ) {
    return;
  }
  dashboardV2CheckpointsModulePromise = import(DASHBOARD_V2_CHECKPOINTS_BUNDLE)
    .then((module) => {
      dashboardV2CheckpointsModule = module as DashboardV2CheckpointsModule;
      renderLatestPipelineInspect();
      renderLatestControlRoom();
      renderLatestIncidents();
      renderLatestTelemetry();
      renderLatestStatus();
    })
    .catch((error: unknown) => {
      dashboardV2CheckpointsModulePromise = null;
      console.error("Unable to start the dashboard v2 checkpoints", error);
    });
}

function normalizedDashboardUiVersion(
  value: string | null,
): DashboardUiVersion | null {
  return value === "v1" || value === "v2" ? value : null;
}

function dashboardStorage(): DashboardUiVersionStorage | null {
  try {
    return window.localStorage;
  } catch (_err) {
    return null;
  }
}

function dashboardSearch(): string {
  const location = window.location;
  if (location.search) return location.search;
  try {
    return new URL(location.href).search;
  } catch (_err) {
    return "";
  }
}

function readDashboardUiVersionPreference(
  storage: DashboardUiVersionStorage | null = dashboardStorage(),
): DashboardUiVersion | null {
  if (!storage) return null;
  try {
    return normalizedDashboardUiVersion(
      storage.getItem(DASHBOARD_UI_VERSION_STORAGE_KEY),
    );
  } catch (_err) {
    return null;
  }
}

function writeDashboardUiVersionPreference(
  version: DashboardUiVersion,
  storage: DashboardUiVersionStorage | null = dashboardStorage(),
): void {
  if (!storage) return;
  try {
    storage.setItem(DASHBOARD_UI_VERSION_STORAGE_KEY, version);
  } catch (_err) {
    // UI version persistence is a convenience; URL opt-in should still work.
  }
}

export function resolveDashboardUiVersion(
  search = dashboardSearch(),
  storage: DashboardUiVersionStorage | null = dashboardStorage(),
): DashboardUiVersion {
  const params = new URLSearchParams(search);
  const urlUiVersion = params.get("ui");
  const urlVersion = normalizedDashboardUiVersion(urlUiVersion);
  if (urlVersion) {
    writeDashboardUiVersionPreference(urlVersion, storage);
    return urlVersion;
  }
  if (urlUiVersion !== null) return "v1";
  return readDashboardUiVersionPreference(storage) ?? "v1";
}

export function dashboardUiVersionUrl(
  href: string,
  version: DashboardUiVersion,
): string {
  const url = new URL(href, "http://localhost/");
  if (version === "v2") {
    url.searchParams.set("ui", "v2");
  } else {
    url.searchParams.delete("ui");
  }
  return url.toString();
}

export function initDashboardUiVersionToggle(
  options: DashboardUiVersionToggleOptions = {},
): void {
  const doc = options.document ?? document;
  const toggle = doc.getElementById(
    DASHBOARD_UI_VERSION_TOGGLE_ID,
  ) as HTMLInputElement | null;
  if (!toggle) return;

  const storage = options.storage ?? dashboardStorage();
  const currentVersion = resolveDashboardUiVersion(
    options.search ?? dashboardSearch(),
    storage,
  );
  toggle.checked = currentVersion === "v2";
  toggle.title = toggle.checked
    ? "Dashboard UI v2 is remembered for this browser"
    : "Dashboard UI v1 is remembered for this browser";

  if (toggle.dataset.dashboardUiVersionBound === "true") return;
  toggle.dataset.dashboardUiVersionBound = "true";
  toggle.addEventListener("change", () => {
    const nextVersion: DashboardUiVersion = toggle.checked ? "v2" : "v1";
    writeDashboardUiVersionPreference(nextVersion, storage);
    const location = options.location ?? window.location;
    const history = options.history ?? window.history;
    const nextUrl = dashboardUiVersionUrl(location.href, nextVersion);
    try {
      history.replaceState({}, "", nextUrl);
    } catch (_err) {
      location.href = nextUrl;
    }
    const reload = options.reload ?? (() => window.location.reload());
    reload();
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

function renderLatestPipelineInspect(): void {
  if (!dashboardV2PipelineInspectActive) {
    hideDashboardV2PipelineInspect();
    return;
  }
  ensureDashboardV2CheckpointsModule();
  if (
    !dashboardV2CheckpointsModule ||
    latestPipelineInspectModel === undefined ||
    !pipelineInspectActions
  )
    return;
  dashboardV2CheckpointsModule.renderDashboardV2PipelineInspectCheckpoint(
    latestPipelineInspectModel,
    pipelineInspectActions,
  );
}

function renderLatestControlRoom(): void {
  if (!dashboardV2ControlRoomActive) {
    hideDashboardV2ControlRoom();
    return;
  }
  ensureDashboardV2CheckpointsModule();
  if (
    !dashboardV2CheckpointsModule ||
    latestControlRoomModel === undefined ||
    !controlRoomActions
  )
    return;
  dashboardV2CheckpointsModule.renderDashboardV2ControlRoomCheckpoint(
    latestControlRoomModel,
    controlRoomActions,
  );
}

function renderLatestIncidents(): void {
  if (!dashboardV2IncidentsActive) {
    hideDashboardV2Incidents();
    return;
  }
  ensureDashboardV2CheckpointsModule();
  if (
    !dashboardV2CheckpointsModule ||
    latestIncidentsModel === undefined ||
    !incidentsActions
  )
    return;
  dashboardV2CheckpointsModule.renderDashboardV2IncidentsCheckpoint(
    latestIncidentsModel,
    incidentsActions,
  );
}

function renderLatestTelemetry(): void {
  if (!dashboardV2TelemetryActive) {
    hideDashboardV2Telemetry();
    return;
  }
  ensureDashboardV2CheckpointsModule();
  if (
    !dashboardV2CheckpointsModule ||
    latestTelemetryModel === undefined ||
    !telemetryActions
  )
    return;
  dashboardV2CheckpointsModule.renderDashboardV2TelemetryCheckpoint(
    latestTelemetryModel,
    telemetryActions,
  );
}

function renderLatestStatus(): void {
  if (!dashboardV2StatusActive) {
    hideDashboardV2Status();
    return;
  }
  ensureDashboardV2CheckpointsModule();
  if (
    !dashboardV2CheckpointsModule ||
    latestStatusModel === undefined ||
    !statusActions
  )
    return;
  dashboardV2CheckpointsModule.renderDashboardV2StatusCheckpoint(
    latestStatusModel,
    statusActions,
  );
}

export function dashboardV2ExperimentEnabled(
  search = dashboardSearch(),
  storage: DashboardUiVersionStorage | null = dashboardStorage(),
): boolean {
  return resolveDashboardUiVersion(search, storage) === "v2";
}

export async function startDashboardV2Experiment(): Promise<boolean> {
  if (!dashboardV2ExperimentEnabled()) return false;
  return true;
}

export function setDashboardV2PresentationScope(options: {
  readonly overviewActive: boolean;
  readonly pipelineActive: boolean;
  readonly pipelineInspectActive?: boolean;
  readonly controlRoomActive?: boolean;
  readonly incidentsActive?: boolean;
  readonly telemetryActive?: boolean;
  readonly statusActive?: boolean;
}): void {
  const nextOverviewActive =
    dashboardV2ExperimentEnabled() && options.overviewActive;
  const nextPipelineActive =
    dashboardV2ExperimentEnabled() && options.pipelineActive;
  const nextPipelineInspectActive =
    dashboardV2ExperimentEnabled() && Boolean(options.pipelineInspectActive);
  const nextControlRoomActive =
    dashboardV2ExperimentEnabled() && Boolean(options.controlRoomActive);
  const nextIncidentsActive =
    dashboardV2ExperimentEnabled() && Boolean(options.incidentsActive);
  const nextTelemetryActive =
    dashboardV2ExperimentEnabled() && Boolean(options.telemetryActive);
  const nextStatusActive =
    dashboardV2ExperimentEnabled() && Boolean(options.statusActive);
  const overviewChanged = dashboardV2OverviewActive !== nextOverviewActive;
  const pipelineChanged = dashboardV2PipelineActive !== nextPipelineActive;
  const pipelineInspectChanged =
    dashboardV2PipelineInspectActive !== nextPipelineInspectActive;
  const controlRoomChanged =
    dashboardV2ControlRoomActive !== nextControlRoomActive;
  const incidentsChanged = dashboardV2IncidentsActive !== nextIncidentsActive;
  const telemetryChanged = dashboardV2TelemetryActive !== nextTelemetryActive;
  const statusChanged = dashboardV2StatusActive !== nextStatusActive;
  dashboardV2OverviewActive = nextOverviewActive;
  dashboardV2PipelineActive = nextPipelineActive;
  dashboardV2PipelineInspectActive = nextPipelineInspectActive;
  dashboardV2ControlRoomActive = nextControlRoomActive;
  dashboardV2IncidentsActive = nextIncidentsActive;
  dashboardV2TelemetryActive = nextTelemetryActive;
  dashboardV2StatusActive = nextStatusActive;
  if (!dashboardV2OverviewActive) hideDashboardV2Overview();
  if (!dashboardV2PipelineActive) hideDashboardV2Pipeline();
  if (!dashboardV2PipelineInspectActive) hideDashboardV2PipelineInspect();
  if (!dashboardV2ControlRoomActive) hideDashboardV2ControlRoom();
  if (!dashboardV2IncidentsActive) hideDashboardV2Incidents();
  if (!dashboardV2TelemetryActive) hideDashboardV2Telemetry();
  if (!dashboardV2StatusActive) hideDashboardV2Status();
  if (overviewChanged && dashboardV2OverviewActive) renderLatestOverview();
  if (pipelineChanged && dashboardV2PipelineActive) {
    renderLatestPipelineSelector();
    renderLatestPipelineHeader();
    renderLatestPipelineInputStatus();
    renderLatestPipelineOutputOverview();
  }
  if (pipelineInspectChanged && dashboardV2PipelineInspectActive) {
    renderLatestPipelineInspect();
  }
  if (controlRoomChanged && dashboardV2ControlRoomActive) {
    renderLatestControlRoom();
  }
  if (incidentsChanged && dashboardV2IncidentsActive) {
    renderLatestIncidents();
  }
  if (telemetryChanged && dashboardV2TelemetryActive) {
    renderLatestTelemetry();
  }
  if (statusChanged && dashboardV2StatusActive) {
    renderLatestStatus();
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
  pipelineInspectActions = actions;
  renderLatestPipelineInspect();
}

export function updateDashboardV2PipelineInspectCheckpoint(
  model: PipelineInspectCheckpointModel | null,
): void {
  latestPipelineInspectModel = model;
  renderLatestPipelineInspect();
}

export function setDashboardV2ControlRoomActions(
  actions: DashboardV2ControlRoomActions,
): void {
  controlRoomActions = actions;
  renderLatestControlRoom();
}

export function updateDashboardV2ControlRoomCheckpoint(
  model: ControlRoomCheckpointModel | null,
): void {
  latestControlRoomModel = model;
  renderLatestControlRoom();
}

export function setDashboardV2IncidentsActions(
  actions: DashboardV2IncidentsActions,
): void {
  incidentsActions = actions;
  renderLatestIncidents();
}

export function updateDashboardV2IncidentsCheckpoint(
  model: IncidentsCheckpointModel | null,
): void {
  latestIncidentsModel = model;
  renderLatestIncidents();
}

export function setDashboardV2TelemetryActions(
  actions: DashboardV2TelemetryActions,
): void {
  telemetryActions = actions;
  renderLatestTelemetry();
}

export function updateDashboardV2TelemetryCheckpoint(
  model: TelemetryCheckpointModel | null,
): void {
  latestTelemetryModel = model;
  renderLatestTelemetry();
}

export function setDashboardV2StatusActions(
  actions: DashboardV2StatusActions,
): void {
  statusActions = actions;
  renderLatestStatus();
}

export function updateDashboardV2StatusCheckpoint(
  model: StatusCheckpointModel | null,
): void {
  latestStatusModel = model;
  renderLatestStatus();
}
