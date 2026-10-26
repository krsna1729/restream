import { lazy, Suspense, useCallback, useMemo, useState } from "react";

import { PrefsProvider } from "../ui/prefs.js";
import { Fleet } from "./fleet.js";
import { useOpenAlerts } from "./polled.js";
import { usePlace } from "./router.js";
import { navBadges } from "./shell/badges.js";
import { CommandPalette, useCommandPaletteShortcut } from "./shell/command-palette.js";
import { MobileTabs, MoreSheet } from "./shell/mobile-nav.js";
import { Sidebar } from "./shell/sidebar.js";
import { Topbar, ViewAsBanner } from "./shell/topbar.js";
import { useLive } from "./store.js";
import { useInTabAlertNotifications } from "./browser-notify.js";
import { useTabAlerts } from "./tab-alerts.js";
import { HelpButton, TourProvider } from "./shell/help.js";

// Each workspace loads when first opened; Fleet, the landing page, ships
// with the shell. Their modules are already separate files (preserveModules).
const Capacity = lazy(() => import("./capacity.js").then((module) => ({ default: module.Capacity })));
const Changes = lazy(() => import("./changes.js").then((module) => ({ default: module.Changes })));
const Channels = lazy(() => import("./channels.js").then((module) => ({ default: module.Channels })));
const Incidents = lazy(() => import("./incidents.js").then((module) => ({ default: module.Incidents })));
const Inspect = lazy(() => import("./inspect.js").then((module) => ({ default: module.Inspect })));
const Logs = lazy(() => import("./logs.js").then((module) => ({ default: module.Logs })));
const Media = lazy(() => import("./media.js").then((module) => ({ default: module.Media })));
const Monitor = lazy(() => import("./monitor.js").then((module) => ({ default: module.Monitor })));
const Operate = lazy(() => import("./operate.js").then((module) => ({ default: module.Operate })));
const Pipelines = lazy(() => import("./pipelines.js").then((module) => ({ default: module.Pipelines })));
const Settings = lazy(() => import("./settings.js").then((module) => ({ default: module.Settings })));
const StreamHealth = lazy(() => import("./stream-health.js").then((module) => ({ default: module.StreamHealth })));
const Wall = lazy(() => import("./wall.js").then((module) => ({ default: module.Wall })));
const StabilityLab = lazy(() => import("./stability/stability.js").then((module) => ({ default: module.StabilityLab })));
const System = lazy(() => import("./system.js").then((module) => ({ default: module.System })));
const Notifications = lazy(() => import("./notifications.js").then((module) => ({ default: module.Notifications })));
const ScheduleView = lazy(() => import("./schedule.js").then((module) => ({ default: module.ScheduleView })));
const Architecture = lazy(() => import("./architecture.js").then((module) => ({ default: module.Architecture })));
const Guides = lazy(() => import("./guides.js").then((module) => ({ default: module.Guides })));
const Team = lazy(() => import("./team.js").then((module) => ({ default: module.Team })));
const Audit = lazy(() => import("./team.js").then((module) => ({ default: module.Audit })));

export function App() {
  return (
    <PrefsProvider>
      <Shell />
    </PrefsProvider>
  );
}

function Shell() {
  const live = useLive();
  const place = usePlace();
  const alerts = useOpenAlerts();
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [moreOpen, setMoreOpen] = useState(false);
  const openPalette = useCallback(() => setPaletteOpen(true), []);
  useCommandPaletteShortcut(openPalette);

  const openAlerts = alerts.value?.alerts ?? null;
  useInTabAlertNotifications(openAlerts);
  useTabAlerts(openAlerts);
  const badges = useMemo(
    () =>
      navBadges(
        live.state,
        openAlerts && {
          total: openAlerts.length,
          critical: openAlerts.filter((alert) => alert.severity === "critical").length,
        },
      ),
    [live.state, openAlerts],
  );

  return (
    <TourProvider firstPipelineId={live.state?.pipelines[0]?.id ?? null}>
      <div className="flex min-h-dvh bg-base-300">
        <Sidebar live={live} badges={badges} />
        <div className="flex min-w-0 flex-1 flex-col">
          <Topbar live={live} onSearch={openPalette} extra={<HelpButton />} />
          <ViewAsBanner />
          {/* core/api.ts toggles these elements' classes and writes
              #error-msg's text; React owns neither (constant classes, no
              children). */}
          {/* style-ok: core/api.ts toggles this element's classes */}
          <span id="saving-badge" className="badge badge-info fixed top-3 right-3 z-50 hidden">
            saving…
          </span>
          <div
            id="error-alert"
            role="alert"
            className="alert alert-error fixed right-4 bottom-20 z-50 hidden max-w-md md:bottom-4"
          >
            <span id="error-msg" />
          </div>
          <main className="mx-auto w-full max-w-[1680px] flex-1 px-3 pt-4 pb-24 md:px-6 md:pb-8">
            <Suspense fallback={<WorkspaceLoading />}>
            {place.workspace === "fleet" && <Fleet live={live} />}
            {place.workspace === "channels" && <Channels live={live} />}
            {place.workspace === "pipelines" && <Pipelines live={live} />}
            {place.workspace === "operate" && <Operate live={live} pipelineId={place.pipelineId} />}
            {place.workspace === "inspect" && <Inspect live={live} pipelineId={place.pipelineId} />}
            {place.workspace === "monitor" && <Monitor live={live} pipelineId={place.pipelineId} />}
            {place.workspace === "schedule" && <ScheduleView live={live} />}
            {place.workspace === "health" && <StreamHealth live={live} pipelineId={place.pipelineId} />}
            {place.workspace === "wall" && <Wall live={live} />}
            {place.workspace === "incidents" && <Incidents live={live} />}
            {place.workspace === "logs" && (
              <Logs live={live} pipelineId={place.pipelineId} outputId={place.outputId} />
            )}
            {place.workspace === "capacity" && <Capacity live={live} />}
            {place.workspace === "stability" && <StabilityLab live={live} />}
            {place.workspace === "media" && <Media live={live} />}
            {place.workspace === "system" && <System live={live} />}
            {place.workspace === "changes" && <Changes live={live} />}
            {place.workspace === "notifications" && <Notifications />}
            {place.workspace === "team" && <Team />}
            {place.workspace === "audit" && <Audit />}
            {place.workspace === "settings" && <Settings />}
            {place.workspace === "guides" && <Guides />}
            {place.workspace === "architecture" && <Architecture />}
            </Suspense>
          </main>
        </div>
        <MobileTabs badges={badges} onMore={() => setMoreOpen(true)} moreOpen={moreOpen} />
        <MoreSheet open={moreOpen} onClose={() => setMoreOpen(false)} live={live} badges={badges} />
        <CommandPalette
          open={paletteOpen}
          onClose={() => setPaletteOpen(false)}
          state={live.state}
          alerts={openAlerts ?? []}
        />
      </div>
    </TourProvider>
  );
}

function WorkspaceLoading() {
  return (
    <p className="text-sm text-base-content/70" role="status">
      Loading…
    </p>
  );
}
