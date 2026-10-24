// Shared filter controls: a search field, single-choice chips that scroll
// sideways on a phone instead of wrapping into a wall, and a count chip.

import { useId, useRef, useState } from "react";
import type { InputHTMLAttributes, ReactNode } from "react";

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
    // A few chips wrap; a long set scrolls sideways on a phone and wraps
    // from md up. (A near-fit scroller grows a hairline scrollbar.)
    <div
      role="group"
      aria-label={props.label}
      className={`-mx-1 flex max-w-full flex-wrap gap-1.5 px-1 py-0.5 ${props.chips.length > 4 ? "scrollbar-thin max-md:flex-nowrap max-md:overflow-x-auto" : ""}`}
    >
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

/** Copies `text`; where the clipboard is refused, shows it to copy by hand. */
export function CopyButton(props: { text: string; label: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="btn btn-ghost btn-xs"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(props.text);
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1500);
        } catch {
          window.prompt("Copy this:", props.text);
        }
      }}
    >
      {copied ? "Copied" : props.label}
    </button>
  );
}

/** A labelled control: its caption over it (and an optional hint under
 * it), the whole a `<label>` so the caption names the control. `invalid`
 * says what a valid value is; it shows, in the error colour, once the
 * person has left the control (`:user-invalid`) with a value its
 * constraints refuse. Give the control the `validator` class. */
export function Field(props: { label: ReactNode; hint?: ReactNode; invalid?: ReactNode; className?: string; children: ReactNode }) {
  return (
    <label className={`flex min-w-0 flex-col justify-end gap-1 [&>:is(input,select,textarea,.join,.input)]:w-full ${props.className ?? ""}`}>
      <span className="text-[0.8rem] font-medium text-base-content/80">{props.label}</span>
      {props.children}
      {props.invalid && <span className="validator-hint mt-0 hidden text-xs">{props.invalid}</span>}
      {props.hint && <span className="text-xs text-base-content/65">{props.hint}</span>}
    </label>
  );
}

/** A text box with its unit inside it, at the end ("ms", "kbps"). */
export function UnitInput(props: Omit<InputHTMLAttributes<HTMLInputElement>, "size"> & { unit: string; small?: boolean }) {
  const { unit, small, className, ...input } = props;
  return (
    <div className={`input validator ${small ? "input-sm" : ""} ${className ?? ""}`}>
      <input {...input} className="grow tabular" />
      <span className="text-xs text-base-content/70">{unit}</span>
    </div>
  );
}

/** A button that opens a panel beside it: the browser's popover (top
 * layer, light dismiss, Escape) placed by CSS anchor positioning, with a
 * fixed corner where anchoring is unsupported. `children` gets `close`. */
export function MenuButton(props: {
  label: string;
  className: string;
  button: ReactNode;
  width?: string;
  /** Runs as the panel opens, e.g. to focus a search box in it. */
  onOpen?: () => void;
  children: (close: () => void) => ReactNode;
}) {
  const id = useId();
  const panel = useRef<HTMLDivElement | null>(null);
  const anchor = `--menu-${id.replace(/[^a-zA-Z0-9]/g, "")}`;
  return (
    <>
      <button type="button" popoverTarget={id} aria-label={props.label} className={props.className} style={{ anchorName: anchor }}>
        {props.button}
      </button>
      <div
        ref={panel}
        id={id}
        popover="auto"
        style={{ positionAnchor: anchor }}
        onToggle={(event) => {
          if ((event.nativeEvent as ToggleEvent).newState === "open") props.onOpen?.();
        }}
        className={`menu-pop rounded-box border border-base-content/10 bg-base-200 p-4 text-base-content shadow-xl ${props.width ?? "w-72"}`}
      >
        {props.children(() => panel.current?.hidePopover())}
      </div>
    </>
  );
}
