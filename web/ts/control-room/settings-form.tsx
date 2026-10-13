// One Settings card: a form that saves its own part of the settings and
// says when the engine stored it.

import { useEffect, useState } from "react";
import type { ReactNode } from "react";

import type { ConfigPatch } from "../core/api.js";

/** Saves one part of the settings; true when the engine stored it. */
export type Saver = (patch: ConfigPatch) => Promise<boolean>;

export function SettingsForm(props: {
  id: string;
  title: string;
  onSave: () => Promise<boolean>;
  /** Why the form cannot be saved yet; the button stays disabled. */
  problem?: string | null;
  children: ReactNode;
}) {
  const [saved, setSaved] = useState(false);
  useEffect(() => {
    if (!saved) return;
    const timer = window.setTimeout(() => setSaved(false), 3000);
    return () => window.clearTimeout(timer);
  }, [saved]);
  return (
    <section className="card bg-base-200" aria-labelledby={props.id}>
      <form
        className="card-body gap-3 p-4"
        onSubmit={async (event) => {
          event.preventDefault();
          if (!props.problem) setSaved(await props.onSave());
        }}
      >
        <h3 id={props.id} className="card-title text-base">
          {props.title}
        </h3>
        {props.children}
        <div className="flex items-center gap-3">
          <button
            type="submit"
            className="btn btn-sm btn-primary"
            disabled={Boolean(props.problem)}
          >
            Save
          </button>
          {props.problem && <span className="text-sm text-warning">{props.problem}</span>}
          {saved && (
            <span role="status" className="text-sm text-success">
              saved
            </span>
          )}
        </div>
      </form>
    </section>
  );
}
