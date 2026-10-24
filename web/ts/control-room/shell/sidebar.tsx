// The workspace navigation. A full sidebar on wide screens, an icon rail on
// tablets, and the same list inside the phone's More drawer.

import { Icon } from "../../ui/icons.js";
import { useSectionDetail } from "../../ui/prefs.js";
import { StatusDot } from "../../ui/tone.js";
import { formatBps, formatBytes, formatPercent } from "../format.js";
import { WORKSPACE_LABELS } from "../route.js";
import type { Workspace } from "../route.js";
import { PlaceLink, usePlace } from "../router.js";
import type { LiveSnapshot } from "../store.js";
import { compactCount } from "./badges.js";
import type { NavBadge } from "./badges.js";
import { useSession } from "../session.js";
import { NAV_ICONS, navGroupsFor } from "./nav.js";

const BADGE_TONE = {
  bad: "bg-error text-error-content",
  warn: "bg-warning text-warning-content",
  ok: "bg-success text-success-content",
  info: "bg-info text-info-content",
  accent: "bg-accent text-accent-content",
  neutral: "bg-base-content/10 text-base-content/80",
};

export function Brand(props: { compact?: boolean }) {
  return (
    <div className="flex items-center gap-2.5 px-2 py-1">
      <img src="logo.png" alt="" className="brand-logo size-8 shrink-0 rounded-lg" />
      {!props.compact && (
        <div className="min-w-0 leading-tight">
          <div className="truncate font-semibold">Restream</div>
          <div className="truncate text-xs text-base-content/70">control room</div>
        </div>
      )}
    </div>
  );
}

export function NavList(props: {
  badges: Partial<Record<Workspace, NavBadge>>;
  rail?: boolean;
  onNavigate?: () => void;
}) {
  const place = usePlace();
  const session = useSession();
  const [unfolded, setUnfolded] = useSectionDetail("nav.more-tools");
  return (
    <nav aria-label="Workspaces" className="flex flex-col gap-3">
      {navGroupsFor(session.role).map((group) => {
        const open = !group.folded || unfolded || group.items.includes(place.workspace);
        // A folded group still says when something in it needs a look.
        const flagged = group.items.map((id) => props.badges[id]).filter((badge) => badge && badge.tone !== "neutral" && badge.tone !== "ok");
        const worst = flagged.find((badge) => badge?.tone === "bad") ?? flagged[0];
        return (
          <div key={group.label}>
            {group.folded ? (
              <button
                type="button"
                aria-expanded={open}
                aria-controls={`nav-group-${group.label}`}
                disabled={group.items.includes(place.workspace)}
                title={group.items.includes(place.workspace) ? "Open while you are in one of its tools" : undefined}
                onClick={() => setUnfolded(!unfolded)}
                className={`touch-36 flex w-full items-center gap-2 rounded-field text-left hover:bg-base-content/5 disabled:cursor-default disabled:hover:bg-transparent ${props.rail ? "justify-center px-0 py-1" : "px-3 py-1"}`}
              >
                {props.rail ? (
                  <Icon name="more" className="size-[1.15rem] text-base-content/70" />
                ) : (
                  <span className="kicker flex-1">{group.label}</span>
                )}
                {props.rail && <span className="sr-only">{group.label}</span>}
                {!open && worst && (
                  <span title={worst.title} className={`size-2 rounded-full ${worst.tone === "bad" ? "bg-error" : "bg-warning"}`}>
                    <span className="sr-only">{worst.title}</span>
                  </span>
                )}
                {!props.rail && <Icon name="chevron" className={`size-3.5 text-base-content/70 transition-transform ${open ? "rotate-90" : ""}`} />}
              </button>
            ) : props.rail ? (
              <div className="mx-auto my-1 h-px w-6 bg-base-content/10" aria-hidden="true" />
            ) : (
              <div className="kicker px-3 pb-1">{group.label}</div>
            )}
            {open && (
              <ul id={`nav-group-${group.label}`} className="flex flex-col gap-0.5" onClick={props.onNavigate}>
                {group.items.map((id) => {
                  const current = id === place.workspace;
                  const badge = props.badges[id];
                  return (
                    <li key={id}>
                      <PlaceLink
                        place={{ workspace: id, pipelineId: place.pipelineId }}
                        current={current}
                        className={`relative flex min-h-9 items-center gap-3 rounded-field px-3 py-2 text-sm transition-colors ${current ? "bg-primary/15 font-semibold text-base-content" : "text-base-content/80 hover:bg-base-content/5 hover:text-base-content"} ${props.rail ? "justify-center px-0" : ""}`}
                      >
                        {current && <span aria-hidden="true" className="absolute inset-y-1.5 left-0 w-0.5 rounded-full bg-primary" />}
                        <Icon name={NAV_ICONS[id]} className={`size-[1.15rem] shrink-0 ${current ? "text-primary" : ""}`} />
                        <span className={props.rail ? "sr-only" : "truncate"}>{WORKSPACE_LABELS[id]}</span>
                        {badge && props.rail && badge.tone !== "neutral" && (
                          <span title={badge.title} className={`absolute top-1 right-2.5 size-2 rounded-full ${badge.tone === "bad" ? "bg-error" : "bg-warning"}`}>
                            <span className="sr-only">{badge.title}</span>
                          </span>
                        )}
                        {badge && !props.rail && (
                          <span title={badge.title} className={`ml-auto tabular rounded-full px-1.5 text-[0.7rem] leading-5 font-semibold ${BADGE_TONE[badge.tone]}`}>
                            {compactCount(badge.count)}
                          </span>
                        )}
                      </PlaceLink>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
        );
      })}
    </nav>
  );
}

/** The host at a glance, at the foot of the sidebar. */
export function HostCard(props: { live: LiveSnapshot }) {
  const metrics = props.live.metrics;
  const cpu = metrics?.cpu?.usagePercent ?? null;
  const memory = metrics?.memory?.usedPercent ?? null;
  const tone = cpu === null ? "neutral" : cpu >= 90 ? "bad" : cpu >= 75 ? "warn" : "ok";
  return (
    <div className="rounded-box border border-base-content/10 bg-base-100 p-3 text-xs">
      <div className="mb-2 flex items-center gap-2 font-semibold">
        <StatusDot tone={tone} />
        <span className="truncate">Host</span>
        {metrics?.cpu?.cores ? <span className="ml-auto tabular text-base-content/70">{metrics.cpu.cores} cores</span> : null}
      </div>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 tabular">
        <dt className="text-base-content/70">CPU</dt>
        <dd className="text-right">{formatPercent(cpu)}</dd>
        <dt className="text-base-content/70">Memory</dt>
        <dd className="text-right">
          {formatPercent(memory)}
          {metrics?.memory?.totalBytes ? <span className="text-base-content/60"> of {formatBytes(metrics.memory.totalBytes)}</span> : null}
        </dd>
        <dt className="text-base-content/70">Egress</dt>
        <dd className="text-right">{formatBps((metrics?.network?.uploadKbps ?? NaN) * 1000)}</dd>
        <dt className="text-base-content/70">Ingest</dt>
        <dd className="text-right">{formatBps((metrics?.network?.downloadKbps ?? NaN) * 1000)}</dd>
      </dl>
    </div>
  );
}

export function Sidebar(props: { live: LiveSnapshot; badges: Partial<Record<Workspace, NavBadge>> }) {
  return (
    <>
      <aside className="sticky top-0 hidden h-dvh w-60 shrink-0 flex-col gap-4 overflow-y-auto border-r border-base-content/10 bg-base-200 px-3 py-4 xl:flex">
        <Brand />
        <NavList badges={props.badges} />
        <div className="mt-auto">
          <HostCard live={props.live} />
        </div>
      </aside>
      <aside className="sticky top-0 hidden h-dvh w-16 shrink-0 flex-col items-stretch gap-4 overflow-y-auto border-r border-base-content/10 bg-base-200 px-2 py-4 md:flex xl:hidden">
        <Brand compact />
        <NavList badges={props.badges} rail />
      </aside>
    </>
  );
}
