import { useCallback, useMemo, useState } from "react";

import { PrefsProvider } from "../ui/prefs.js";
import { Capacity } from "./capacity.js";
import { Changes } from "./changes.js";
import { Channels } from "./channels.js";
import { Fleet } from "./fleet.js";
import { Incidents } from "./incidents.js";
import { Inspect } from "./inspect.js";
import { Logs } from "./logs.js";
import { Media } from "./media.js";
import { Monitor } from "./monitor.js";
import { Operate } from "./operate.js";
import { Pipelines } from "./pipelines.js";
import { useOpenAlerts } from "./polled.js";
import { usePlace } from "./router.js";
import { Settings } from "./settings.js";
import { StreamHealth } from "./stream-health.js";
import { navBadges } from "./shell/badges.js";
import { CommandPalette, useCommandPaletteShortcut } from "./shell/command-palette.js";
import { MobileTabs, MoreSheet } from "./shell/mobile-nav.js";
import { Sidebar } from "./shell/sidebar.js";
import { Topbar, ViewAsBanner } from "./shell/topbar.js";
import { useLive } from "./store.js";
import { StabilityLab } from "./stability/stability.js";
import { System } from "./system.js";
import { Audit, Team } from "./team.js";
import { Notifications } from "./notifications.js";
import { ScheduleView } from "./schedule.js";
import { useInTabAlertNotifications } from "./browser-notify.js";

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
    <div className="flex min-h-dvh bg-base-300">
      <Sidebar live={live} badges={badges} />
      <div className="flex min-w-0 flex-1 flex-col">
        <Topbar live={live} onSearch={openPalette} />
        <ViewAsBanner />
        {/* core/api.ts toggles these elements' classes and writes
            #error-msg's text; React owns neither (constant classes, no
            children). */}
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
          {place.workspace === "fleet" && <Fleet live={live} />}
          {place.workspace === "channels" && <Channels live={live} />}
          {place.workspace === "pipelines" && <Pipelines live={live} />}
          {place.workspace === "operate" && <Operate live={live} pipelineId={place.pipelineId} />}
          {place.workspace === "inspect" && <Inspect live={live} pipelineId={place.pipelineId} />}
          {place.workspace === "monitor" && <Monitor live={live} pipelineId={place.pipelineId} />}
          {place.workspace === "schedule" && <ScheduleView live={live} />}
          {place.workspace === "health" && <StreamHealth live={live} pipelineId={place.pipelineId} />}
          {place.workspace === "incidents" && <Incidents live={live} />}
          {place.workspace === "logs" && (
            <Logs key={place.pipelineId ?? ""} live={live} pipelineId={place.pipelineId} />
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
  );
}
