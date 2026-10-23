// The console's one table. Columns carry a priority, so a narrow screen
// drops the least important ones; on a phone every row becomes a card
// (title, status, then the remaining fields as labelled values). Long lists
// are windowed: only the rows in view are in the DOM, so ten thousand
// destinations scroll as smoothly as ten.

import { useMemo, useState } from "react";
import type { ReactNode } from "react";

import { usePhone, useWindowedRows } from "./hooks.js";

export interface Column<T> {
  readonly id: string;
  readonly header: ReactNode;
  readonly cell: (row: T) => ReactNode;
  /** A CSS grid track: `minmax(10rem,2fr)`, `6rem`, `auto`. */
  readonly width?: string;
  readonly align?: "left" | "right" | "center";
  /** 1: always; 2: from `md`; 3: from `xl`. Phone cards show 1 and 2. */
  readonly priority?: 1 | 2 | 3;
  /** Present: the header sorts by it. */
  readonly sort?: (row: T) => number | string | null;
  /** On phone cards, render the cell without its header label. */
  readonly bare?: boolean;
}

const HIDE: Record<1 | 2 | 3, string> = { 1: "", 2: "hidden md:block", 3: "hidden xl:block" };

/** Above this many rows the table scrolls inside itself and is windowed. */
const WINDOW_AFTER = 80;

export function DataTable<T>(props: {
  rows: readonly T[];
  columns: readonly Column<T>[];
  rowKey: (row: T) => string;
  /** Row height in px (desktop). Cells truncate to keep it. */
  rowHeight?: number;
  /** Phone card height in px. */
  cardHeight?: number;
  /** Max height of the scrolling body once windowed, in px. */
  maxHeight?: number;
  onRowClick?: (row: T) => void;
  selectedKey?: string | null;
  /** Card title (phone); defaults to the first column's cell. */
  cardTitle?: (row: T) => ReactNode;
  /** Card status (phone, right of the title). */
  cardStatus?: (row: T) => ReactNode;
  empty?: ReactNode;
  label: string;
  initialSort?: { id: string; dir: "asc" | "desc" };
}) {
  const phone = usePhone();
  const [sort, setSort] = useState(props.initialSort ?? null);
  const rows = useMemo(() => {
    if (!sort) return props.rows;
    const column = props.columns.find((candidate) => candidate.id === sort.id);
    if (!column?.sort) return props.rows;
    const pick = column.sort;
    const sign = sort.dir === "asc" ? 1 : -1;
    return [...props.rows].sort((a, b) => {
      const x = pick(a);
      const y = pick(b);
      if (x === y) return 0;
      if (x === null) return 1;
      if (y === null) return -1;
      return x < y ? -sign : sign;
    });
  }, [props.rows, props.columns, sort]);

  const rowHeight = phone ? (props.cardHeight ?? 112) : (props.rowHeight ?? 40);
  const windowed = rows.length > WINDOW_AFTER;
  const view = useWindowedRows(windowed ? rows.length : 0, rowHeight);
  const slice = windowed ? rows.slice(view.first, view.last) : rows;

  if (rows.length === 0) {
    return <div className="px-4 py-6 text-center text-sm text-base-content/70">{props.empty ?? "Nothing to show."}</div>;
  }

  const visible = props.columns;
  const template = visible.map((column) => column.width ?? "minmax(0,1fr)").join(" ");
  const gridStyle = { gridTemplateColumns: template };

  const body = phone ? (
    <ul className="flex flex-col gap-2 p-2">
      {windowed && <li aria-hidden="true" style={{ height: view.before }} />}
      {slice.map((row) => {
        const key = props.rowKey(row);
        const selected = props.selectedKey === key;
        const details = visible.slice(1).filter((column) => (column.priority ?? 1) <= 2);
        return (
          <li key={key} style={windowed ? { height: rowHeight - 8 } : undefined}>
            <div
              role={props.onRowClick ? "button" : undefined}
              tabIndex={props.onRowClick ? 0 : undefined}
              onClick={props.onRowClick ? () => props.onRowClick?.(row) : undefined}
              onKeyDown={props.onRowClick ? (event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); props.onRowClick?.(row); } } : undefined}
              className={`flex h-full min-h-11 flex-col gap-1.5 overflow-hidden rounded-box border bg-base-100 p-3 ${selected ? "border-primary" : "border-base-content/10"} ${props.onRowClick ? "cursor-pointer active:bg-base-content/5" : ""}`}
            >
              <div className="flex min-w-0 items-start justify-between gap-2">
                <div className="min-w-0 truncate font-semibold">{(props.cardTitle ?? visible[0].cell)(row)}</div>
                {props.cardStatus && <div className="shrink-0">{props.cardStatus(row)}</div>}
              </div>
              <dl className="grid grid-cols-2 gap-x-3 gap-y-1 text-xs">
                {details.map((column) =>
                  column.bare ? (
                    <dd key={column.id} className="col-span-2 min-w-0 truncate">{column.cell(row)}</dd>
                  ) : (
                    <div key={column.id} className="min-w-0">
                      <dt className="kicker truncate">{column.header}</dt>
                      <dd className="tabular truncate">{column.cell(row)}</dd>
                    </div>
                  ),
                )}
              </dl>
            </div>
          </li>
        );
      })}
      {windowed && <li aria-hidden="true" style={{ height: view.after }} />}
    </ul>
  ) : (
    <div role="rowgroup">
      {windowed && <div aria-hidden="true" style={{ height: view.before }} />}
      {slice.map((row) => {
        const key = props.rowKey(row);
        const selected = props.selectedKey === key;
        return (
          <div
            key={key}
            role="row"
            aria-selected={props.onRowClick ? selected : undefined}
            tabIndex={props.onRowClick ? 0 : undefined}
            onClick={props.onRowClick ? () => props.onRowClick?.(row) : undefined}
            onKeyDown={props.onRowClick ? (event) => { if (event.key === "Enter") props.onRowClick?.(row); } : undefined}
            style={{ ...gridStyle, height: rowHeight }}
            className={`grid items-center gap-x-3 border-b border-base-content/5 px-4 text-sm ${selected ? "bg-primary/10" : "hover:bg-base-content/5"} ${props.onRowClick ? "cursor-pointer" : ""}`}
          >
            {visible.map((column) => (
              <div
                key={column.id}
                role="cell"
                className={`min-w-0 truncate ${HIDE[column.priority ?? 1]} ${column.align === "right" ? "tabular text-right" : column.align === "center" ? "text-center" : ""}`}
              >
                {column.cell(row)}
              </div>
            ))}
          </div>
        );
      })}
      {windowed && <div aria-hidden="true" style={{ height: view.after }} />}
    </div>
  );

  return (
    <div role="table" aria-label={props.label} aria-rowcount={rows.length} className="min-w-0">
      {!phone && (
        <div role="rowgroup" className="sticky top-0 z-[1]">
          <div role="row" style={gridStyle} className="grid items-center gap-x-3 border-b border-base-content/10 bg-base-200 px-4 py-2">
            {visible.map((column) => {
              const active = sort?.id === column.id;
              return (
                <div
                  key={column.id}
                  role="columnheader"
                  aria-sort={active ? (sort.dir === "asc" ? "ascending" : "descending") : undefined}
                  className={`kicker min-w-0 truncate ${HIDE[column.priority ?? 1]} ${column.align === "right" ? "text-right" : ""}`}
                >
                  {column.sort ? (
                    <button
                      type="button"
                      className="kicker inline-flex max-w-full items-center gap-1 truncate hover:text-base-content"
                      onClick={() =>
                        setSort((current) =>
                          current?.id === column.id
                            ? { id: column.id, dir: current.dir === "asc" ? "desc" : "asc" }
                            : { id: column.id, dir: "desc" },
                        )
                      }
                    >
                      {column.header}
                      {active && <span aria-hidden="true">{sort.dir === "asc" ? "↑" : "↓"}</span>}
                    </button>
                  ) : (
                    column.header
                  )}
                </div>
              );
            })}
          </div>
        </div>
      )}
      {windowed ? (
        <div ref={view.ref} className="scrollbar-thin overflow-y-auto overscroll-contain" style={{ maxHeight: props.maxHeight ?? 560 }}>
          {body}
        </div>
      ) : (
        body
      )}
      {windowed && (
        <div className="border-t border-base-content/10 px-4 py-1.5 text-xs text-base-content/70 tabular">
          {rows.length.toLocaleString()} rows · showing {view.first + 1}–{Math.min(view.last, rows.length)}
        </div>
      )}
    </div>
  );
}
