export type PipelineWorkspaceView = "operate" | "inspect" | "monitor";

export type DashboardMode =
  | "overview"
  | "incidents"
  | "telemetry"
  | "pipeline"
  | "media"
  | "settings"
  | "status";

export interface DashboardLocation {
  mode: DashboardMode;
  pipelineView: PipelineWorkspaceView;
  url: URL;
  needsCanonicalReplace: boolean;
}

const dashboardModes = new Set<DashboardMode>([
  "overview",
  "incidents",
  "telemetry",
  "pipeline",
  "media",
  "settings",
  "status",
]);
const pipelineViews = new Set<PipelineWorkspaceView>([
  "operate",
  "inspect",
  "monitor",
]);

export function resolveDashboardLocation(href: string): DashboardLocation {
  const url = new URL(href);
  const requestedMode = url.searchParams.get("mode");
  const requestedView = url.searchParams.get("view");
  let mode: DashboardMode;
  let pipelineView: PipelineWorkspaceView = "operate";

  if (requestedMode === "inspect" || requestedMode === "control") {
    mode = "pipeline";
    pipelineView = requestedMode === "inspect" ? "inspect" : "monitor";
  } else {
    mode =
      requestedMode === "admin"
        ? "settings"
        : requestedMode && dashboardModes.has(requestedMode as DashboardMode)
          ? (requestedMode as DashboardMode)
          : url.searchParams.has("p")
            ? "pipeline"
            : "overview";
    if (
      mode === "pipeline" &&
      requestedView &&
      pipelineViews.has(requestedView as PipelineWorkspaceView)
    ) {
      pipelineView = requestedView as PipelineWorkspaceView;
    }
  }

  const canonicalUrl = new URL(url);
  canonicalUrl.searchParams.set("mode", mode);
  if (mode === "pipeline") {
    canonicalUrl.searchParams.set("view", pipelineView);
  } else {
    canonicalUrl.searchParams.delete("view");
    canonicalUrl.searchParams.delete("p");
  }

  return {
    mode,
    pipelineView,
    url: canonicalUrl,
    needsCanonicalReplace: canonicalUrl.href !== url.href,
  };
}

export function canonicalizeDashboardLocation(): DashboardLocation {
  const location = resolveDashboardLocation(window.location.href);
  if (location.needsCanonicalReplace) {
    window.history.replaceState({}, "", location.url);
  }
  return location;
}

export function dashboardModeUrl(href: string, mode: DashboardMode): URL {
  const url = new URL(href);
  url.searchParams.set("mode", mode);
  if (mode === "pipeline") {
    url.searchParams.set("view", "operate");
  } else {
    url.searchParams.delete("view");
    url.searchParams.delete("p");
  }
  return url;
}

export function pipelineWorkspaceUrl(
  href: string,
  view: PipelineWorkspaceView,
  pipelineId?: string | null,
): URL {
  const url = new URL(href);
  url.searchParams.set("mode", "pipeline");
  url.searchParams.set("view", view);
  if (pipelineId === null) url.searchParams.delete("p");
  if (pipelineId) url.searchParams.set("p", pipelineId);
  return url;
}

export function syncPipelineWorkspaceShell(
  mode: DashboardMode,
  view: PipelineWorkspaceView,
): void {
  const active = mode === "pipeline";
  document
    .getElementById("pipeline-workspace-view-bar")
    ?.classList.toggle("hidden", !active);

  document
    .querySelectorAll<HTMLButtonElement>("[data-pipeline-workspace-view]")
    .forEach((button) => {
      const selected = active && button.dataset.pipelineWorkspaceView === view;
      button.classList.toggle("btn-accent", selected);
      button.classList.toggle("btn-outline", !selected);
      button.setAttribute("aria-pressed", selected ? "true" : "false");
    });

  const panels: Record<PipelineWorkspaceView, HTMLElement | null> = {
    operate: document.getElementById("dashboard-grid"),
    inspect: document.getElementById("inspect-mode-panel"),
    monitor: document.getElementById("control-mode-panel"),
  };
  for (const [panelView, panel] of Object.entries(panels)) {
    panel?.classList.toggle("hidden", !active || panelView !== view);
  }
}
