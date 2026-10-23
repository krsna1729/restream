import { DataTable } from "../ui/data-table.js";
import type { Column } from "../ui/data-table.js";
import { Meter } from "../ui/kpi.js";
import { StatusBadge } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";
import { pipelineHealth } from "./fleet-model.js";
import type { FleetPipelineRow, PipelineHealth } from "./fleet-model.js";
import { formatBps } from "./format.js";
import { PlaceLink, useGo } from "./router.js";

const HEALTH_TONE: Record<PipelineHealth["tone"], Tone> = {
  warning: "warn",
  info: "info",
  idle: "neutral",
  ok: "ok",
};

export function HealthBadge({ row }: { row: FleetPipelineRow }) {
  const health = pipelineHealth(row);
  return (
    <StatusBadge tone={HEALTH_TONE[health.tone]} dot>
      {health.label}
    </StatusBadge>
  );
}

function protocolMix(row: FleetPipelineRow): string {
  return (
    [...row.protocols].map(([protocol, count]) => `${protocol.toUpperCase()} ${count}`).join(" · ") || "none"
  );
}

const COLUMNS: readonly Column<FleetPipelineRow>[] = [
  {
    id: "name",
    header: "Pipeline",
    width: "minmax(10rem,2fr)",
    sort: (row) => row.name.toLowerCase(),
    cell: (row) => (
      <PlaceLink className="font-medium hover:underline" place={{ workspace: "operate", pipelineId: row.id }}>
        {row.name}
      </PlaceLink>
    ),
  },
  {
    id: "source",
    header: "Source",
    width: "minmax(6rem,1fr)",
    priority: 2,
    cell: (row) =>
      row.source ? (
        <span>
          {row.source.protocol.toUpperCase()}
          {row.source.bonded ? " bonded" : ""}
          <span className="text-base-content/60">{row.source.videoCodec ? ` · ${row.source.videoCodec}` : ""}</span>
        </span>
      ) : (
        <span className="text-base-content/60">no source</span>
      ),
  },
  {
    id: "delivering",
    header: "Delivering",
    width: "minmax(7rem,1fr)",
    align: "right",
    sort: (row) => (row.wanted === 0 ? null : row.delivering / row.wanted),
    cell: (row) => (
      <span className="inline-flex w-full items-center justify-end gap-2">
        <span className="hidden w-14 lg:block">
          <Meter value={row.wanted === 0 ? null : row.delivering / row.wanted} tone={row.attention > 0 ? "warn" : "ok"} label={`${row.name} delivering`} />
        </span>
        {row.delivering} / {row.wanted}
      </span>
    ),
  },
  { id: "protocols", header: "Destinations", width: "minmax(9rem,1.5fr)", priority: 3, cell: protocolMix },
  {
    id: "flow",
    header: "In → out",
    width: "minmax(9rem,1.2fr)",
    align: "right",
    priority: 2,
    sort: (row) => row.egressBps,
    cell: (row) => `${formatBps(row.ingestBps)} → ${formatBps(row.egressBps)}`,
  },
  {
    id: "health",
    header: "Health",
    width: "8.5rem",
    sort: (row) => row.attention * 1000 + row.pending,
    cell: (row) => <HealthBadge row={row} />,
  },
];

/** One row per pipeline: source, delivery, destinations, flow and health,
 * each linked to its Operate view. Windowed, so the whole plant fits. */
export function PipelineTable(props: {
  rows: readonly FleetPipelineRow[];
  label: string;
  maxHeight?: number;
  /** Where a row click goes; Operate by default. */
  onOpen?: (pipelineId: string) => void;
}) {
  const go = useGo();
  return (
    <DataTable
      rows={props.rows}
      columns={COLUMNS}
      rowKey={(row) => row.id}
      label={props.label}
      maxHeight={props.maxHeight}
      cardHeight={118}
      cardStatus={(row) => <HealthBadge row={row} />}
      onRowClick={(row) => (props.onOpen ? props.onOpen(row.id) : go({ workspace: "operate", pipelineId: row.id }))}
    />
  );
}
