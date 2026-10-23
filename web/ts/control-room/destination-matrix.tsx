// Every destination of one pipeline: filterable by state, protocol and name,
// windowed so thousands of rows scroll, with start/stop, edit and delete.
// Operators read a state badge, a retry countdown and the cause in words;
// engineers also get delivery ratio, feed lag, shard and stage.

import { useMemo, useState } from "react";

import { deleteOutput, getYoutubeMonitoringStatus, startOut, stopOut } from "../core/api.js";
import type { TelemetryEgress } from "../core/api-types.js";
import type { EgressProtocol, OutputState } from "../core/generated/runtime-state.js";
import type { ConfigData } from "../types.js";
import { ChipGroup, SearchField } from "../ui/controls.js";
import { DataTable } from "../ui/data-table.js";
import type { Column } from "../ui/data-table.js";
import { Icon } from "../ui/icons.js";
import { Panel } from "../ui/panel.js";
import { useEngineer } from "../ui/prefs.js";
import { StatusBadge } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";
import { ConfirmDelete } from "./confirm.js";
import { useSession } from "./session.js";
import { describeCause } from "./fleet-model.js";
import { formatBps } from "./format.js";
import {
  destinationFilterCounts,
  filterDestinationRows,
  isYoutubeUrl,
  youtubeVerdict,
} from "./operate-model.js";
import type { DestinationFilter, DestinationRow } from "./operate-model.js";
import { OutputForm } from "./output-form.js";
import { PROTOCOL_LABEL } from "./protocol-style.js";
import { useNow } from "./store.js";

const FILTER_LABEL: Record<DestinationFilter, string> = {
  all: "All",
  attention: "Needs attention",
  retrying: "Retrying",
  starting: "Starting",
  delivering: "Delivering",
  stopped: "Stopped",
};

/** One badge for an output's state, in the operator's words. */
export function outputState(output: OutputState, delivering: boolean): { tone: Tone; label: string } {
  if (output.desired === "stopped") return { tone: "neutral", label: "stopped" };
  if (output.convergence === "waitingForIngest") return { tone: "info", label: "waiting for source" };
  if (output.convergence === "retriesExhausted") return { tone: "bad", label: "gave up" };
  if (output.convergence === "retrying") return { tone: "warn", label: "retrying" };
  if (output.diagnosis) return { tone: output.diagnosis.layer === "destination" ? "warn" : "bad", label: output.flow === "stalled" ? "stalled" : "degraded" };
  if (delivering) return { tone: "ok", label: "live" };
  return { tone: "info", label: output.convergence === "stopping" ? "stopping" : "starting" };
}

function cause(output: OutputState): string {
  if (output.diagnosis) return describeCause(output.diagnosis.cause);
  if (output.evidence) return output.evidence.error;
  if (output.backpressure) return `held back: ${output.backpressure}`;
  return "";
}

/** Seconds until the next attempt, ticking. */
function RetryCountdown(props: { atMs: number; attempts: number }) {
  const now = useNow(1000);
  const seconds = Math.max(0, Math.round((props.atMs - now) / 1000));
  return (
    <span className="tabular text-xs text-base-content/70">
      #{props.attempts} · {seconds > 0 ? `next in ${seconds >= 60 ? `${Math.floor(seconds / 60)}m ${seconds % 60}s` : `${seconds}s`}` : "now"}
    </span>
  );
}

export function DestinationMatrix(props: {
  pipelineId: string;
  rows: readonly DestinationRow[];
  egressBps: ReadonlyMap<string, number> | null;
  egresses: readonly TelemetryEgress[] | null;
  config: ConfigData | null;
  onConfigChanged: () => void;
}) {
  const engineer = useEngineer();
  const [filter, setFilter] = useState<DestinationFilter>("all");
  const [protocol, setProtocol] = useState<EgressProtocol | "any">("any");
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  /** The output id being edited, "new", or null. */
  const [editing, setEditing] = useState<string | null>(null);
  const { config } = props;
  const counts = useMemo(() => destinationFilterCounts(props.rows), [props.rows]);
  const protocols = useMemo(() => {
    const seen = new Map<EgressProtocol, number>();
    for (const row of props.rows) seen.set(row.output.protocol, (seen.get(row.output.protocol) ?? 0) + 1);
    return [...seen].sort((a, b) => b[1] - a[1]);
  }, [props.rows]);
  const shown = useMemo(
    () => filterDestinationRows(props.rows, filter, query, protocol === "any" ? null : protocol),
    [props.rows, filter, query, protocol],
  );
  const telemetry = useMemo(() => new Map((props.egresses ?? []).map((egress) => [egress.outputId, egress])), [props.egresses]);
  const monitoringUrls = useMemo(
    () => new Map((config?.outputs ?? []).map((stored) => [stored.id, stored.monitoringUrl ?? ""])),
    [config],
  );
  const editedOutput = editing && editing !== "new" ? (config?.outputs.find((output) => output.id === editing) ?? null) : null;

  const toggle = async (output: OutputState) => {
    setBusy(output.id);
    try {
      await (output.desired === "running" ? stopOut : startOut)(props.pipelineId, output.id);
    } finally {
      setBusy(null);
    }
  };

  const canOperate = useSession().can("operator");
  const columns: Column<DestinationRow>[] = [
    {
      id: "name",
      header: "Destination",
      width: "minmax(10rem,1.6fr)",
      sort: (row) => row.output.name.toLowerCase(),
      cell: (row) => (
        <span className="inline-flex min-w-0 items-center gap-2">
          <span className="truncate font-medium" title={row.output.name}>{row.output.name}</span>
          {isYoutubeUrl(monitoringUrls.get(row.output.id) ?? "") && (
            <YoutubeCheck name={row.output.name} url={monitoringUrls.get(row.output.id) ?? ""} />
          )}
        </span>
      ),
    },
    {
      id: "protocol",
      header: "Protocol",
      width: "5rem",
      priority: 2,
      sort: (row) => row.output.protocol,
      cell: (row) => PROTOCOL_LABEL[row.output.protocol],
    },
    {
      id: "state",
      header: "State",
      width: "minmax(9rem,1fr)",
      sort: (row) => (row.output.diagnosis ? 0 : row.delivering ? 2 : 1),
      cell: (row) => {
        const state = outputState(row.output, row.delivering);
        return (
          <span className="inline-flex min-w-0 items-center gap-2">
            <StatusBadge tone={state.tone} dot>
              {state.label}
            </StatusBadge>
            {row.output.retry && <RetryCountdown atMs={row.output.retry.nextAttemptAtMs} attempts={row.output.retry.attempts} />}
          </span>
        );
      },
    },
    {
      id: "rate",
      header: "Rate",
      width: "6.5rem",
      align: "right",
      priority: 2,
      sort: (row) => props.egressBps?.get(row.output.id) ?? null,
      cell: (row) => formatBps(props.egressBps?.get(row.output.id)),
    },
    ...(engineer
      ? ([
          {
            id: "ratio",
            header: "Delivery",
            width: "5.5rem",
            align: "right",
            priority: 3,
            sort: (row) => (telemetry.get(row.output.id)?.quality?.deliveryRatio as number | undefined) ?? null,
            cell: (row) => {
              const value = telemetry.get(row.output.id)?.quality?.deliveryRatio;
              return typeof value === "number" ? (
                <span className={value < 0.95 ? "text-warning" : ""}>{value.toFixed(3)}</span>
              ) : (
                "--"
              );
            },
          },
          {
            id: "lag",
            header: "Feed lag",
            width: "5rem",
            align: "right",
            priority: 3,
            sort: (row) => row.feedLagUnits,
            cell: (row) => (row.feedLagUnits === null ? "--" : row.feedLagUnits.toLocaleString()),
          },
          {
            id: "shard",
            header: "Shard",
            width: "4rem",
            align: "right",
            priority: 3,
            sort: (row) => row.output.shard,
            cell: (row) => (row.output.shard === null ? "--" : `#${row.output.shard}`),
          },
        ] satisfies Column<DestinationRow>[])
      : []),
    {
      id: "cause",
      header: "Cause",
      width: "minmax(8rem,2fr)",
      priority: 2,
      bare: true,
      cell: (row) => {
        const text = cause(row.output);
        return text ? (
          <span className={`block truncate ${row.output.diagnosis ? "text-warning" : "text-base-content/70"}`} title={text}>
            {text}
          </span>
        ) : (
          <span className="text-base-content/40">—</span>
        );
      },
    },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      width: "11rem",
      align: "right",
      cell: (row) => {
        const running = row.output.desired === "running";
        return (
          <span className="inline-flex items-center gap-1" onClick={(event) => event.stopPropagation()}>
            <button
              type="button"
              className={`btn btn-xs h-7 ${running ? "btn-ghost border-base-content/20" : "btn-accent"}`}
              disabled={busy !== null}
              aria-label={`${running ? "Stop" : "Start"} ${row.output.name}`}
              onClick={() => void toggle(row.output)}
            >
              <Icon name={running ? "stop" : "play"} className="size-3" />
              {busy === row.output.id ? "…" : running ? "Stop" : "Start"}
            </button>
            <button
              type="button"
              className="btn btn-ghost btn-xs h-7"
              aria-label={`Edit ${row.output.name}`}
              disabled={config === null}
              onClick={() => setEditing(row.output.id)}
            >
              Edit
            </button>
            <ConfirmDelete
              label={`Delete ${row.output.name}`}
              question="Delete this destination?"
              onDelete={async () => {
                const deleted = (await deleteOutput(props.pipelineId, row.output.id)) !== null;
                if (deleted) {
                  if (editing === row.output.id) setEditing(null);
                  props.onConfigChanged();
                }
                return deleted;
              }}
            />
          </span>
        );
      },
    },
  ];

  return (
    <Panel
      flush
      id="operate-destinations"
      title="Destination matrix"
      subtitle={`${shown.length.toLocaleString()} of ${props.rows.length.toLocaleString()} · degraded first`}
      actions={
        <>
          <SearchField value={query} onChange={setQuery} placeholder="Name or id" label="Search destinations" className="w-full sm:w-52" />
          {canOperate && (
            <button type="button" className="btn btn-primary btn-sm" disabled={config === null} onClick={() => setEditing("new")}>
              Add destination
            </button>
          )}
        </>
      }
    >
      {config && (editing === "new" || editedOutput) && (
        <div className="border-b border-base-content/10 p-4">
          <OutputForm
            key={editing}
            pipelineId={props.pipelineId}
            stored={editedOutput}
            profiles={Object.keys(config.transcodeProfiles ?? {})}
            defaultHost={config.ingestHost || "localhost"}
            onSaved={(output) => {
              setEditing(output.id);
              props.onConfigChanged();
            }}
            onClose={() => setEditing(null)}
          />
        </div>
      )}
      <div className="flex flex-col gap-2 border-b border-base-content/10 px-4 py-2 lg:flex-row lg:items-center">
        <ChipGroup<DestinationFilter>
          label="State"
          value={filter}
          onChange={setFilter}
          chips={(Object.keys(FILTER_LABEL) as DestinationFilter[]).map((value) => ({
            value,
            label: FILTER_LABEL[value],
            count: counts[value],
            alert: value === "attention" || value === "retrying",
          }))}
        />
        {protocols.length > 1 && (
          <div className="min-w-0 lg:ml-auto">
            <ChipGroup<EgressProtocol | "any">
              label="Protocol"
              value={protocol}
              onChange={setProtocol}
              chips={[
                { value: "any", label: "Any protocol" },
                ...protocols.map(([value, count]) => ({ value, label: PROTOCOL_LABEL[value], count })),
              ]}
            />
          </div>
        )}
      </div>
      <DataTable
        rows={shown}
        columns={canOperate ? columns : columns.filter((column) => column.id !== "actions")}
        rowKey={(row) => row.output.id}
        label="Destinations"
        rowHeight={44}
        cardHeight={128}
        maxHeight={620}
        cardStatus={(row) => {
          const state = outputState(row.output, row.delivering);
          return <StatusBadge tone={state.tone} dot>{state.label}</StatusBadge>;
        }}
        empty={filter === "all" && !query ? "No destination yet. Add one to start restreaming." : "No destination matches."}
      />
    </Panel>
  );
}

/** On request, asks the engine what YouTube's watch page says about the
 * stream; never polled. */
function YoutubeCheck(props: { name: string; url: string }) {
  const [checking, setChecking] = useState(false);
  const [verdict, setVerdict] = useState<{ text: string; title: string } | null>(null);
  return (
    <span className="inline-flex shrink-0 items-center gap-1 text-xs font-normal" onClick={(event) => event.stopPropagation()}>
      <button
        type="button"
        className="btn btn-ghost btn-xs"
        disabled={checking}
        aria-label={`Check ${props.name} on YouTube`}
        onClick={async () => {
          setChecking(true);
          try {
            const status = await getYoutubeMonitoringStatus(props.url);
            setVerdict({ text: youtubeVerdict(status), title: status?.title ?? "" });
          } finally {
            setChecking(false);
          }
        }}
      >
        {checking ? "…" : "YouTube"}
      </button>
      {verdict && (
        <span role="status" title={verdict.title}>
          {verdict.text}
        </span>
      )}
    </span>
  );
}
