import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";

import type { ControlRoomCheckpointModel } from "../features/control-room-view-model.js";
import type { IncidentsCheckpointModel } from "../features/incidents-view-model.js";
import type { MediaCheckpointModel } from "../features/media-view-model.js";
import type { PipelineInspectCheckpointModel } from "../features/pipeline-inspect-view-model.js";
import type { SettingsCheckpointModel } from "../features/settings-view-model.js";
import type { StatusCheckpointModel } from "../features/status-view-model.js";
import type { TelemetryCheckpointModel } from "../features/telemetry-view-model.js";
import type {
  DashboardV2ControlRoomActions,
  DashboardV2IncidentsActions,
  DashboardV2MediaActions,
  DashboardV2PipelineInspectActions,
  DashboardV2SettingsActions,
  DashboardV2StatusActions,
  DashboardV2TelemetryActions,
} from "./dashboard-v2-loader.js";

interface DashboardV2CheckpointMetric {
  readonly label: string;
  readonly value: string;
}

type DashboardV2CheckpointAction = readonly [
  label: string,
  onClick: () => void,
  disabled?: boolean,
  title?: string,
];

interface DashboardV2CheckpointCardProps {
  readonly actions: readonly DashboardV2CheckpointAction[];
  readonly className?: string;
  readonly focusLabel: string;
  readonly focusTitle: string;
  readonly headingId: string;
  readonly metrics: readonly DashboardV2CheckpointMetric[];
  readonly nextStep: string;
  readonly primaryCards: readonly (readonly [string, string])[];
  readonly statusLabel: string;
  readonly statusTone: string;
  readonly summary: string;
  readonly title: string;
}

function toneBadgeClass(tone: string): string {
  return tone === "success"
    ? "badge-success"
    : tone === "warning"
      ? "badge-warning"
      : tone === "error"
        ? "badge-error"
        : "badge-ghost";
}

function DashboardV2CheckpointCard({
  actions,
  className = "",
  focusLabel,
  focusTitle,
  headingId,
  metrics,
  nextStep,
  primaryCards,
  statusLabel,
  statusTone,
  summary,
  title,
}: DashboardV2CheckpointCardProps): React.JSX.Element {
  return (
    <section
      aria-labelledby={headingId}
      className={`dashboard-section ${className}`}
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h2
              className="text-base-content text-lg font-semibold leading-tight"
              id={headingId}
            >
              {title}
            </h2>
            <span className={`badge badge-sm ${toneBadgeClass(statusTone)}`}>
              {statusLabel}
            </span>
          </div>
          <p
            className="text-base-content/65 mt-1 max-w-4xl text-sm"
            role="status"
            aria-live="polite"
          >
            {summary}
          </p>
        </div>
        <div className="flex shrink-0 flex-wrap gap-2">
          {actions.map(([label, onClick, disabled, title]) => (
            <button
              className="btn btn-xs btn-accent btn-outline"
              disabled={disabled}
              key={label}
              onClick={onClick}
              title={title}
              type="button"
            >
              {label}
            </button>
          ))}
        </div>
      </div>
      <div className="mt-3 grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
        {primaryCards.map(([label, value]) => (
          <div
            className="border-base-content/10 bg-base-100/60 rounded-lg border px-3 py-2"
            key={label}
          >
            <div className="text-base-content/55 text-[0.68rem] font-semibold uppercase tracking-wide">
              {label}
            </div>
            <div className="mt-0.5 truncate text-sm font-medium tabular-nums">
              {value}
            </div>
          </div>
        ))}
      </div>
      {metrics.length ? (
        <div className="text-base-content/60 mt-2 flex flex-wrap gap-x-3 gap-y-1 text-xs tabular-nums">
          {metrics.map((metric) => (
            <span key={metric.label}>
              {metric.label}: {metric.value}
            </span>
          ))}
        </div>
      ) : null}
      <div className="border-base-content/10 bg-base-100/50 mt-3 rounded-lg border px-3 py-2">
        <div className="text-base-content/55 text-[0.68rem] font-semibold uppercase tracking-wide">
          {focusTitle}
        </div>
        <p className="text-base-content/70 mt-1 text-sm">{focusLabel}</p>
        <p className="text-base-content/60 mt-1 text-xs">
          Next: {nextStep}
        </p>
      </div>
    </section>
  );
}

function DashboardV2PipelineInspectCheckpoint({
  actions,
  model,
}: {
  actions: DashboardV2PipelineInspectActions;
  model: PipelineInspectCheckpointModel;
}): React.JSX.Element {
  const canActOnPipeline = model.pipelineId !== null;
  return (
    <DashboardV2CheckpointCard
      actions={[
        [
          "Operate",
          () => {
            if (model.pipelineId) actions.openPipeline(model.pipelineId);
          },
          !model.canOpenPipeline || !canActOnPipeline,
        ],
        [
          "Diagnostics",
          () => {
            if (model.pipelineId) actions.runDiagnostics(model.pipelineId);
          },
          !model.canRunDiagnostics || !canActOnPipeline,
          model.diagnosticsDisabledReason,
        ],
      ]}
      className="border-info/25 bg-info/5"
      focusLabel={model.focusLabel}
      focusTitle="Inspection focus"
      headingId="dashboard-v2-pipeline-inspect-title"
      metrics={model.metrics}
      nextStep={model.nextStep}
      primaryCards={[
        ["Input", model.inputLabel],
        ["Outputs", model.outputLabel],
        ["Attention", model.attentionLabel],
        ["Graph", model.graphLabel],
      ]}
      statusLabel={model.statusLabel}
      statusTone={model.statusTone}
      summary={model.summary}
      title={model.title}
    />
  );
}

function DashboardV2ControlRoomCheckpoint({
  actions,
  model,
}: {
  actions: DashboardV2ControlRoomActions;
  model: ControlRoomCheckpointModel;
}): React.JSX.Element {
  const canActOnPipeline = model.pipelineId !== null;
  return (
    <DashboardV2CheckpointCard
      actions={[
        [
          "Operate",
          () => {
            if (model.pipelineId) actions.openPipeline(model.pipelineId);
          },
          !model.canOpenPipeline || !canActOnPipeline,
        ],
      ]}
      className="border-accent/25 bg-accent/5 mb-4"
      focusLabel={model.focusLabel}
      focusTitle="Monitor focus"
      headingId="dashboard-v2-control-room-title"
      metrics={model.metrics}
      nextStep={model.nextStep}
      primaryCards={[
        ["Monitor coverage", model.monitoredLabel],
        ["Missing URLs", model.missingLabel],
        ["Search", model.searchLabel],
        ["Preview loading", model.previewLabel],
      ]}
      statusLabel={model.statusLabel}
      statusTone={model.statusTone}
      summary={model.summary}
      title={model.title}
    />
  );
}

function DashboardV2IncidentsCheckpoint({
  actions,
  model,
}: {
  actions: DashboardV2IncidentsActions;
  model: IncidentsCheckpointModel;
}): React.JSX.Element {
  return (
    <DashboardV2CheckpointCard
      actions={[
        [
          "Telemetry",
          actions.openTelemetry,
          !model.canOpenTelemetry,
        ],
      ]}
      className="border-error/25 bg-error/5 mb-4"
      focusLabel={model.focusLabel}
      focusTitle="Incident focus"
      headingId="dashboard-v2-incidents-title"
      metrics={model.metrics}
      nextStep={model.nextStep}
      primaryCards={[
        ["Alert state", model.alertLabel],
        ["Events", model.eventLabel],
        ["Scope", model.scopeLabel],
        ["Search", model.searchLabel],
      ]}
      statusLabel={model.statusLabel}
      statusTone={model.statusTone}
      summary={model.summary}
      title={model.title}
    />
  );
}

function DashboardV2TelemetryCheckpoint({
  actions,
  model,
}: {
  actions: DashboardV2TelemetryActions;
  model: TelemetryCheckpointModel;
}): React.JSX.Element {
  return (
    <DashboardV2CheckpointCard
      actions={[
        [
          "Status",
          actions.openStatus,
          !model.canOpenStatus,
        ],
      ]}
      className="border-secondary/25 bg-secondary/5 mb-4"
      focusLabel={model.focusLabel}
      focusTitle="Telemetry focus"
      headingId="dashboard-v2-telemetry-title"
      metrics={model.metrics}
      nextStep={model.nextStep}
      primaryCards={[
        ["Pipeline", model.pipelineLabel],
        ["Counters", model.counterLabel],
        ["Egresses", model.egressLabel],
        ["Search", model.searchLabel],
      ]}
      statusLabel={model.statusLabel}
      statusTone={model.statusTone}
      summary={model.summary}
      title={model.title}
    />
  );
}

function DashboardV2StatusCheckpoint({
  actions,
  model,
}: {
  actions: DashboardV2StatusActions;
  model: StatusCheckpointModel;
}): React.JSX.Element {
  return (
    <DashboardV2CheckpointCard
      actions={[
        [
          "Telemetry",
          actions.openTelemetry,
          !model.canOpenTelemetry,
        ],
      ]}
      className="border-neutral/25 bg-base-200/50 mb-4"
      focusLabel={model.focusLabel}
      focusTitle="Status focus"
      headingId="dashboard-v2-status-title"
      metrics={model.metrics}
      nextStep={model.nextStep}
      primaryCards={[
        ["Build", model.buildLabel],
        ["Process logs", model.logLabel],
        ["Activity", model.activityLabel],
        ["Search", model.searchLabel],
      ]}
      statusLabel={model.statusLabel}
      statusTone={model.statusTone}
      summary={model.summary}
      title={model.title}
    />
  );
}

function DashboardV2MediaCheckpoint({
  actions,
  model,
}: {
  actions: DashboardV2MediaActions;
  model: MediaCheckpointModel;
}): React.JSX.Element {
  return (
    <DashboardV2CheckpointCard
      actions={[
        [
          "Overview",
          actions.openOverview,
          !model.canOpenOverview,
        ],
      ]}
      className="border-primary/25 bg-primary/5 mb-4"
      focusLabel={model.focusLabel}
      focusTitle="Media focus"
      headingId="dashboard-v2-media-title"
      metrics={model.metrics}
      nextStep={model.nextStep}
      primaryCards={[
        ["Recordings", model.recordingLabel],
        ["Source files", model.sourceLabel],
        ["Search", model.searchLabel],
        ["Storage", model.storageLabel],
      ]}
      statusLabel={model.statusLabel}
      statusTone={model.statusTone}
      summary={model.summary}
      title={model.title}
    />
  );
}

function DashboardV2SettingsCheckpoint({
  actions,
  model,
}: {
  actions: DashboardV2SettingsActions;
  model: SettingsCheckpointModel;
}): React.JSX.Element {
  return (
    <DashboardV2CheckpointCard
      actions={[
        [
          "Status",
          actions.openStatus,
          !model.canOpenStatus,
        ],
      ]}
      className="border-warning/25 bg-warning/5 mb-4"
      focusLabel={model.focusLabel}
      focusTitle="Settings focus"
      headingId="dashboard-v2-settings-title"
      metrics={model.metrics}
      nextStep={model.nextStep}
      primaryCards={[
        ["Sections", model.sectionLabel],
        ["Profiles", model.profileLabel],
        ["Authentication", model.authLabel],
        ["Search", model.searchLabel],
      ]}
      statusLabel={model.statusLabel}
      statusTone={model.statusTone}
      summary={model.summary}
      title={model.title}
    />
  );
}

const pipelineInspectContainer = document.getElementById(
  "dashboard-v2-pipeline-inspect-root",
);
if (!pipelineInspectContainer) {
  throw new Error("Dashboard v2 pipeline inspect root is missing");
}
const inspectContainer: HTMLElement = pipelineInspectContainer;
let inspectRoot: Root | null = null;

const controlRoomContainer = document.getElementById(
  "dashboard-v2-control-room-root",
);
if (!controlRoomContainer) {
  throw new Error("Dashboard v2 control room root is missing");
}
const controlRoomRootContainer: HTMLElement = controlRoomContainer;
let controlRoomRoot: Root | null = null;

const incidentsContainer = document.getElementById(
  "dashboard-v2-incidents-root",
);
if (!incidentsContainer) {
  throw new Error("Dashboard v2 incidents root is missing");
}
const incidentsRootContainer: HTMLElement = incidentsContainer;
let incidentsRoot: Root | null = null;

const telemetryContainer = document.getElementById(
  "dashboard-v2-telemetry-root",
);
if (!telemetryContainer) {
  throw new Error("Dashboard v2 telemetry root is missing");
}
const telemetryRootContainer: HTMLElement = telemetryContainer;
let telemetryRoot: Root | null = null;

const statusContainer = document.getElementById("dashboard-v2-status-root");
if (!statusContainer) {
  throw new Error("Dashboard v2 status root is missing");
}
const statusRootContainer: HTMLElement = statusContainer;
let statusRoot: Root | null = null;

const mediaContainer = document.getElementById("dashboard-v2-media-root");
if (!mediaContainer) {
  throw new Error("Dashboard v2 media root is missing");
}
const mediaRootContainer: HTMLElement = mediaContainer;
let mediaRoot: Root | null = null;

const settingsContainer = document.getElementById("dashboard-v2-settings-root");
if (!settingsContainer) {
  throw new Error("Dashboard v2 settings root is missing");
}
const settingsRootContainer: HTMLElement = settingsContainer;
let settingsRoot: Root | null = null;

export function renderDashboardV2PipelineInspectCheckpoint(
  model: PipelineInspectCheckpointModel | null,
  actions: DashboardV2PipelineInspectActions,
): void {
  inspectRoot ??= createRoot(inspectContainer);
  inspectContainer.hidden = model === null;
  inspectRoot.render(
    model ? (
      <DashboardV2PipelineInspectCheckpoint actions={actions} model={model} />
    ) : null,
  );
}

export function renderDashboardV2IncidentsCheckpoint(
  model: IncidentsCheckpointModel | null,
  actions: DashboardV2IncidentsActions,
): void {
  incidentsRoot ??= createRoot(incidentsRootContainer);
  incidentsRootContainer.hidden = model === null;
  incidentsRoot.render(
    model ? (
      <DashboardV2IncidentsCheckpoint actions={actions} model={model} />
    ) : null,
  );
}

export function renderDashboardV2TelemetryCheckpoint(
  model: TelemetryCheckpointModel | null,
  actions: DashboardV2TelemetryActions,
): void {
  telemetryRoot ??= createRoot(telemetryRootContainer);
  telemetryRootContainer.hidden = model === null;
  telemetryRoot.render(
    model ? (
      <DashboardV2TelemetryCheckpoint actions={actions} model={model} />
    ) : null,
  );
}

export function renderDashboardV2StatusCheckpoint(
  model: StatusCheckpointModel | null,
  actions: DashboardV2StatusActions,
): void {
  statusRoot ??= createRoot(statusRootContainer);
  statusRootContainer.hidden = model === null;
  statusRoot.render(
    model ? <DashboardV2StatusCheckpoint actions={actions} model={model} /> : null,
  );
}

export function renderDashboardV2MediaCheckpoint(
  model: MediaCheckpointModel | null,
  actions: DashboardV2MediaActions,
): void {
  mediaRoot ??= createRoot(mediaRootContainer);
  mediaRootContainer.hidden = model === null;
  mediaRoot.render(
    model ? <DashboardV2MediaCheckpoint actions={actions} model={model} /> : null,
  );
}

export function renderDashboardV2SettingsCheckpoint(
  model: SettingsCheckpointModel | null,
  actions: DashboardV2SettingsActions,
): void {
  settingsRoot ??= createRoot(settingsRootContainer);
  settingsRootContainer.hidden = model === null;
  settingsRoot.render(
    model ? (
      <DashboardV2SettingsCheckpoint actions={actions} model={model} />
    ) : null,
  );
}

export function renderDashboardV2ControlRoomCheckpoint(
  model: ControlRoomCheckpointModel | null,
  actions: DashboardV2ControlRoomActions,
): void {
  controlRoomRoot ??= createRoot(controlRoomRootContainer);
  controlRoomRootContainer.hidden = model === null;
  controlRoomRoot.render(
    model ? (
      <DashboardV2ControlRoomCheckpoint actions={actions} model={model} />
    ) : null,
  );
}
