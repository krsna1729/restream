import { useEffect, useState } from "react";

import { withBasePath } from "../core/base-path.js";
import { Fleet } from "./fleet.js";
import { formatAge } from "./format.js";
import {
  WORKSPACES,
  WORKSPACE_LABELS,
  parseWorkspace,
  workspaceSearch,
} from "./route.js";
import type { Workspace } from "./route.js";
import { useLive } from "./store.js";

// Data older than this is flagged: the stream or the sampler is not keeping up.
const STALE_AFTER_MS = 10_000;

function useWorkspace(): [Workspace, (next: Workspace) => void] {
  const [workspace, setWorkspace] = useState(() =>
    parseWorkspace(window.location.search),
  );
  useEffect(() => {
    const onPop = () => setWorkspace(parseWorkspace(window.location.search));
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);
  const navigate = (next: Workspace) => {
    if (next === workspace) return;
    window.history.pushState(
      {},
      "",
      workspaceSearch(window.location.search, next),
    );
    setWorkspace(next);
  };
  return [workspace, navigate];
}

function useNow(intervalMs: number): number {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), intervalMs);
    return () => window.clearInterval(timer);
  }, [intervalMs]);
  return now;
}

export function App() {
  const live = useLive();
  const [workspace, navigate] = useWorkspace();
  const now = useNow(1000);
  const stale =
    live.stateAtMs === null || now - live.stateAtMs > STALE_AFTER_MS;
  return (
    <div className="flex min-h-screen flex-col">
      <header className="navbar gap-4 bg-base-200 px-4">
        <h1 className="text-lg font-semibold">Restream control room</h1>
        <nav aria-label="Workspaces" className="flex-1">
          <ul className="menu menu-horizontal gap-1 p-0">
            {WORKSPACES.map((id) => (
              <li key={id}>
                <a
                  href={workspaceSearch(window.location.search, id)}
                  aria-current={id === workspace ? "page" : undefined}
                  className={id === workspace ? "menu-active" : undefined}
                  onClick={(event) => {
                    event.preventDefault();
                    navigate(id);
                  }}
                >
                  {WORKSPACE_LABELS[id]}
                </a>
              </li>
            ))}
          </ul>
        </nav>
        <span
          role="status"
          className={`badge ${stale ? "badge-warning" : "badge-ghost"}`}
        >
          {live.stateAtMs === null
            ? "connecting…"
            : `state ${formatAge(live.stateAtMs, now)} · sample ${formatAge(
                live.sampledAtMs,
                now,
              )}`}
        </span>
        <a className="link text-sm" href={withBasePath("/")}>
          Classic dashboard
        </a>
      </header>
      <main className="mx-auto w-full max-w-7xl flex-1 p-4">
        {workspace === "fleet" && <Fleet live={live} />}
      </main>
    </div>
  );
}
