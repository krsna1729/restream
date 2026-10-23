// Viewer preferences: theme, density and audience mode. They belong to this
// browser, not the engine, so they live in localStorage (wrapped: a blocked
// store leaves the defaults) and are applied as attributes on <html>, where
// the stylesheet reads them.

import { createContext, useContext, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";

import { resolveTheme } from "./theme.js";
import type { ThemeChoice } from "./theme.js";

export type { ThemeChoice };

export type Density = "comfortable" | "compact";
/** Operator: verdicts and large controls. Engineer: every counter, id and
 * raw payload. */
export type Mode = "operator" | "engineer";

export interface Prefs {
  readonly theme: ThemeChoice;
  readonly density: Density;
  readonly mode: Mode;
}

const KEYS = { theme: "restream.theme", density: "restream.density", mode: "restream.mode" };

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
    mode: read<Mode>(KEYS.mode, ["operator", "engineer"], "operator"),
  };
}

interface PrefsContextValue extends Prefs {
  readonly setTheme: (theme: ThemeChoice) => void;
  readonly setDensity: (density: Density) => void;
  readonly setMode: (mode: Mode) => void;
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
    root.dataset.mode = prefs.mode;
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
      setMode: (mode) => {
        write(KEYS.mode, mode);
        setPrefs((current) => ({ ...current, mode }));
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
  return { ...prefs, setTheme: () => {}, setDensity: () => {}, setMode: () => {} };
}

/** Whether engineer detail should render. */
export function useEngineer(): boolean {
  return usePrefs().mode === "engineer";
}

/** Renders its children only in engineer mode. */
export function EngineerOnly(props: { children: ReactNode }) {
  return useEngineer() ? <>{props.children}</> : null;
}
