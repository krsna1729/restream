import { useEffect, useState } from "react";

import { withBasePath } from "../core/base-path.js";
import { Capacity } from "./capacity.js";
import { Fleet } from "./fleet.js";
import { formatAge } from "./format.js";
import { Incidents } from "./incidents.js";
import { Inspect } from "./inspect.js";
import { Media } from "./media.js";
import { Monitor } from "./monitor.js";
import { Operate } from "./operate.js";
import { PlaceLink } from "./place-link.js";
import { Pipelines } from "./pipelines.js";
import { WORKSPACES, WORKSPACE_LABELS, parsePlace } from "./route.js";
import { Settings } from "./settings.js";
import { System } from "./system.js";
import type { Place } from "./route.js";
import { SAMPLE_STALE_AFTER_MS, useLive, useNow } from "./store.js";

/** The place in the URL; links move it with `go`, which fires `popstate`. */
function usePlace(): Place {
  const [place, setPlace] = useState(() => parsePlace(window.location.search));
  useEffect(() => {
    const onPop = () => setPlace(parsePlace(window.location.search));
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);
  return place;
}

export function App() {
  const live = useLive();
  const place = usePlace();
  const now = useNow(1000);
  // The oldest sampled source: one failing endpoint shows here even while
  // the other keeps answering.
  const sampledAtMs =
    live.telemetryAtMs === null || live.metricsAtMs === null
      ? null
      : Math.min(live.telemetryAtMs, live.metricsAtMs);
  const stale =
    live.stream !== "live" ||
    sampledAtMs === null ||
    now - sampledAtMs > SAMPLE_STALE_AFTER_MS;
  return (
    <div className="flex min-h-screen flex-col">
      <header className="navbar gap-4 bg-base-200 px-4">
        <h1 className="text-lg font-semibold">Restream control room</h1>
        <nav aria-label="Workspaces" className="flex-1">
          <ul className="menu menu-horizontal gap-1 p-0">
            {WORKSPACES.map((id) => (
              <li key={id}>
                <PlaceLink
                  place={{ workspace: id, pipelineId: place.pipelineId }}
                  current={id === place.workspace}
                  className={id === place.workspace ? "menu-active" : undefined}
                >
                  {WORKSPACE_LABELS[id]}
                </PlaceLink>
              </li>
            ))}
          </ul>
        </nav>
        {/* core/api.ts toggles this element's class; React must own neither
            its class (keep it a constant) nor its children. */}
        <span id="saving-badge" className="badge badge-info hidden">
          saving…
        </span>
        <span
          role="status"
          className={`badge ${stale ? "badge-warning" : "badge-ghost"}`}
        >
          {live.stream === "live"
            ? `live · rev ${live.state?.revision ?? "--"}`
            : live.stream}{" "}
          · sample {formatAge(sampledAtMs, now)}
        </span>
        <a className="link text-sm" href={withBasePath("/")}>
          Classic dashboard
        </a>
      </header>
      {/* core/utils.ts toggles the class and writes #error-msg's text; React
          must own neither (constant class, no children). */}
      <div
        id="error-alert"
        role="alert"
        className="alert alert-error fixed bottom-4 right-4 z-50 hidden max-w-md"
      >
        <span id="error-msg" />
      </div>
      <main className="mx-auto w-full max-w-7xl flex-1 p-4">
        {place.workspace === "fleet" && <Fleet live={live} />}
        {place.workspace === "pipelines" && <Pipelines live={live} />}
        {place.workspace === "operate" && (
          <Operate live={live} pipelineId={place.pipelineId} />
        )}
        {place.workspace === "inspect" && (
          <Inspect live={live} pipelineId={place.pipelineId} />
        )}
        {place.workspace === "monitor" && (
          <Monitor live={live} pipelineId={place.pipelineId} />
        )}
        {place.workspace === "incidents" && <Incidents live={live} />}
        {place.workspace === "capacity" && <Capacity live={live} />}
        {place.workspace === "media" && <Media live={live} />}
        {place.workspace === "system" && <System live={live} />}
        {place.workspace === "settings" && <Settings />}
      </main>
    </div>
  );
}
