import {
  dashboardModeUrl,
  pipelineWorkspaceUrl,
  resolveDashboardLocation,
  type DashboardLocation,
  type DashboardMode,
  type PipelineWorkspaceView,
} from "../../core/pipeline-workspace.js";
import { state } from "../../core/state.js";
import {
  renderPipelineInspector,
  resetPipelineInspectorSelection,
  syncPipelineInspectorVisibility,
} from "../../features/pipeline-inspector/index.js";
import {
  loadStatus,
  setStatusStreamActive,
  syncStatusStreamVisibility,
} from "../../features/status/index.js";
import { loadSettings, renderSettingsPanel } from "../../features/settings/index.js";
import { renderMediaLibraryMode } from "../../features/media-library.js";
import {
  refreshDashboard,
  refreshDashboardRuntime,
  requestDetailedMetricsRefresh,
  syncDashboardRuntimeStream,
} from "../../features/dashboard.js";
import { syncOverviewActivityStream } from "./overview.js";

type NavigationOptions = {
  focus?: "panel" | "none";
};

let currentMode: DashboardMode | null = null;
let currentPipelineView: PipelineWorkspaceView | null = null;
let settingsMounted = false;
let statusMounted = false;
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
    "#dashboard-v2-host [data-v2-body]:not([hidden])",
  );
  activeModeContainer?.focus();
}

function configureDashboardV2RouteBodyTargets(
  mode: DashboardMode,
  pipelineView: PipelineWorkspaceView | null,
): void {
  const v2Host = document.getElementById("dashboard-v2-host");
  if (!v2Host) return;

  const modeTargets: Record<string, string> = {
    overview: "dashboard-v2-overview-content",
    pipeline: "dashboard-v2-pipeline-content",
    incidents: "dashboard-v2-incidents-content",
    telemetry: "dashboard-v2-telemetry-content",
    media: "dashboard-v2-media-content",
    settings: "dashboard-v2-settings-content",
    status: "dashboard-v2-status-content",
  };

  Object.entries(modeTargets).forEach(([m, targetId]) => {
    const el = document.getElementById(targetId);
    if (el) {
      el.hidden = m !== mode;
    }
  });

  if (mode === "pipeline" && pipelineView) {
    const viewTargets: Record<string, string> = {
      operate: "pipeline-v2-operate-content",
      inspect: "pipeline-v2-inspect-content",
      monitor: "pipeline-v2-monitor-content",
    };
    Object.entries(viewTargets).forEach(([v, targetId]) => {
      const el = document.getElementById(targetId);
      if (el) {
        el.hidden = v !== pipelineView;
      }
    });
  }
}

function restorePipelineWorkspaceShell(_pipelineView: PipelineWorkspaceView): void {
  // Restores workspace shell DOM containers when switching views
}

function renderOverview(): void {
  // Renders dashboard overview metrics and cards
}

function canonicalizeDashboardLocation(): DashboardLocation {
  return resolveDashboardLocation(window.location.href);
}

function renderSettingsMode(containerId: string): void {
  const container = document.getElementById(containerId);
  if (!container) return;
  if (!settingsMounted || container.children.length === 0) {
    renderSettingsPanel(container);
    settingsMounted = true;
  } else {
    void loadSettings({ embedded: true });
  }
}

function renderStatusMode(containerId: string): void {
  const container = document.getElementById(containerId);
  if (!container) return;
  if (!statusMounted || container.children.length === 0) {
    void loadStatus();
    statusMounted = true;
  } else {
    void loadStatus();
  }
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

  if (mode === "settings") {
    renderSettingsMode("settings-mode-content");
  } else if (mode === "status") {
    renderStatusMode("status-mode-content");
  } else if (mode === "media") {
    if (previousMode !== "media") {
      requestDetailedMetricsRefresh();
      void refreshDashboardRuntime();
      void renderMediaLibraryMode({ force: false });
    }
  }
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

function scrollTabIntoView(target: HTMLElement): void {
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
  if (location.mode === "pipeline" && location.pipelineView === "inspect") {
    restorePipelineWorkspaceShell(location.pipelineView);
    renderPipelineInspector();
  }
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
    const location = resolveDashboardLocation(window.location.href);
    configureDashboardV2RouteBodyTargets(location.mode, location.pipelineView);
    if (
      location.mode === "pipeline" &&
      location.pipelineView === "inspect"
    ) {
      renderPipelineInspector();
      return;
    }
    applyMode(location.mode, location.pipelineView);
  });
  (window as any).setDashboardMode = setDashboardMode;
  refreshActiveMode();
}
