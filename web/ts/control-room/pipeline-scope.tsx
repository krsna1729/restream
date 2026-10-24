import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";

import { MenuButton, SearchField } from "../ui/controls.js";
import { Icon } from "../ui/icons.js";
import { PageHeader } from "../ui/page-header.js";
import { StatusDot } from "../ui/tone.js";
import { EmptyState, Panel } from "../ui/panel.js";
import { buildFleetModel, filterPipelineRows, pipelineHealth } from "./fleet-model.js";
import type { FleetPipelineRow } from "./fleet-model.js";
import { formatBps } from "./format.js";
import { HEALTH_TONE, HealthBadge, PipelineTable } from "./pipeline-table.js";
import { WORKSPACE_LABELS } from "./route.js";
import type { Workspace } from "./route.js";
import { PlaceLink, useGo } from "./router.js";
import type { LiveSnapshot } from "./store.js";

/** The workspaces about one pipeline, as tabs under its header. */
const PIPELINE_TABS: readonly Workspace[] = ["operate", "health", "monitor", "inspect", "logs"];

/**
 * A workspace about one pipeline: a searchable picker until one is chosen
 * (or after it is deleted), then its header and `children`, keyed by the
 * pipeline so per-pipeline state and polls restart when it changes.
 */
export function PipelineScope(props: {
  live: LiveSnapshot;
  workspace: Workspace;
  pipelineId: string | null;
  verb: string;
  children: (pipelineId: string) => ReactNode;
}) {
  const { live, workspace, pipelineId } = props;
  const state = live.state;
  const rows = useMemo(() => (state ? buildFleetModel(state, live.rates).pipelines : null), [state, live.rates]);
  const row = rows?.find((candidate) => candidate.id === pipelineId) ?? null;
  const [query, setQuery] = useState("");
  if (state === null || rows === null) {
    return <EmptyState title="Waiting for runtime state…" />;
  }
  if (pipelineId === null || row === null) {
    const shown = filterPipelineRows(rows, query, "all");
    return (
      <div className="flex flex-col gap-4">
        <PageHeader
          eyebrow={WORKSPACE_LABELS[workspace]}
          title={pipelineId === null ? `Choose a pipeline to ${props.verb}` : "That pipeline no longer exists"}
          subtitle="Degraded pipelines first. Search by name or id."
        />
        <Panel
          flush
          title="Pipelines"
          subtitle={`${shown.length.toLocaleString()} of ${rows.length.toLocaleString()}`}
          actions={<SearchField value={query} onChange={setQuery} placeholder="Name or id" label="Find a pipeline" className="w-full sm:w-64" />}
        >
          <PipelineTableFor rows={shown} workspace={workspace} />
        </Panel>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-4">
      <PipelineHeading live={live} workspace={workspace} pipelineId={pipelineId} />
      <div key={pipelineId} className="flex flex-col gap-4">
        {props.children(pipelineId)}
      </div>
    </div>
  );
}

/**
 * One pipeline's header (name, health, rates, a picker) and the tabs
 * between the workspaces about it; null while it is unknown.
 */
export function PipelineHeading(props: { live: LiveSnapshot; workspace: Workspace; pipelineId: string }) {
  const { live, workspace, pipelineId } = props;
  const state = live.state;
  const rows = useMemo(() => (state ? buildFleetModel(state, live.rates).pipelines : null), [state, live.rates]);
  const row = rows?.find((candidate) => candidate.id === pipelineId) ?? null;
  const tabs = useRef<HTMLElement | null>(null);
  // On a phone the strip scrolls: bring the current tab into it.
  useEffect(() => {
    const strip = tabs.current;
    const current = strip?.querySelector<HTMLElement>('[aria-current="page"]');
    if (!strip || !current) return;
    const left = current.offsetLeft - strip.offsetLeft;
    if (left + current.offsetWidth > strip.scrollLeft + strip.clientWidth || left < strip.scrollLeft) {
      strip.scrollLeft = left - (strip.clientWidth - current.offsetWidth) / 2;
    }
  }, [workspace, row !== null]);
  if (state === null || row === null) return null;
  return (
    <>
      <PageHeader
        eyebrow={`Pipeline · ${WORKSPACE_LABELS[workspace]}`}
        title={
          <span className="inline-flex flex-wrap items-center gap-x-3 gap-y-1">
            {row.name} <HealthBadge row={row} />
          </span>
        }
        subtitle={`${row.source ? `${row.source.protocol.toUpperCase()}${row.source.bonded ? " bonded" : ""} in · ${formatBps(row.ingestBps)}` : "no source connected"} · ${formatBps(row.egressBps)} out · ${row.delivering} / ${row.wanted} destinations delivering`}
        actions={
          <PipelinePicker rows={rows ?? []} workspace={workspace} pipelineId={pipelineId} />
        }
      />
      <nav ref={tabs} aria-label="Pipeline views" className="tab-strip -mt-1">
        {PIPELINE_TABS.map((tab) => (
          <PlaceLink
            key={tab}
            place={{ workspace: tab, pipelineId }}
            current={tab === workspace}
            className="tab-strip-item"
          >
            {WORKSPACE_LABELS[tab]}
          </PlaceLink>
        ))}
      </nav>
    </>
  );
}

/** How many matches the picker lists; a search narrows the rest. */
const PICKER_LIMIT = 50;

/** Switch pipeline: a button with the current name that opens a search
 * over every pipeline, worst health first, Enter taking the top match. */
function PipelinePicker(props: { rows: readonly FleetPipelineRow[]; workspace: Workspace; pipelineId: string }) {
  const go = useGo();
  const [query, setQuery] = useState("");
  const search = useRef<HTMLInputElement | null>(null);
  const current = props.rows.find((row) => row.id === props.pipelineId);
  const shown = filterPipelineRows(props.rows, query, "all");
  return (
    <MenuButton
      label="Switch pipeline"
      className="btn btn-sm touch-36 w-full justify-between border-base-content/15 bg-base-100 font-normal sm:w-64"
      width="w-80"
      onOpen={() => search.current?.select()}
      button={
        <>
          <span className="truncate">{current?.name ?? "Pipeline"}</span>
          <Icon name="chevron" className="size-4 shrink-0 rotate-90 text-base-content/70" />
        </>
      }
    >
      {(close) => (
        <div className="flex flex-col gap-2">
          <label className="input input-sm w-full">
            <Icon name="search" className="size-4 text-base-content/70" />
            <input
              ref={search}
              type="search"
              aria-label="Find a pipeline"
              placeholder="Name or id"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={(event) => {
                const first = shown[0];
                if (event.key === "Enter" && first) {
                  // Closing hands focus back to the picker button; without
                  // this the same Enter would press it and reopen the menu.
                  event.preventDefault();
                  close();
                  go({ workspace: props.workspace, pipelineId: first.id });
                }
              }}
            />
          </label>
          <ul aria-label="Pipelines" className="scrollbar-thin -mx-2 flex max-h-80 flex-col overflow-y-auto">
            {shown.slice(0, PICKER_LIMIT).map((row) => (
              <li key={row.id}>
                <PlaceLink
                  place={{ workspace: props.workspace, pipelineId: row.id }}
                  current={row.id === props.pipelineId}
                  onClick={close}
                  className={`touch-36 flex items-center gap-2 rounded-field px-2 py-1.5 text-sm hover:bg-base-content/5 ${row.id === props.pipelineId ? "bg-primary/10 font-semibold" : ""}`}
                >
                  <StatusDot tone={HEALTH_TONE[pipelineHealth(row).tone]} />
                  <span className="min-w-0 flex-1 truncate">{row.name}</span>
                  {row.egressBps !== null && <span className="text-xs text-base-content/70 tabular">{formatBps(row.egressBps)} out</span>}
                </PlaceLink>
              </li>
            ))}
            {shown.length === 0 && <li className="px-2 py-1.5 text-sm text-base-content/70">No pipeline matches.</li>}
            {shown.length > PICKER_LIMIT && (
              <li className="px-2 py-1.5 text-xs text-base-content/70">{(shown.length - PICKER_LIMIT).toLocaleString()} more: narrow the search.</li>
            )}
          </ul>
        </div>
      )}
    </MenuButton>
  );
}

function PipelineTableFor(props: { rows: Parameters<typeof PipelineTable>[0]["rows"]; workspace: Workspace }) {
  // The picker's rows open the scoped workspace, not Operate.
  const go = useGo();
  return (
    <PipelineTable
      rows={props.rows}
      label="Choose a pipeline"
      maxHeight={640}
      onOpen={(id) => go({ workspace: props.workspace, pipelineId: id })}
    />
  );
}
