// Viewer preferences: theme and density, plus which sections show their
// detail. They belong to this
// browser, not the engine, so they live in localStorage (wrapped: a blocked
// store leaves the defaults) and are applied as attributes on <html>, where
// the stylesheet reads them.

import { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";

import { resolveTheme } from "./theme.js";
import type { ThemeChoice } from "./theme.js";

export type { ThemeChoice };

export type Density = "comfortable" | "compact";

export interface Prefs {
  readonly theme: ThemeChoice;
  readonly density: Density;
}

const KEYS = { theme: "restream.theme", density: "restream.density" };

function read<T extends string>(key: string, allowed: readonly T[], fallback: T): T {
  try {
    const value = window.localStorage.getItem(key);
    return value !== null && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;
  } catch {
    return fallback;
  }
}

function write(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    /* storage blocked: the choice lasts this visit */
  }
}

/** The stored preferences, or the defaults for anything unset. */
export function loadPrefs(): Prefs {
  return {
    theme: read<ThemeChoice>(KEYS.theme, ["system", "dark", "light"], "system"),
    density: read<Density>(KEYS.density, ["comfortable", "compact"], "comfortable"),
  };
}

interface PrefsContextValue extends Prefs {
  readonly setTheme: (theme: ThemeChoice) => void;
  readonly setDensity: (density: Density) => void;
}

const PrefsContext = createContext<PrefsContextValue | null>(null);

function systemPrefersDark(): boolean {
  return typeof window.matchMedia !== "function" || window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function PrefsProvider(props: { children: ReactNode }) {
  const [prefs, setPrefs] = useState<Prefs>(loadPrefs);
  const [systemDark, setSystemDark] = useState(systemPrefersDark);

  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const query = window.matchMedia("(prefers-color-scheme: dark)");
    const update = () => setSystemDark(query.matches);
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  }, []);

  useEffect(() => {
    const root = document.documentElement;
    root.dataset.theme = resolveTheme(prefs.theme, systemDark);
    root.dataset.density = prefs.density;
  }, [prefs, systemDark]);

  const value = useMemo<PrefsContextValue>(
    () => ({
      ...prefs,
      setTheme: (theme) => {
        write(KEYS.theme, theme);
        setPrefs((current) => ({ ...current, theme }));
      },
      setDensity: (density) => {
        write(KEYS.density, density);
        setPrefs((current) => ({ ...current, density }));
      },
    }),
    [prefs],
  );
  return <PrefsContext.Provider value={value}>{props.children}</PrefsContext.Provider>;
}

/** The preferences; outside a provider, the stored values and no setters. */
export function usePrefs(): PrefsContextValue {
  const value = useContext(PrefsContext);
  if (value) return value;
  const prefs = loadPrefs();
  return { ...prefs, setTheme: () => {}, setDensity: () => {} };
}

/**
 * Whether a section shows its detail (ids, counters, raw errors), and the
 * switch. Each section remembers its own choice in this browser, so a deeper
 * look at destinations does not crowd the rest of the console.
 */
export function useSectionDetail(section: string): readonly [boolean, (open: boolean) => void] {
  const key = `restream.detail.${section}`;
  const [open, setOpen] = useState(() => read(key, ["1", "0"], "0") === "1");
  const set = useCallback(
    (next: boolean) => {
      write(key, next ? "1" : "0");
      setOpen(next);
    },
    [key],
  );
  return [open, set] as const;
}

/** The header switch for a section's detail. */
export function DetailToggle(props: { open: boolean; onChange: (open: boolean) => void; label?: string }) {
  return (
    <button
      type="button"
      aria-pressed={props.open}
      onClick={() => props.onChange(!props.open)}
      title={props.open ? "Hide ids, counters and raw errors" : "Show ids, counters and raw errors"}
      className={`btn btn-sm ${props.open ? "btn-primary" : "btn-outline border-base-content/20"}`}
    >
      {props.label ?? "Detail"}
    </button>
  );
}
