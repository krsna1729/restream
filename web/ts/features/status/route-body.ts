import { loadStatus } from "./view.js";

export function renderDashboardV2StatusBody(
  container: HTMLElement,
): Promise<void> {
  container.dataset.statusRouteBody = "v2";
  if (!container.querySelector("#status-versions")) {
    container.innerHTML = `
      <div class="dashboard-page-shell">
        <div class="flex flex-wrap items-end justify-between gap-3">
          <div>
            <h1 class="dashboard-title">Status</h1>
            <p class="dashboard-subtitle">Runtime build, native libraries, and system details.</p>
          </div>
          <button type="button" class="btn btn-sm btn-outline" id="refresh-status-btn" aria-label="Refresh status data">Refresh</button>
        </div>
        <section class="dashboard-section p-5">
          <h2 class="dashboard-section-title mb-4">Runtime</h2>
          <div id="status-versions" class="space-y-5">
            <p class="text-sm opacity-60">Loading...</p>
          </div>
        </section>
      </div>`;
    container
      .querySelector<HTMLButtonElement>("#refresh-status-btn")
      ?.addEventListener("click", () => void loadStatus());
  }
  return loadStatus();
}
