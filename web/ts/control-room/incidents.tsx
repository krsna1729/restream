import { useEffect, useMemo, useRef, useState } from "react";

import { getAggregateAlerts, getLifecycleEvents } from "../core/api.js";
import type {
  AlertsSnapshot,
  LifecycleEventsSnapshot,
  OperatorAlert,
} from "../core/api-types.js";
import { formatAge } from "./format.js";
import {
  evidencePacket,
  incidentTimeline,
  rankIncidents,
  updateResolved,
} from "./incidents-model.js";
import type { ResolvedIncident } from "./incidents-model.js";
import { PlaceLink } from "./place-link.js";
import { Freshness, usePolled } from "./polled.js";
import { useNow } from "./store.js";
import type { LiveSnapshot } from "./store.js";

const RESOLVED_CAP = 20;
const EVENT_LIMIT = 200;

/** Open incidents, what failed and what did not, and what to do. */
export function Incidents({ live }: { live: LiveSnapshot }) {
  const alerts = usePolled<AlertsSnapshot>(() => getAggregateAlerts());
  const open = useMemo(
    () => rankIncidents(alerts.value?.alerts ?? []),
    [alerts.value],
  );
  const [resolved, setResolved] = useState<readonly ResolvedIncident[]>([]);
  const previous = useRef<readonly OperatorAlert[] | null>(null);
  useEffect(() => {
    if (!alerts.value) return;
    const current = alerts.value.alerts;
    if (previous.current) {
      const before = previous.current;
      setResolved((list) =>
        updateResolved(list, before, current, Date.now(), RESOLVED_CAP),
      );
    }
    previous.current = current;
  }, [alerts.value]);

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected =
    open.find((alert) => alert.id === selectedId) ??
    resolved.find((entry) => entry.alert.id === selectedId)?.alert ??
    open[0] ??
    null;
  const names = useMemo(() => {
    const map = new Map<string, string>();
    for (const pipeline of live.state?.pipelines ?? []) map.set(pipeline.id, pipeline.name);
    for (const output of live.state?.outputs ?? []) map.set(output.id, output.name);
    return map;
  }, [live.state]);

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="text-xl font-semibold">Incidents</h2>
        <span className="badge badge-warning">{open.length} open</span>
        <span className="badge badge-ghost">
          {resolved.length} resolved since this page opened
        </span>
        <span className="ml-auto">
          <Freshness atMs={alerts.atMs} />
        </span>
      </div>
      {alerts.value === null ? (
        <p className="opacity-70">Loading alerts…</p>
      ) : (
        <div className="grid gap-4 lg:grid-cols-[18rem_1fr_22rem]">
          <Queue
            open={open}
            resolved={resolved}
            selectedId={selected?.id ?? null}
            names={names}
            onSelect={setSelectedId}
          />
          {selected ? (
            <>
              <Detail
                key={selected.id}
                alert={selected}
                isOpen={open.includes(selected)}
                names={names}
              />
              <Evidence alert={selected} names={names} />
            </>
          ) : (
            <p className="opacity-70 lg:col-span-2">
              No open incident. Every alert the engine raises appears here.
            </p>
          )}
        </div>
      )}
    </div>
  );
}

function affected(alert: OperatorAlert, names: Map<string, string>): string {
  const parts: string[] = [];
  if (alert.pipelineId) {
    parts.push(`pipeline ${names.get(alert.pipelineId) ?? alert.pipelineId}`);
  }
  if (alert.outputId) {
    parts.push(`output ${names.get(alert.outputId) ?? alert.outputId}`);
  }
  if (alert.stageId) parts.push(`stage ${alert.stageId}`);
  return parts.length > 0 ? parts.join(" · ") : "the engine";
}

function Queue(props: {
  open: readonly OperatorAlert[];
  resolved: readonly ResolvedIncident[];
  selectedId: string | null;
  names: Map<string, string>;
  onSelect: (id: string) => void;
}) {
  const now = useNow(1000);
  const item = (alert: OperatorAlert, sub: string) => (
    <li key={alert.id}>
      <button
        type="button"
        aria-pressed={alert.id === props.selectedId}
        className={`flex w-full flex-col items-start gap-1 rounded p-2 text-left ${
          alert.id === props.selectedId ? "bg-base-300" : "hover:bg-base-300/50"
        }`}
        onClick={() => props.onSelect(alert.id)}
      >
        <span className="flex w-full items-center justify-between gap-2">
          <b className="min-w-0 break-words text-sm">{alert.title}</b>
          <span
            className={`badge badge-sm shrink-0 ${alert.severity === "critical" ? "badge-error" : "badge-warning"}`}
          >
            {alert.severity}
          </span>
        </span>
        <span className="text-xs opacity-70">
          {affected(alert, props.names)} · {sub}
        </span>
      </button>
    </li>
  );
  return (
    <section className="card bg-base-200" aria-labelledby="incident-queue">
      <div className="card-body gap-2 p-3">
        <h3 id="incident-queue" className="card-title text-base">
          Open
          <span className="text-xs font-normal opacity-70">
            critical, then widest, then oldest
          </span>
        </h3>
        <ul className="space-y-1">
          {props.open.map((alert) =>
            item(
              alert,
              alert.firstSeen
                ? `open ${formatAge(Date.parse(alert.firstSeen), now)}`
                : "open",
            ),
          )}
        </ul>
        {props.resolved.length > 0 && (
          <>
            <h3 className="card-title mt-2 text-base">Resolved</h3>
            <ul className="space-y-1 opacity-80">
              {props.resolved.map(({ alert, resolvedAtMs }) =>
                item(alert, `resolved ${formatAge(resolvedAtMs, now)} ago`),
              )}
            </ul>
          </>
        )}
      </div>
    </section>
  );
}

function Detail(props: {
  alert: OperatorAlert;
  isOpen: boolean;
  names: Map<string, string>;
}) {
  const { alert } = props;
  const events = usePolled<LifecycleEventsSnapshot>(() =>
    getLifecycleEvents({ pipelineId: alert.pipelineId ?? null, limit: EVENT_LIMIT }),
  );
  const timeline = useMemo(
    () => (events.value ? incidentTimeline(alert, events.value.events) : null),
    [alert, events.value],
  );
  return (
    <section className="card bg-base-200" aria-labelledby="incident-detail">
      <div className="card-body gap-3 p-4">
        <div className="flex flex-wrap items-center gap-2">
          <h3 id="incident-detail" className="card-title text-base">
            {alert.title}
          </h3>
          <span className={`badge ${props.isOpen ? "badge-warning" : "badge-success"}`}>
            {props.isOpen ? "open" : "resolved"}
          </span>
          {alert.pipelineId && (
            <PlaceLink
              place={{ workspace: "operate", pipelineId: alert.pipelineId }}
              className="link ml-auto text-sm"
            >
              Operate this pipeline
            </PlaceLink>
          )}
        </div>
        <div className="rounded bg-base-100 p-3 text-sm">
          <b>Current judgement</b>
          <p>{alert.cause}</p>
        </div>
        <div className="flex items-center justify-between">
          <h4 className="font-medium">Timeline</h4>
          <Freshness atMs={events.atMs} />
        </div>
        {timeline === null ? (
          <p className="opacity-70">Loading events…</p>
        ) : timeline.length === 0 ? (
          <p className="opacity-70">
            No lifecycle event for {affected(alert, props.names)} since a minute
            before this opened.
          </p>
        ) : (
          <ol className="space-y-1 text-sm" aria-label="Incident timeline">
            {timeline.map((event) => (
              <li key={event.seq} className="grid grid-cols-[7rem_1fr] gap-2">
                <time className="tabular-nums opacity-70" dateTime={event.timestamp}>
                  {new Date(event.timestamp).toLocaleTimeString()}
                </time>
                <span>
                  {event.kind}
                  {event.outputId && ` · ${props.names.get(event.outputId) ?? event.outputId}`}
                  {event.phase && ` · ${event.phase}`}
                  {event.error && <span className="opacity-70"> · {event.error}</span>}
                </span>
              </li>
            ))}
          </ol>
        )}
      </div>
    </section>
  );
}

function Evidence(props: { alert: OperatorAlert; names: Map<string, string> }) {
  const packet = evidencePacket(props.alert);
  return (
    <section className="card bg-base-200" aria-labelledby="incident-evidence">
      <div className="card-body gap-3 p-4 text-sm">
        <h3 id="incident-evidence" className="card-title text-base">
          Evidence packet
        </h3>
        <div>
          <h4 className="text-xs uppercase opacity-70">Affected</h4>
          <p>{affected(props.alert, props.names)}</p>
        </div>
        <div>
          <h4 className="text-xs uppercase opacity-70">What failed</h4>
          <p>{packet.failed ?? "Not diagnosed by layer; see the evidence."}</p>
        </div>
        <div>
          <h4 className="text-xs uppercase opacity-70">What did not fail</h4>
          {packet.notFailed.length === 0 ? (
            <p className="opacity-70">Nothing ruled out.</p>
          ) : (
            <ul className="list-inside list-disc">
              {packet.notFailed.map((layer) => (
                <li key={layer}>{layer}</li>
              ))}
            </ul>
          )}
        </div>
        <div>
          <h4 className="text-xs uppercase opacity-70">Evidence</h4>
          <ul className="list-inside list-disc break-words">
            {packet.evidence.map((line) => (
              <li key={line}>{line}</li>
            ))}
          </ul>
        </div>
        <div className="rounded bg-base-100 p-3">
          <h4 className="text-xs uppercase opacity-70">Operator action</h4>
          <p>{packet.action}</p>
        </div>
      </div>
    </section>
  );
}
