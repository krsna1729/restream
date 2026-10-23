// One Settings panel: a form that saves its own part of the settings and
// says when the engine stored it.

import { useEffect, useState } from "react";
import type { ReactNode } from "react";

import type { ConfigPatch } from "../core/api.js";
import { Panel } from "../ui/panel.js";

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
    <Panel id={props.id} title={props.title}>
      <form
        className="flex flex-col gap-3"
        onSubmit={async (event) => {
          event.preventDefault();
          if (!props.problem) setSaved(await props.onSave());
        }}
      >
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
    </Panel>
  );
}
