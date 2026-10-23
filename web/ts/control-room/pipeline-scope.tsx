import { useMemo, useState } from "react";
import type { ReactNode } from "react";

import { SearchField } from "../ui/controls.js";
import { PageHeader } from "../ui/page-header.js";
import { EmptyState, Panel } from "../ui/panel.js";
import { useEngineer } from "../ui/prefs.js";
import { buildFleetModel, filterPipelineRows } from "./fleet-model.js";
import { formatBps } from "./format.js";
import { HealthBadge, PipelineTable } from "./pipeline-table.js";
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
  const go = useGo();
  const engineer = useEngineer();
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
      <PageHeader
        eyebrow={
          <>
            Pipeline · {WORKSPACE_LABELS[workspace]}
            {engineer && <span className="ml-2 font-mono normal-case tracking-normal select-all">{pipelineId}</span>}
          </>
        }
        title={
          <span className="inline-flex flex-wrap items-center gap-x-3 gap-y-1">
            {row.name} <HealthBadge row={row} />
          </span>
        }
        subtitle={`${row.source ? `${row.source.protocol.toUpperCase()}${row.source.bonded ? " bonded" : ""} in · ${formatBps(row.ingestBps)}` : "no source connected"} · ${formatBps(row.egressBps)} out · ${row.delivering} / ${row.wanted} destinations delivering`}
        actions={
          <select
            className="select select-sm w-full sm:w-64"
            aria-label="Pipeline"
            value={pipelineId}
            onChange={(event) => go({ workspace, pipelineId: event.target.value })}
          >
            {state.pipelines.map((pipeline) => (
              <option key={pipeline.id} value={pipeline.id}>
                {pipeline.name}
              </option>
            ))}
          </select>
        }
      />
      <nav aria-label="Pipeline views" className="scrollbar-thin -mt-1 flex gap-1 overflow-x-auto border-b border-base-content/10">
        {PIPELINE_TABS.map((tab) => (
          <PlaceLink
            key={tab}
            place={{ workspace: tab, pipelineId }}
            current={tab === workspace}
            className={`-mb-px border-b-2 px-3 py-2 text-sm whitespace-nowrap ${tab === workspace ? "border-primary font-semibold" : "border-transparent text-base-content/70 hover:text-base-content"}`}
          >
            {WORKSPACE_LABELS[tab]}
          </PlaceLink>
        ))}
      </nav>
      <div key={pipelineId} className="flex flex-col gap-4">
        {props.children(pipelineId)}
      </div>
    </div>
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
