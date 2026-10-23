// The console's surfaces. A Panel is a titled section on the page surface;
// its header holds the title, a short subtitle, the age of its data and its
// actions, and wraps on narrow screens. Panels do not nest.

import type { ReactNode } from "react";

export function Panel(props: {
  title?: ReactNode;
  subtitle?: ReactNode;
  /** Right side of the header: freshness, filters, buttons. */
  actions?: ReactNode;
  children: ReactNode;
  /** No body padding: tables and canvases run edge to edge. */
  flush?: boolean;
  className?: string;
  id?: string;
  /** Tone stripe on the left edge for a panel that is itself a status. */
  stripe?: "ok" | "warn" | "bad" | "info";
}) {
  const stripe = props.stripe
    ? { ok: "border-l-success", warn: "border-l-warning", bad: "border-l-error", info: "border-l-info" }[props.stripe]
    : "";
  // The heading carries the id, and the section is labelled by it.
  const labelled = props.title && props.id ? props.id : undefined;
  return (
    <section
      aria-labelledby={labelled}
      className={`min-w-0 rounded-box border border-base-content/10 bg-base-200 ${props.stripe ? `border-l-4 ${stripe}` : ""} ${props.className ?? ""}`}
    >
      {(props.title || props.actions) && (
        <header className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-base-content/10 px-4 py-2.5">
          <div className="flex min-w-0 flex-1 items-baseline gap-2">
            {props.title && (
              // Long names (ids, keys) wrap anywhere rather than widen the page.
              <h3 id={labelled} className="min-w-0 text-[0.95rem] font-semibold [overflow-wrap:anywhere]">
                {props.title}
              </h3>
            )}
            {props.subtitle && <span className="hidden min-w-0 shrink-[2] truncate text-xs text-base-content/70 sm:inline">{props.subtitle}</span>}
          </div>
          {props.actions && <div className="flex flex-wrap items-center gap-2">{props.actions}</div>}
        </header>
      )}
      <div className={props.flush ? "" : "p-4"}>{props.children}</div>
    </section>
  );
}

/** A quiet in-panel block (a row, a card in a grid). */
export function Tile(props: { children: ReactNode; className?: string; onClick?: () => void; selected?: boolean }) {
  const interactive = props.onClick !== undefined;
  const Tag = interactive ? "button" : "div";
  return (
    <Tag
      type={interactive ? "button" : undefined}
      onClick={props.onClick}
      aria-pressed={interactive ? !!props.selected : undefined}
      className={`min-w-0 rounded-box border bg-base-100 p-3 text-left ${props.selected ? "border-primary" : "border-base-content/10"} ${interactive ? "transition-colors hover:border-base-content/30" : ""} ${props.className ?? ""}`}
    >
      {props.children}
    </Tag>
  );
}

/** What a panel shows when it has nothing: says why, and what to do. */
export function EmptyState(props: { title: string; detail?: ReactNode; action?: ReactNode; tone?: "neutral" | "ok" }) {
  return (
    <div className="flex flex-col items-center gap-2 px-4 py-8 text-center">
      <p className={`font-semibold ${props.tone === "ok" ? "text-success" : ""}`}>{props.title}</p>
      {props.detail && <p className="max-w-md text-sm text-base-content/70">{props.detail}</p>}
      {props.action}
    </div>
  );
}
