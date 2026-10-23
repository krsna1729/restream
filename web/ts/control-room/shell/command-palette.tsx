// ⌘K / Ctrl+K (or `/`): jump to any workspace, pipeline, destination or
// incident, or change a viewing preference, from the keyboard.

import { useEffect, useMemo, useRef, useState } from "react";

import type { OperatorAlert } from "../../core/api-types.js";
import type { RuntimeState } from "../../core/generated/runtime-state.js";
import { Icon } from "../../ui/icons.js";
import type { IconName } from "../../ui/icons.js";
import { usePrefs } from "../../ui/prefs.js";
import type { Density, Mode, ThemeChoice } from "../../ui/prefs.js";
import { useGo, usePlace } from "../router.js";
import { paletteEntries, rank } from "./palette-model.js";
import type { PaletteEntry, PaletteKind } from "./palette-model.js";

const KIND_ICON: Record<PaletteKind, IconName> = {
  workspace: "chevron",
  pipeline: "pipelines",
  destination: "channels",
  incident: "incidents",
  action: "settings",
};

export function useCommandPaletteShortcut(open: () => void): void {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const typing = target && (target.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName));
      if ((event.key === "k" && (event.metaKey || event.ctrlKey)) || (event.key === "/" && !typing)) {
        event.preventDefault();
        open();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);
}

export function CommandPalette(props: {
  open: boolean;
  onClose: () => void;
  state: RuntimeState | null;
  alerts: readonly OperatorAlert[];
}) {
  const dialog = useRef<HTMLDialogElement | null>(null);
  const input = useRef<HTMLInputElement | null>(null);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const go = useGo();
  const place = usePlace();
  const prefs = usePrefs();

  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    if (props.open && !element.open) {
      setQuery("");
      setActive(0);
      element.showModal();
      window.setTimeout(() => input.current?.focus(), 0);
    } else if (!props.open && element.open) {
      element.close();
    }
  }, [props.open]);

  const entries = useMemo(
    () => (props.open ? paletteEntries(props.state, props.alerts, place.pipelineId) : []),
    [props.open, props.state, props.alerts, place.pipelineId],
  );
  const results = useMemo(() => rank(entries, query), [entries, query]);

  const choose = (entry: PaletteEntry | undefined) => {
    if (!entry) return;
    if (entry.place) go(entry.place);
    if (entry.action) {
      const [kind, value] = entry.action.split(":");
      if (kind === "theme") prefs.setTheme(value as ThemeChoice);
      if (kind === "mode") prefs.setMode(value as Mode);
      if (kind === "density") prefs.setDensity(value as Density);
    }
    props.onClose();
  };

  return (
    <dialog
      ref={dialog}
      className="modal modal-top sm:modal-middle"
      onClose={props.onClose}
      aria-label="Command palette"
    >
      <div className="modal-box max-h-[80dvh] w-full max-w-2xl overflow-hidden p-0 sm:mt-0">
        <div className="flex items-center gap-3 border-b border-base-content/10 px-4">
          <Icon name="search" className="size-5 text-base-content/60" />
          <input
            ref={input}
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setActive(0);
            }}
            onKeyDown={(event) => {
              if (event.key === "ArrowDown") {
                event.preventDefault();
                setActive((index) => Math.min(results.length - 1, index + 1));
              } else if (event.key === "ArrowUp") {
                event.preventDefault();
                setActive((index) => Math.max(0, index - 1));
              } else if (event.key === "Enter") {
                event.preventDefault();
                choose(results[active]);
              }
            }}
            placeholder="Search pipelines, destinations, incidents, settings…"
            aria-label="Search"
            aria-controls="palette-results"
            aria-activedescendant={results[active] ? `palette-${results[active].id}` : undefined}
            className="h-14 w-full bg-transparent text-base outline-none"
          />
          <kbd className="kbd kbd-sm hidden sm:inline-flex">esc</kbd>
        </div>
        <ul id="palette-results" role="listbox" className="scrollbar-thin max-h-[60dvh] overflow-y-auto p-2">
          {results.length === 0 && <li className="px-3 py-6 text-center text-sm text-base-content/70">No match.</li>}
          {results.map((entry, index) => (
            <li
              key={entry.id}
              id={`palette-${entry.id}`}
              role="option"
              aria-selected={index === active}
              onMouseEnter={() => setActive(index)}
              onClick={() => choose(entry)}
              className={`flex min-h-11 cursor-pointer items-center gap-3 rounded-field px-3 py-2 ${index === active ? "bg-primary/15" : ""}`}
            >
              <Icon name={KIND_ICON[entry.kind]} className="size-4 shrink-0 text-base-content/60" />
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-medium">{entry.label}</div>
                <div className="truncate text-xs text-base-content/70">{entry.detail}</div>
              </div>
              <span className="kicker shrink-0">{entry.kind}</span>
            </li>
          ))}
        </ul>
      </div>
      <form method="dialog" className="modal-backdrop">
        <button type="submit" aria-label="Close search">close</button>
      </form>
    </dialog>
  );
}
