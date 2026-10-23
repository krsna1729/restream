// The top of every workspace: a breadcrumb eyebrow, a headline that states
// the judgement ("2 destinations need attention"), a supporting line, and
// the workspace's actions. It stacks on narrow screens.

import type { ReactNode } from "react";

import { TONE_TEXT } from "./tone.js";
import type { Tone } from "./tone.js";

export function PageHeader(props: {
  eyebrow?: ReactNode;
  title: ReactNode;
  /** Colours the title when it is itself a verdict. */
  tone?: Tone;
  subtitle?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <header data-tour="page-header" className="flex flex-col gap-3 md:flex-row md:items-end md:justify-between">
      <div className="min-w-0">
        {props.eyebrow && <div className="kicker mb-1 truncate text-primary">{props.eyebrow}</div>}
        <h2 className={`text-xl leading-tight font-semibold text-balance ${props.tone ? TONE_TEXT[props.tone] : ""}`}>
          {props.title}
        </h2>
        {props.subtitle && <p className="mt-1 text-sm text-base-content/70">{props.subtitle}</p>}
      </div>
      {props.actions && <div className="flex flex-wrap items-center gap-2">{props.actions}</div>}
    </header>
  );
}
