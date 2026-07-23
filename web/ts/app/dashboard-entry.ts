import { initDashboardApp } from "./dashboard-app.js";
import { startDashboardRuntime } from "../features/dashboard.js";

function initSkipToMainContent(): void {
  const skipLink = document.getElementById("skip-to-dashboard-main");
  const main = document.getElementById("dashboard-main");
  if (!(skipLink instanceof HTMLAnchorElement) || !main) return;
  document.addEventListener("keydown", (event) => {
    if (
      event.key !== "Tab" ||
      event.shiftKey ||
      event.altKey ||
      event.ctrlKey ||
      event.metaKey ||
      document.activeElement !== document.body
    )
      return;
    event.preventDefault();
    skipLink.focus();
  });
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
initDashboardApp();
startDashboardRuntime();
