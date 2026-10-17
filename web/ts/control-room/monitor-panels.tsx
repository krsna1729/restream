import { useMemo } from "react";
import type { ReactNode } from "react";

import { formatBps, formatPercent } from "./format.js";
import { destinationOutlier, DELIVERY_FLOOR, inputTransport, mediaPath } from "./monitor-path.js";
import { Freshness } from "./polled.js";
import { PlaceLink } from "./router.js";
import { pipelineGraph, pipelineInputs } from "./state-views.js";
import type { LiveSnapshot } from "./store.js";

/** The selected input's transport, the shared media path and the
 * destination outlier of one pipeline. */
export function MonitorPanels(props: { live: LiveSnapshot; pipelineId: string }) {
  return (
    <div className="grid gap-4 lg:grid-cols-3">
      <InputTransport live={props.live} pipelineId={props.pipelineId} />
      <SharedPath live={props.live} pipelineId={props.pipelineId} />
      <Outlier live={props.live} pipelineId={props.pipelineId} />
    </div>
  );
}

function Panel(props: {
  id: string;
  title: string;
  atMs?: number | null;
  badge?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="card bg-base-200" aria-labelledby={props.id}>
      <div className="card-body gap-2 p-4">
        <div className="flex items-center justify-between gap-2">
          <h3 id={props.id} className="card-title text-base">
            {props.title}
            {props.badge}
          </h3>
          {props.atMs !== undefined && <Freshness atMs={props.atMs} />}
        </div>
        {props.children}
      </div>
    </section>
  );
}

function Rows({ rows }: { rows: ReadonlyArray<[string, string]> }) {
  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
      {rows.map(([label, value]) => (
        <div key={label} className="contents">
          <dt className="opacity-70">{label}</dt>
          <dd className="text-right tabular-nums">{value}</dd>
        </div>
      ))}
    </dl>
  );
}

function InputTransport(props: { live: LiveSnapshot; pipelineId: string }) {
  const { state, telemetry } = props.live;
  const transport = useMemo(() => {
    const inputs = pipelineInputs(state, telemetry, props.pipelineId);
    return inputs === null ? undefined : inputTransport(inputs);
  }, [state, telemetry, props.pipelineId]);
  return (
    <Panel id="monitor-transport" title="Input transport" atMs={props.live.telemetryAtMs}>
      {transport === undefined ? (
        <p className="text-sm opacity-70">Loading inputs…</p>
      ) : transport === null ? (
        <p className="text-sm opacity-70">No selected input is connected.</p>
      ) : (
        <>
          <p className="text-sm">
            {transport.label} · {transport.protocol}
          </p>
          {transport.rows.length === 0 ? (
            <p className="text-sm opacity-70">The publisher reports no transport metrics.</p>
          ) : (
            <Rows rows={transport.rows} />
          )}
        </>
      )}
    </Panel>
  );
}

function SharedPath(props: { live: LiveSnapshot; pipelineId: string }) {
  const { state, telemetry } = props.live;
  const path = useMemo(() => {
    const graph = pipelineGraph(state, telemetry, props.pipelineId);
    return graph ? mediaPath(graph, state?.outputs ?? []) : null;
  }, [state, telemetry, props.pipelineId]);
  return (
    <Panel id="monitor-path" title="Shared media path" atMs={props.live.telemetryAtMs}>
      {path === null ? (
        <p className="text-sm opacity-70">Loading the processing graph…</p>
      ) : (
        <Rows
          rows={[
            [
              "Source ring",
              path.ring ? `${path.ring.fill} / ${path.ring.capacity}` : "not running",
            ],
            ...(path.ring?.worstLagSlots !== null && path.ring
              ? [["Worst reader lag", `${path.ring.worstLagSlots} slots`] as [string, string]]
              : []),
            ...path.stages.map(
              (stage): [string, string] => [
                stage.label,
                [
                  stage.active ? null : "stopped",
                  stage.packetsPerSec === null
                    ? null
                    : `${Math.round(stage.packetsPerSec)} pkt/s`,
                  `${stage.outputs} output${stage.outputs === 1 ? "" : "s"}`,
                ]
                  .filter(Boolean)
                  .join(" · "),
              ],
            ),
            ["Reader overflow", path.ring ? String(path.ring.overflows) : "--"],
          ]}
        />
      )}
    </Panel>
  );
}

function Outlier(props: { live: LiveSnapshot; pipelineId: string }) {
  const outputs = props.live.state?.outputs;
  const egresses = props.live.telemetry?.egresses;
  const outlier = useMemo(
    () =>
      outputs && egresses ? destinationOutlier(props.pipelineId, outputs, egresses) : null,
    [props.pipelineId, outputs, egresses],
  );
  return (
    <Panel
      id="monitor-outlier"
      title="Destination outlier"
      atMs={props.live.telemetryAtMs}
      badge={
        outlier && (
          <span className="badge badge-warning badge-sm font-normal">{outlier.name}</span>
        )
      }
    >
      {!egresses ? (
        <p className="text-sm opacity-70">Waiting for engine telemetry…</p>
      ) : outlier === null ? (
        <p className="text-sm opacity-70">
          Every rated output delivers at least {formatPercent(100 * DELIVERY_FLOOR)} of its
          feed.
        </p>
      ) : (
        <>
          <Rows
            rows={[
              ["Offered", formatBps(outlier.offeredBps)],
              ["Delivered", formatBps(outlier.deliveredBps)],
              ["Ratio", outlier.ratio.toFixed(3)],
              [
                "Peers (same stage and protocol)",
                outlier.peersRatio === null ? "none" : outlier.peersRatio.toFixed(3),
              ],
              ["Feed lag", outlier.feedLag === null ? "--" : `${outlier.feedLag} units`],
            ]}
          />
          <PlaceLink
            className="link text-sm"
            place={{ workspace: "operate", pipelineId: props.pipelineId }}
          >
            Open in Operate
          </PlaceLink>
        </>
      )}
    </Panel>
  );
}
