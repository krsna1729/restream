import { initDashboardApp } from "./dashboard-app.js";
import { startDashboardV2Experiment } from "./dashboard-v2-loader.js";
import { startDashboardRuntime } from "../features/dashboard.js";

initDashboardApp();
startDashboardRuntime();
void startDashboardV2Experiment().catch((error: unknown) => {
  console.error("Unable to start the dashboard v2 experiment", error);
});
