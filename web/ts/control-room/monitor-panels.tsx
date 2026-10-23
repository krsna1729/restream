import { useMemo } from "react";
import type { ReactNode } from "react";

import { KeyValue } from "../ui/kpi.js";
import { Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
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

function Section(props: { id: string; title: string; atMs?: number | null; badge?: ReactNode; children: ReactNode }) {
  return (
    <Panel
      id={props.id}
      title={props.title}
      actions={
        <>
          {props.badge}
          {props.atMs !== undefined && <Freshness atMs={props.atMs} />}
        </>
      }
    >
      <div className="flex flex-col gap-2">{props.children}</div>
    </Panel>
  );
}

function Rows({ rows }: { rows: ReadonlyArray<[string, string]> }) {
  return <KeyValue rows={rows} />;
}

function InputTransport(props: { live: LiveSnapshot; pipelineId: string }) {
  const { state, telemetry } = props.live;
  const transport = useMemo(() => {
    const inputs = pipelineInputs(state, telemetry, props.pipelineId);
    return inputs === null ? undefined : inputTransport(inputs);
  }, [state, telemetry, props.pipelineId]);
  return (
    <Section id="monitor-transport" title="Input transport" atMs={props.live.telemetryAtMs}>
      {transport === undefined ? (
        <p className="text-sm text-base-content/70">Loading inputs…</p>
      ) : transport === null ? (
        <p className="text-sm text-base-content/70">No selected input is connected.</p>
      ) : (
        <>
          <p className="text-sm">
            {transport.label} · {transport.protocol}
          </p>
          {transport.rows.length === 0 ? (
            <p className="text-sm text-base-content/70">The publisher reports no transport metrics.</p>
          ) : (
            <Rows rows={transport.rows} />
          )}
        </>
      )}
    </Section>
  );
}

function SharedPath(props: { live: LiveSnapshot; pipelineId: string }) {
  const { state, telemetry } = props.live;
  const path = useMemo(() => {
    const graph = pipelineGraph(state, telemetry, props.pipelineId);
    return graph ? mediaPath(graph, state?.outputs ?? []) : null;
  }, [state, telemetry, props.pipelineId]);
  return (
    <Section id="monitor-path" title="Shared media path" atMs={props.live.telemetryAtMs}>
      {path === null ? (
        <p className="text-sm text-base-content/70">Loading the processing graph…</p>
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
    </Section>
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
    <Section
      id="monitor-outlier"
      title="Destination outlier"
      atMs={props.live.telemetryAtMs}
      badge={
        outlier && (
          <StatusBadge tone="warn">{outlier.name}</StatusBadge>
        )
      }
    >
      {!egresses ? (
        <p className="text-sm text-base-content/70">Waiting for engine telemetry…</p>
      ) : outlier === null ? (
        <p className="text-sm text-base-content/70">
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
            className="link touch-36 inline-flex items-center self-start text-sm"
            place={{ workspace: "operate", pipelineId: props.pipelineId }}
          >
            Open in Operate
          </PlaceLink>
        </>
      )}
    </Section>
  );
}
