import { initDashboardApp } from "./dashboard-app.js";
import {
  initDashboardUiVersionToggle,
  startDashboardV2Experiment,
} from "./dashboard-v2-loader.js";
import { startDashboardRuntime } from "../features/dashboard.js";

function initSkipToMainContent(): void {
  const skipLink = document.getElementById("skip-to-dashboard-main");
  const main = document.getElementById("dashboard-main");
  if (!(skipLink instanceof HTMLAnchorElement) || !main) return;
  skipLink.addEventListener("click", (event) => {
    event.preventDefault();
    const target =
      main.querySelector<HTMLElement>('[role="tabpanel"]:not(.hidden)') ?? main;
    target.tabIndex = -1;
    target.focus({ preventScroll: true });
    target.scrollIntoView({ block: "start" });
    if (window.location.hash !== "#dashboard-main") {
      window.history.pushState({}, "", "#dashboard-main");
    }
  });
}

initSkipToMainContent();
initDashboardUiVersionToggle();
initDashboardApp();
startDashboardRuntime();
void startDashboardV2Experiment().catch((error: unknown) => {
  console.error("Unable to start the dashboard v2 experiment", error);
});
