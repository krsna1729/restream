// Shared filter controls: a search field, single-choice chips that scroll
// sideways on a phone instead of wrapping into a wall, and a count chip.

import type { ReactNode } from "react";

import { Icon } from "./icons.js";

export function SearchField(props: { value: string; onChange: (value: string) => void; placeholder: string; label: string; className?: string }) {
  return (
    <label className={`input input-sm touch-36 flex h-9 min-w-0 items-center gap-2 ${props.className ?? ""}`}>
      <Icon name="search" className="size-4 shrink-0 opacity-60" />
      <input
        type="search"
        className="min-w-0 grow self-stretch"
        placeholder={props.placeholder}
        aria-label={props.label}
        value={props.value}
        onChange={(event) => props.onChange(event.target.value)}
      />
    </label>
  );
}

export interface Chip<T extends string> {
  readonly value: T;
  readonly label: ReactNode;
  readonly count?: number;
  /** Draws attention to a chip whose set is non-empty (degraded). */
  readonly alert?: boolean;
}

export function ChipGroup<T extends string>(props: { label: string; value: T; chips: readonly Chip<T>[]; onChange: (value: T) => void }) {
  return (
    <div role="group" aria-label={props.label} className="scrollbar-thin -mx-1 flex max-w-full gap-1.5 overflow-x-auto px-1 py-0.5 md:flex-wrap md:overflow-visible">
      {props.chips.map((chip) => {
        const active = chip.value === props.value;
        return (
          <button
            key={chip.value}
            type="button"
            aria-pressed={active}
            onClick={() => props.onChange(chip.value)}
            className={`touch-36 inline-flex h-8 shrink-0 items-center gap-1.5 rounded-full border px-3 text-sm whitespace-nowrap transition-colors ${active ? "border-primary bg-primary/15 font-semibold" : "border-base-content/15 hover:border-base-content/35"}`}
          >
            {chip.label}
            {chip.count !== undefined && (
              <span className={`tabular rounded-full px-1.5 text-xs ${chip.alert && chip.count > 0 ? "bg-warning text-warning-content" : "bg-base-content/10"}`}>
                {chip.count.toLocaleString()}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}

/** A horizontal bar of shares: one segment per item, labelled below. */
export function ShareBar(props: { label: string; items: ReadonlyArray<{ key: string; label: string; value: number; color: string }> }) {
  const total = props.items.reduce((sum, item) => sum + item.value, 0);
  if (total === 0) return <p className="text-sm text-base-content/70">None.</p>;
  return (
    <div>
      <div role="img" aria-label={props.label} className="flex h-2.5 w-full overflow-hidden rounded-full bg-base-content/10">
        {props.items.map((item) => (
          <span key={item.key} className={`h-full ${item.color}`} style={{ width: `${(100 * item.value) / total}%` }} />
        ))}
      </div>
      <ul className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-sm sm:grid-cols-3">
        {props.items.map((item) => (
          <li key={item.key} className="flex min-w-0 items-center gap-2">
            <span aria-hidden="true" className={`size-2.5 shrink-0 rounded-sm ${item.color}`} />
            <span className="truncate">{item.label}</span>
            <span className="ml-auto tabular text-base-content/70">{item.value.toLocaleString()}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
