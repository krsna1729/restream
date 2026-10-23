// The sticky bar over every workspace: search, the stream's liveness and
// sample age, the clock, and the viewer's preferences. On a phone it keeps
// the brand, the liveness dot and search; the rest moves to More.

import { useEffect, useRef } from "react";
import type { ReactNode } from "react";

import { Icon } from "../../ui/icons.js";
import { usePrefs } from "../../ui/prefs.js";
import type { Density, Mode, ThemeChoice } from "../../ui/prefs.js";
import { StatusDot } from "../../ui/tone.js";
import { formatAge } from "../format.js";
import { SAMPLE_STALE_AFTER_MS, useNow } from "../store.js";
import type { LiveSnapshot } from "../store.js";
import type { Role } from "../../core/api-types.js";
import { useSession } from "../session.js";
import { ROLE_LABEL, ROLE_SUMMARY, viewAsChoices } from "../team-model.js";
import { Brand } from "./sidebar.js";

/** The live pill: stream state, revision and the age of the oldest sample. */
export function LiveStatus(props: { live: LiveSnapshot; compact?: boolean }) {
  const now = useNow(1000);
  const { live } = props;
  const sampledAtMs =
    live.telemetryAtMs === null || live.metricsAtMs === null ? null : Math.min(live.telemetryAtMs, live.metricsAtMs);
  const stale = live.stream !== "live" || sampledAtMs === null || now - sampledAtMs > SAMPLE_STALE_AFTER_MS;
  const tone = live.stream !== "live" ? "bad" : stale ? "warn" : "ok";
  const label =
    live.stream === "live" ? `live · rev ${live.state?.revision ?? "--"}` : live.stream;
  return (
    <span
      role="status"
      title="Runtime state stream and the age of the oldest sampled source"
      className={`inline-flex items-center gap-2 rounded-full border border-base-content/10 bg-base-100 px-2.5 py-1 text-xs tabular ${tone === "ok" ? "" : tone === "warn" ? "text-warning" : "text-error"}`}
    >
      <StatusDot tone={tone} pulse={tone === "ok"} />
      {props.compact ? (
        <span className="sr-only">
          {label} · sample {formatAge(sampledAtMs, now)}
        </span>
      ) : (
        <span>
          {label} · sample {formatAge(sampledAtMs, now)}
        </span>
      )}
    </span>
  );
}

function UtcClock() {
  const now = useNow(1000);
  return (
    <time className="hidden text-xs text-base-content/70 tabular 2xl:inline" dateTime={new Date(now).toISOString()}>
      {new Date(now).toISOString().slice(11, 19)} UTC
    </time>
  );
}

function Segmented<T extends string>(props: { label: string; value: T; options: ReadonlyArray<[T, string]>; onChange: (value: T) => void }) {
  return (
    <fieldset className="flex flex-col gap-1">
      <legend className="kicker mb-1">{props.label}</legend>
      <div className="join w-full">
        {props.options.map(([value, text]) => (
          <button
            key={value}
            type="button"
            aria-pressed={props.value === value}
            onClick={() => props.onChange(value)}
            className={`btn join-item btn-sm flex-1 ${props.value === value ? "btn-primary" : "btn-ghost border-base-content/15"}`}
          >
            {text}
          </button>
        ))}
      </div>
    </fieldset>
  );
}

/** Theme, density and audience, as segmented controls. */
export function PrefsControls() {
  const prefs = usePrefs();
  return (
    <div className="flex flex-col gap-3">
      <Segmented<Mode> label="Audience" value={prefs.mode} options={[["operator", "Operator"], ["engineer", "Engineer"]]} onChange={prefs.setMode} />
      <Segmented<ThemeChoice> label="Theme" value={prefs.theme} options={[["system", "System"], ["dark", "Dark"], ["light", "Light"]]} onChange={prefs.setTheme} />
      <Segmented<Density> label="Density" value={prefs.density} options={[["comfortable", "Comfortable"], ["compact", "Compact"]]} onChange={prefs.setDensity} />
    </div>
  );
}

/** A dropdown that closes on an outside click. */
function useDropdown() {
  const details = useRef<HTMLDetailsElement | null>(null);
  useEffect(() => {
    const close = (event: MouseEvent) => {
      if (details.current?.open && !details.current.contains(event.target as Node)) details.current.open = false;
    };
    document.addEventListener("click", close);
    return () => document.removeEventListener("click", close);
  }, []);
  return details;
}

/** Who is signed in, the role they act as, and viewing as a lower one. */
export function SessionControls(props: { onDone?: () => void }) {
  const session = useSession();
  const principal = session.principal;
  if (!principal) return null;
  const own = principal.role;
  const choices: Role[] = [own, ...viewAsChoices(own).reverse()];
  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-3">
        <span className="grid size-9 place-items-center rounded-full bg-primary/15 text-primary">
          <Icon name="user" className="size-5" />
        </span>
        <div className="min-w-0">
          <div className="truncate font-semibold">{principal.username}</div>
          <div className="text-xs text-base-content/70">{ROLE_LABEL[own]}</div>
        </div>
      </div>
      {choices.length > 1 && (
        <Segmented<Role>
          label="View as"
          value={principal.effectiveRole}
          options={choices.map((role): [Role, string] => [role, role === own ? `Me (${ROLE_LABEL[role]})` : ROLE_LABEL[role]])}
          onChange={(role) => {
            props.onDone?.();
            void session.viewAs(role === own ? null : role);
          }}
        />
      )}
      <p className="text-xs text-base-content/70">{ROLE_SUMMARY[principal.effectiveRole]}</p>
      <button type="button" className="btn btn-ghost btn-sm justify-start border-base-content/15" onClick={() => void session.signOut()}>
        Sign out
      </button>
    </div>
  );
}

function UserMenu() {
  const details = useDropdown();
  const session = useSession();
  if (!session.principal) return null;
  const viewing = session.principal.viewAs !== null;
  return (
    <details ref={details} className="dropdown dropdown-end">
      <summary
        className={`btn btn-ghost btn-sm gap-2 ${viewing ? "text-warning" : ""}`}
        aria-label={`Account: ${session.principal.username}, acting as ${ROLE_LABEL[session.role]}`}
      >
        <Icon name={viewing ? "eye" : "user"} className="size-[1.15rem]" />
        <span className="hidden max-w-32 truncate lg:inline">{session.principal.username}</span>
      </summary>
      <div className="dropdown-content z-30 mt-2 w-72 rounded-box border border-base-content/10 bg-base-200 p-4 shadow-xl">
        <SessionControls
          onDone={() => {
            if (details.current) details.current.open = false;
          }}
        />
      </div>
    </details>
  );
}

/** While a member views the dashboard as a lower role: say so, and how to stop. */
export function ViewAsBanner() {
  const session = useSession();
  const view = session.principal?.viewAs;
  if (!session.principal || !view) return null;
  return (
    <div role="status" className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-warning/30 bg-warning/15 px-3 py-2 text-sm md:px-5">
      <Icon name="eye" className="size-4 shrink-0 text-warning" />
      <span className="min-w-0 flex-1">
        Viewing as <strong>{ROLE_LABEL[view]}</strong>. The engine enforces it: what this role cannot do is refused, not just hidden.
      </span>
      <button type="button" className="btn btn-warning btn-xs" onClick={() => void session.viewAs(null)}>
        Back to {ROLE_LABEL[session.principal.role]}
      </button>
    </div>
  );
}

function PrefsMenu() {
  const details = useDropdown();
  return (
    <details ref={details} className="dropdown dropdown-end">
      <summary className="btn btn-ghost btn-sm btn-square" aria-label="View preferences">
        <Icon name="settings" className="size-[1.15rem]" />
      </summary>
      <div className="dropdown-content z-30 mt-2 w-72 rounded-box border border-base-content/10 bg-base-200 p-4 shadow-xl">
        <PrefsControls />
      </div>
    </details>
  );
}

function ModeToggle() {
  const prefs = usePrefs();
  return (
    <div className="join hidden md:inline-flex" role="group" aria-label="Audience">
      {(["operator", "engineer"] as const).map((mode) => (
        <button
          key={mode}
          type="button"
          aria-pressed={prefs.mode === mode}
          onClick={() => prefs.setMode(mode)}
          className={`btn join-item btn-xs h-8 px-3 ${prefs.mode === mode ? "btn-primary" : "btn-ghost border-base-content/15"}`}
        >
          {mode === "operator" ? "Operator" : "Engineer"}
        </button>
      ))}
    </div>
  );
}

export function Topbar(props: { live: LiveSnapshot; onSearch: () => void; extra?: ReactNode }) {
  return (
    <header className="sticky top-0 z-20 flex h-14 items-center gap-3 border-b border-base-content/10 bg-base-300/90 px-3 backdrop-blur md:px-5">
      <div className="md:hidden">
        <Brand compact />
      </div>
      <button
        type="button"
        aria-label="Search and commands"
        onClick={props.onSearch}
        className="touch-36 flex h-9 min-w-0 flex-1 items-center gap-2 rounded-field border border-base-content/10 bg-base-100 px-3 text-left text-sm text-base-content/70 transition-colors hover:border-base-content/30 md:max-w-md"
      >
        <Icon name="search" className="size-4 shrink-0" />
        <span className="truncate">Search pipelines, destinations, incidents…</span>
        <kbd className="kbd kbd-xs ml-auto hidden sm:inline-flex">⌘K</kbd>
      </button>
      <div className="ml-auto flex items-center gap-2">
        {props.extra}
        <span className="hidden md:inline-flex">
          <LiveStatus live={props.live} />
        </span>
        <span className="md:hidden">
          <LiveStatus live={props.live} compact />
        </span>
        <UtcClock />
        <ModeToggle />
        <span className="hidden md:inline-flex">
          <PrefsMenu />
        </span>
        <span className="hidden md:inline-flex">
          <UserMenu />
        </span>
      </div>
    </header>
  );
}
