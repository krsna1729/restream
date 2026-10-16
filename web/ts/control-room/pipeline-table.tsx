import { pipelineHealth } from "./fleet-model.js";
import type { FleetPipelineRow, PipelineHealth } from "./fleet-model.js";
import { formatBps } from "./format.js";
import { PlaceLink } from "./router.js";

const HEALTH_BADGE: Record<PipelineHealth["tone"], string> = {
  warning: "badge-warning",
  info: "badge-info",
  idle: "badge-ghost",
  ok: "badge-success",
};

export function HealthBadge({ row }: { row: FleetPipelineRow }) {
  const health = pipelineHealth(row);
  return (
    <span className={`badge badge-sm ${HEALTH_BADGE[health.tone]}`}>
      {health.label}
    </span>
  );
}

/** One row per pipeline: its source, delivery, outputs, flow and health,
 * each linked to its Operate view. */
export function PipelineTable(props: {
  rows: readonly FleetPipelineRow[];
  label: string;
}) {
  return (
    <div className="overflow-x-auto">
      <table className="table table-sm" aria-label={props.label}>
        <thead>
          <tr>
            <th>Pipeline</th>
            <th>Source</th>
            <th>Delivering</th>
            <th>Outputs by protocol</th>
            <th>Flow</th>
            <th>Health</th>
          </tr>
        </thead>
        <tbody>
          {props.rows.map((row) => (
            <tr key={row.id}>
              <td>
                <PlaceLink
                  className="link font-medium"
                  place={{ workspace: "operate", pipelineId: row.id }}
                >
                  {row.name}
                </PlaceLink>
                <div className="text-xs opacity-60">{row.id}</div>
              </td>
              <td>
                {row.source
                  ? `${row.source.protocol.toUpperCase()}${
                      row.source.videoCodec ? ` · ${row.source.videoCodec}` : ""
                    }`
                  : "no input"}
              </td>
              <td className="tabular-nums">
                {row.delivering} / {row.wanted}
              </td>
              <td>
                {[...row.protocols]
                  .map(([protocol, count]) => `${protocol.toUpperCase()} ${count}`)
                  .join(" · ") || "none"}
              </td>
              <td className="tabular-nums">
                {formatBps(row.ingestBps)} → {formatBps(row.egressBps)}
              </td>
              <td>
                <HealthBadge row={row} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
