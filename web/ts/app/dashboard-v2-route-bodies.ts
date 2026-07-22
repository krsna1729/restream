export type DashboardV2RouteBodyMode =
  | "pipeline-inspect"
  | "pipeline-monitor"
  | "incidents"
  | "telemetry"
  | "media"
  | "settings"
  | "status";

export interface DashboardV2RouteBodyConfig {
  readonly mode: DashboardV2RouteBodyMode;
  readonly hostId: string;
}

const DASHBOARD_V2_ROUTE_BODY_CONFIGS = {
  "pipeline-inspect": {
    mode: "pipeline-inspect",
    hostId: "dashboard-v2-pipeline-inspect-content",
  },
  "pipeline-monitor": {
    mode: "pipeline-monitor",
    hostId: "dashboard-v2-control-room-content",
  },
  incidents: {
    mode: "incidents",
    hostId: "dashboard-v2-incidents-content",
  },
  telemetry: {
    mode: "telemetry",
    hostId: "dashboard-v2-telemetry-content",
  },
  media: {
    mode: "media",
    hostId: "dashboard-v2-media-content",
  },
  settings: {
    mode: "settings",
    hostId: "dashboard-v2-settings-content",
  },
  status: {
    mode: "status",
    hostId: "dashboard-v2-status-content",
  },
} as const satisfies Readonly<
  Record<DashboardV2RouteBodyMode, DashboardV2RouteBodyConfig>
>;

export const DASHBOARD_V2_ROUTE_BODIES: readonly DashboardV2RouteBodyConfig[] =
  Object.values(DASHBOARD_V2_ROUTE_BODY_CONFIGS);

export function dashboardV2RouteBodyConfig(
  mode: DashboardV2RouteBodyMode,
): DashboardV2RouteBodyConfig {
  return DASHBOARD_V2_ROUTE_BODY_CONFIGS[mode];
}
