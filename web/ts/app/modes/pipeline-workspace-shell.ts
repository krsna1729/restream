import type {
  DashboardMode,
  PipelineWorkspaceView,
} from "../../core/pipeline-workspace.js";

function scrollPipelineTabIntoView(tab: HTMLButtonElement | null): void {
  if (!tab || typeof tab.scrollIntoView !== "function") return;
  tab.scrollIntoView({
    block: "nearest",
    inline: "nearest",
  });
}

export function syncPipelineWorkspaceShell(
  mode: DashboardMode,
  view: PipelineWorkspaceView,
): void {
  const active = mode === "pipeline";
  document
    .getElementById("pipeline-workspace-view-bar")
    ?.classList.toggle("hidden", !active);

  let activeViewButton: HTMLButtonElement | null = null;
  document
    .querySelectorAll<HTMLButtonElement>("[data-pipeline-workspace-view]")
    .forEach((button) => {
      const selected = active && button.dataset.pipelineWorkspaceView === view;
      button.classList.toggle("btn-accent", selected);
      button.classList.toggle("btn-outline", !selected);
      button.setAttribute("aria-selected", selected ? "true" : "false");
      button.tabIndex = selected ? 0 : -1;
      if (selected) activeViewButton = button;
    });
  scrollPipelineTabIntoView(activeViewButton);

  const panels: Record<PipelineWorkspaceView, HTMLElement | null> = {
    operate: document.getElementById("dashboard-v2-operate-panel"),
    inspect: document.getElementById("inspect-mode-panel"),
    monitor: document.getElementById("control-mode-panel"),
  };
  for (const [panelView, panel] of Object.entries(panels)) {
    panel?.classList.toggle("hidden", !active || panelView !== view);
  }
}
