import {
  canonicalizeDashboardLocation,
  dashboardModeUrl,
  pipelineWorkspaceUrl,
  resolveDashboardLocation,
  type DashboardLocation,
  type DashboardMode,
  type PipelineWorkspaceView,
} from "../../core/pipeline-workspace.js";
import { state } from "../../core/state.js";
import {
  resetPipelineInspectorSelection,
  syncPipelineInspectorVisibility,
} from "../../features/pipeline-inspector/index.js";
import {
  renderDashboardV2StatusBody,
  setStatusStreamActive,
  syncStatusStreamVisibility,
} from "../../features/status/index.js";
import { renderDashboardV2SettingsBody } from "../../features/settings/index.js";
import {
  renderDashboardV2MediaBody,
  resetMediaLibraryShellState,
} from "../../features/media-library.js";
import {
  clearDashboardV2IncidentsBody,
  renderDashboardV2IncidentsBody,
} from "../../features/incidents-route-body.js";
import {
  clearDashboardV2TelemetryBody,
  renderDashboardV2TelemetryBody,
} from "../../features/telemetry-route-body.js";
import {
  renderDashboardV2ControlRoomBody,
} from "../../features/control-room-route-body.js";
import {
  renderDashboardV2PipelineInspectBody,
} from "../../features/pipeline-inspect-route-body.js";
import { selectPipeline } from "../../features/render.js";
import { buildOverviewViewModel } from "../../features/overview-view-model.js";
import { syncPipelineWorkspaceShell } from "./pipeline-workspace-shell.js";
import {
  refreshDashboard,
  refreshDashboardRuntime,
  requestDetailedMetricsRefresh,
  syncDashboardPolling,
  syncDashboardRuntimeStream,
} from "../../features/dashboard.js";
import {
  renderOverview,
  syncOverviewActivityStream,
} from "./overview.js";
import {
  DASHBOARD_V2_ROUTE_BODIES,
  dashboardV2RouteBodyConfig,
  type DashboardV2RouteBodyMode,
} from "../dashboard-v2-route-bodies.js";

type NavigationOptions = {
  focus?: "panel" | "none";
};

let currentMode: DashboardMode | null = null;
let currentPipelineView: PipelineWorkspaceView | null = null;
let settingsMounted = false;
let dashboardModePresentationSync:
  | ((location: DashboardLocation) => void)
  | null = null;

const rememberedPipelineUrlsKey = "restream:remembered-pipeline-urls";
export function configureDashboardModePresentationSync(
  callback: ((location: DashboardLocation) => void) | null,
): void {
  dashboardModePresentationSync = callback;
}

function rememberPipelineWorkspaceLocation(urlHref: string): void {
  try {
    sessionStorage.setItem(rememberedPipelineUrlsKey, urlHref);
  } catch {}
}

function restoredPipelineWorkspaceUrl(): URL | null {
  try {
    const raw = sessionStorage.getItem(rememberedPipelineUrlsKey);
    if (raw) return new URL(raw);
  } catch {}
  return null;
}

function focusActivePanel(): void {
  const activeModeContainer = document.querySelector<HTMLElement>(
    '#dashboard-main [role="tabpanel"]:not(.hidden)',
  );
  if (!activeModeContainer) return;
  activeModeContainer.tabIndex = -1;
  activeModeContainer.focus({ preventScroll: true });
  activeModeContainer.scrollIntoView({ block: "start" });
}

function dashboardV2RouteBodyMode(
  mode: DashboardMode,
  pipelineView: PipelineWorkspaceView,
): DashboardV2RouteBodyMode | null {
  if (mode === "pipeline" && pipelineView === "inspect")
    return "pipeline-inspect";
  if (mode === "pipeline" && pipelineView === "monitor")
    return "pipeline-monitor";
  if (
    mode === "incidents" ||
    mode === "telemetry" ||
    mode === "media" ||
    mode === "settings" ||
    mode === "status"
  ) {
    return mode;
  }
  return null;
}

function clearDormantRouteBodies(options: {
  readonly activeMode: DashboardV2RouteBodyMode | null;
}): void {
  const { activeMode } = options;
  for (const config of DASHBOARD_V2_ROUTE_BODIES) {
    if (activeMode !== config.mode) {
      document.getElementById(config.hostId)?.replaceChildren();
    }
    document.getElementById(config.hostId)?.toggleAttribute(
      "hidden",
      activeMode !== config.mode,
    );
  }
  if (activeMode !== "media") resetMediaLibraryShellState();
  if (activeMode !== "settings") settingsMounted = false;
}

function configureDashboardV2RouteBodyTargets(
  mode: DashboardMode,
  pipelineView: PipelineWorkspaceView,
): void {
  const activeMode = dashboardV2RouteBodyMode(mode, pipelineView);
  clearDormantRouteBodies({ activeMode });
}

function unmountInactiveV2HeavyRoute(previousMode: DashboardMode | null): void {
  if (
    previousMode !== "incidents" &&
    previousMode !== "telemetry" &&
    previousMode !== "media" &&
    previousMode !== "settings" &&
    previousMode !== "status"
  )
    return;
  if (previousMode === currentMode) return;
  if (previousMode === "media") resetMediaLibraryShellState();
  if (previousMode === "settings") settingsMounted = false;
}

function renderSettingsMode(containerId: string): void {
  const container = document.getElementById(containerId);
  if (!container) return;
  if (!settingsMounted || container.children.length === 0) {
    renderDashboardV2SettingsBody(container);
    settingsMounted = true;
  }
}

function renderStatusMode(containerId: string): void {
  const container = document.getElementById(containerId);
  if (!container) return;
  void renderDashboardV2StatusBody(container);
}

const runtimeDashboardModes = new Set<DashboardMode>([
  "overview",
  "pipeline",
  "incidents",
  "telemetry",
  "media",
]);

function applyMode(mode: DashboardMode, pipelineView: PipelineWorkspaceView | null): void {
  const previousMode = currentMode;
  currentMode = mode;
  currentPipelineView = pipelineView;

  syncOverviewActivityStream();
  setStatusStreamActive(mode === "status");
  syncDashboardRuntimeStream();

  if (
    previousMode !== null &&
    previousMode !== mode &&
    !runtimeDashboardModes.has(previousMode) &&
    runtimeDashboardModes.has(mode)
  ) {
    void refreshDashboard();
  }
  const activePipelineView = pipelineView ?? "operate";
  const panels: Record<
    Exclude<DashboardMode, "pipeline">,
    HTMLElement | null
  > = {
    overview: document.getElementById("overview-mode-panel"),
    incidents: document.getElementById("incidents-mode-panel"),
    telemetry: document.getElementById("telemetry-mode-panel"),
    media: document.getElementById("media-mode-panel"),
    settings: document.getElementById("settings-mode-panel"),
    status: document.getElementById("status-mode-panel"),
  };
  for (const [name, panel] of Object.entries(panels)) {
    panel?.classList.toggle("hidden", name !== mode);
  }
  unmountInactiveV2HeavyRoute(previousMode);
  syncPipelineWorkspaceShell(mode, activePipelineView);

  let activeModeButton: HTMLButtonElement | null = null;
  document
    .querySelectorAll<HTMLButtonElement>("[data-dashboard-mode]")
    .forEach((button) => {
      const active = button.dataset.dashboardMode === mode;
      button.classList.toggle("btn-accent", active);
      button.classList.toggle("btn-outline", !active);
      button.setAttribute("aria-selected", active ? "true" : "false");
      button.tabIndex = active ? 0 : -1;
      if (active) activeModeButton = button;
    });
  scrollTabIntoView(activeModeButton);

  const summary = document.getElementById("workspace-mode-summary");
  if (summary) {
    const counts = buildOverviewViewModel(state.pipelines).counts;
    const taskSummary =
      mode === "overview"
        ? `${counts.liveInputs} live inputs / ${counts.runningOutputs} running outputs${counts.retryingOutputs ? ` / ${counts.retryingOutputs} retrying` : ""}${counts.flappingOutputs ? ` / ${counts.flappingOutputs} flapping` : ""}`
        : mode === "pipeline"
          ? activePipelineView === "inspect"
            ? "Pipeline graph and diagnostics"
            : activePipelineView === "monitor"
              ? "Pipeline monitoring wall"
              : "Pipeline workflow"
          : mode === "incidents"
            ? "Alerts, evidence, and lifecycle events"
            : mode === "telemetry"
              ? "Engine and pipeline counters"
              : mode === "media"
                ? "Recordings and source files"
                : mode === "settings"
                  ? "Server configuration"
                  : "Runtime status";
    summary.textContent = `Dashboard · ${taskSummary}`;
  }
  if (mode === "pipeline" && activePipelineView === "inspect") {
    renderDashboardV2PipelineInspectBody(
      dashboardV2RouteBodyConfig("pipeline-inspect").hostId,
    );
  } else if (mode === "pipeline" && activePipelineView === "monitor") {
    renderDashboardV2ControlRoomBody(
      dashboardV2RouteBodyConfig("pipeline-monitor").hostId,
    );
  }
  const pipelineOptions = state.pipelines.map((pipeline) => ({
    id: pipeline.id,
    name: pipeline.name || pipeline.id,
  }));
  if (mode === "incidents") {
    renderDashboardV2IncidentsBody(
      dashboardV2RouteBodyConfig("incidents").hostId,
      {
        pipelines: pipelineOptions,
        navigateToPipeline: (pipelineId) => {
          selectPipeline(pipelineId);
          setDashboardMode("pipeline");
        },
      },
    );
  } else {
    clearDashboardV2IncidentsBody();
  }
  if (mode === "telemetry") {
    renderDashboardV2TelemetryBody(
      dashboardV2RouteBodyConfig("telemetry").hostId,
      { pipelines: pipelineOptions },
    );
  } else {
    clearDashboardV2TelemetryBody();
  }

  if (mode === "settings") {
    renderSettingsMode(dashboardV2RouteBodyConfig("settings").hostId);
  } else if (mode === "status") {
    renderStatusMode(dashboardV2RouteBodyConfig("status").hostId);
  } else if (mode === "media") {
    const container = document.getElementById(
      dashboardV2RouteBodyConfig("media").hostId,
    );
    if (container) {
      const result = renderDashboardV2MediaBody(container, {
        routeChanged: previousMode !== "media",
      });
      if (result.needsDashboardRuntimeRefresh) {
        requestDetailedMetricsRefresh();
        void refreshDashboardRuntime();
      }
      if (result.rendered) void result.rendered;
    }
  }
  syncDashboardPolling();
}

function refreshActiveMode(options: NavigationOptions = {}): void {
  renderDashboardModes();
  if (options.focus === "panel") focusActivePanel();
}

function pushDashboardUrl(url: URL): boolean {
  if (url.href === window.location.href) return false;
  window.history.pushState({}, "", url);
  return true;
}

function setModeUrl(mode: DashboardMode): void {
  const url = dashboardModeUrl(window.location.href, mode);
  pushDashboardUrl(url);
}

function tablistNavigationTarget(
  button: HTMLButtonElement,
  selector: string,
  key: string,
): HTMLButtonElement | null {
  const tablist = button.closest('[role="tablist"]');
  if (!tablist) return null;
  const tabs = Array.from(
    tablist.querySelectorAll<HTMLButtonElement>(selector),
  ).filter((tab) => !tab.disabled && tab.offsetParent !== null);
  if (tabs.length === 0) return null;
  const index = tabs.indexOf(button);
  if (key === "Home") return tabs[0];
  if (key === "End") return tabs[tabs.length - 1];
  if (index < 0) return null;
  if (key === "ArrowRight" || key === "ArrowDown") {
    return tabs[(index + 1) % tabs.length];
  }
  if (key === "ArrowLeft" || key === "ArrowUp") {
    return tabs[(index - 1 + tabs.length) % tabs.length];
  }
  return null;
}

function scrollTabIntoView(target: HTMLElement | null): void {
  if (!target || typeof target.scrollIntoView !== "function") return;
  target.scrollIntoView({ behavior: "smooth", block: "nearest", inline: "nearest" });
}

function activateTabFromKeyboard(
  event: KeyboardEvent,
  button: HTMLButtonElement,
  selector: string,
): void {
  const target = tablistNavigationTarget(button, selector, event.key);
  if (!target) return;
  event.preventDefault();
  target.focus();
  scrollTabIntoView(target);
  target.click();
}

export function setDashboardMode(
  mode: string,
  options: NavigationOptions = {},
): void {
  if (mode === "inspect" || mode === "control") {
    setPipelineWorkspaceView(
      mode === "inspect" ? "inspect" : "monitor",
      undefined,
      options,
    );
    return;
  }
  const currentLocation = resolveDashboardLocation(window.location.href);
  if (currentLocation.mode === "pipeline") {
    rememberPipelineWorkspaceLocation(currentLocation.url.href);
  }
  const candidate = new URL(window.location.href);
  candidate.searchParams.set("mode", mode);
  candidate.searchParams.delete("view");
  const nextMode = resolveDashboardLocation(candidate.href).mode;
  if (nextMode === "pipeline" && currentLocation.mode === "pipeline") {
    applyMode(nextMode, currentLocation.pipelineView);
    if (options.focus === "panel") focusActivePanel();
    return;
  }
  const restoredPipelineUrl =
    nextMode === "pipeline" ? restoredPipelineWorkspaceUrl() : null;
  if (restoredPipelineUrl) {
    pushDashboardUrl(restoredPipelineUrl);
  } else {
    setModeUrl(nextMode);
  }
  if (currentMode === nextMode) {
    const location = resolveDashboardLocation(window.location.href);
    applyMode(nextMode, location.pipelineView);
    if (options.focus === "panel") focusActivePanel();
    return;
  }
  refreshActiveMode(options);
}

export function setPipelineWorkspaceView(
  view: PipelineWorkspaceView,
  pipelineId?: string | null,
  options: NavigationOptions = {},
): void {
  const url = pipelineWorkspaceUrl(window.location.href, view, pipelineId);
  pushDashboardUrl(url);
  rememberPipelineWorkspaceLocation(url.href);
  refreshActiveMode(options);
}

export function openInspectGraph(
  pipeId: string,
  options: NavigationOptions = {},
): void {
  resetPipelineInspectorSelection(pipeId);
  setPipelineWorkspaceView("inspect", pipeId, options);
}

export function renderDashboardModes(): void {
  const location = canonicalizeDashboardLocation();
  if (location.mode === "pipeline") {
    rememberPipelineWorkspaceLocation(location.url.href);
  }
  dashboardModePresentationSync?.(location);
  configureDashboardV2RouteBodyTargets(location.mode, location.pipelineView);
  if (location.mode === "overview") renderOverview();
  applyMode(location.mode, location.pipelineView);
}

export function initDashboardModes(): void {
  document
    .querySelectorAll<HTMLButtonElement>("[data-dashboard-mode]")
    .forEach((button) => {
      button.onclick = () =>
        setDashboardMode(button.dataset.dashboardMode || "overview");
      button.onkeydown = (event) =>
        activateTabFromKeyboard(event, button, "[data-dashboard-mode]");
    });
  document
    .querySelectorAll<HTMLButtonElement>("[data-pipeline-workspace-view]")
    .forEach((button) => {
      button.onclick = () =>
        setPipelineWorkspaceView(
          (button.dataset.pipelineWorkspaceView ||
            "operate") as PipelineWorkspaceView,
        );
      button.onkeydown = (event) =>
        activateTabFromKeyboard(
          event,
          button,
          "[data-pipeline-workspace-view]",
        );
    });
  window.addEventListener("popstate", () => refreshActiveMode());
  document.addEventListener("visibilitychange", () => {
    syncOverviewActivityStream();
    syncStatusStreamVisibility();
    if (
      !document.hidden &&
      resolveDashboardLocation(window.location.href).mode === "pipeline" &&
      resolveDashboardLocation(window.location.href).pipelineView === "inspect"
    ) {
      syncPipelineInspectorVisibility();
    }
  });
  document.addEventListener("dashboard:v2-checkpoints-ready", () => {
    renderDashboardModes();
  });
  (window as any).setDashboardMode = setDashboardMode;
  refreshActiveMode();
}
