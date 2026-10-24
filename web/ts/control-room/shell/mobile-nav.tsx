// The phone's navigation: a bottom dock with the on-call path (Fleet,
// Incidents, Pipelines, Operate) and More, a sheet holding every workspace,
// the host card and the viewing preferences.

import { useEffect, useRef } from "react";

import { Icon } from "../../ui/icons.js";
import { WORKSPACE_LABELS } from "../route.js";
import type { Workspace } from "../route.js";
import { PlaceLink, usePlace } from "../router.js";
import type { LiveSnapshot } from "../store.js";
import { compactCount } from "./badges.js";
import type { NavBadge } from "./badges.js";
import { MOBILE_TABS, NAV_ICONS } from "./nav.js";
import { HostCard, NavList } from "./sidebar.js";
import { PrefsControls, SessionControls } from "./topbar.js";

export function MobileTabs(props: {
  badges: Partial<Record<Workspace, NavBadge>>;
  onMore: () => void;
  moreOpen: boolean;
}) {
  const place = usePlace();
  const inMore = !MOBILE_TABS.includes(place.workspace);
  return (
    <nav
      aria-label="Primary"
      className="dock z-30 border-t border-base-content/10 bg-base-200/95 backdrop-blur md:hidden"
    >
      {MOBILE_TABS.map((id) => {
        const current = id === place.workspace;
        const badge = props.badges[id];
        return (
          <PlaceLink
            key={id}
            place={{ workspace: id, pipelineId: place.pipelineId }}
            current={current}
            className={current ? "dock-active text-primary" : "text-base-content/75"}
          >
            <Icon name={NAV_ICONS[id]} className="size-5" />
            <span className="dock-label">{WORKSPACE_LABELS[id]}</span>
            {badge && badge.tone !== "neutral" && (
              <span className={`absolute top-1.5 left-1/2 ml-1.5 rounded-full px-1 text-[0.65rem] leading-4 font-semibold ${badge.tone === "bad" ? "bg-error text-error-content" : "bg-warning text-warning-content"}`}>
                {compactCount(badge.count)}
              </span>
            )}
          </PlaceLink>
        );
      })}
      <button
        type="button"
        onClick={props.onMore}
        aria-expanded={props.moreOpen}
        className={inMore ? "dock-active text-primary" : "text-base-content/75"}
      >
        <Icon name="menu" className="size-5" />
        <span className="dock-label">{inMore ? WORKSPACE_LABELS[place.workspace] : "More"}</span>
      </button>
    </nav>
  );
}

export function MoreSheet(props: {
  open: boolean;
  onClose: () => void;
  live: LiveSnapshot;
  badges: Partial<Record<Workspace, NavBadge>>;
}) {
  const dialog = useRef<HTMLDialogElement | null>(null);
  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    if (props.open && !element.open) element.showModal();
    if (!props.open && element.open) element.close();
  }, [props.open]);
  return (
    <dialog ref={dialog} className="modal modal-bottom" onClose={props.onClose} aria-label="All workspaces">
      <div className="modal-box max-h-[85dvh] overflow-y-auto pb-[calc(1rem+env(safe-area-inset-bottom))]">
        <div className="mb-3 flex items-center justify-between">
          <h2 className="font-semibold">Workspaces</h2>
          <button type="button" className="btn btn-ghost btn-sm btn-square" onClick={props.onClose} aria-label="Close">
            <Icon name="close" className="size-5" />
          </button>
        </div>
        <NavList badges={props.badges} onNavigate={props.onClose} />
        <div className="mt-4 flex flex-col gap-4">
          <SessionControls onDone={props.onClose} />
          <HostCard live={props.live} />
          <PrefsControls />
        </div>
      </div>
      <form method="dialog" className="modal-backdrop">
        <button type="submit" aria-label="Close workspaces">close</button>
      </form>
    </dialog>
  );
}
