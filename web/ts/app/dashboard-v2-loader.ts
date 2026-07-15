import type { OverviewViewModel } from "../features/overview-view-model.js";
import type {
  PipelineOperateHeaderModel,
  PipelineOperateInputStatusModel,
  PipelineOperateSelectorModel,
  PipelineOutputOverviewModel,
} from "../features/pipeline-operate-view-model.js";

const DASHBOARD_V2_BUNDLE = "./dashboard-v2-entry.js";

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

let dashboardV2Module: DashboardV2Module | null = null;
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

function renderLatestOverview(): void {
  if (!dashboardV2Module || !latestOverviewModel || !overviewActions) return;
  dashboardV2Module.renderDashboardV2Overview(
    latestOverviewModel,
    overviewActions,
  );
}

function renderLatestPipelineSelector(): void {
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
  if (
    !dashboardV2Module ||
    latestPipelineHeaderModel === undefined ||
    !pipelineHeaderActions
  ) {
    return;
  }
  dashboardV2Module.renderDashboardV2PipelineHeader(
    latestPipelineHeaderModel,
    pipelineHeaderActions,
  );
}

function renderLatestPipelineInputStatus(): void {
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

export function dashboardV2ExperimentEnabled(
  search = window.location.search,
): boolean {
  return new URLSearchParams(search).get("ui") === "v2";
}

export async function startDashboardV2Experiment(): Promise<boolean> {
  if (!dashboardV2ExperimentEnabled()) return false;
  dashboardV2Module = (await import(DASHBOARD_V2_BUNDLE)) as DashboardV2Module;
  renderLatestOverview();
  renderLatestPipelineSelector();
  renderLatestPipelineHeader();
  renderLatestPipelineInputStatus();
  renderLatestPipelineOutputOverview();
  return true;
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
