import { useMemo, useState } from "react";

import type { LifecycleEventsSnapshot, OperatorAlert, ResolvedAlert } from "../core/api-types.js";
import type { FleetHistoryWindow } from "../core/generated/runtime-state.js";
import { degradedTotal } from "./fleet-history.js";
import { formatAge } from "./format.js";
import {
  RESOLVED_WINDOWS,
  correlationWindow,
  evidenceExport,
  evidencePacket,
  incidentMarks,
  incidentTimeline,
  rankIncidents,
  resolvedMayBeIncomplete,
  timelineMayBeIncomplete,
  visibleResolved,
} from "./incidents-model.js";
import { Freshness, useFleetHistory, useLifecycleEvents, useOpenAlerts } from "./polled.js";
import type { Polled } from "./polled.js";
import { PlaceLink } from "./router.js";
import { Sparkline } from "./sparkline.js";
import { useNow } from "./store.js";
import type { LiveSnapshot } from "./store.js";

/** Open incidents, what failed and what did not, and what to do. */
export function Incidents({ live }: { live: LiveSnapshot }) {
  const [windowIndex, setWindowIndex] = useState(1);
  const resolvedWindow = RESOLVED_WINDOWS[windowIndex] ?? RESOLVED_WINDOWS[0];
  const alerts = useOpenAlerts(resolvedWindow.ms);
  const open = useMemo(
    () => rankIncidents(alerts.value?.alerts ?? []),
    [alerts.value],
  );
  const resolved = useMemo(
    () => visibleResolved(alerts.value?.resolved ?? [], open),
    [alerts.value, open],
  );

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selectedResolved = open.some((alert) => alert.id === selectedId)
    ? undefined
    : resolved.find((entry) => entry.alert.id === selectedId);
  const selected =
    open.find((alert) => alert.id === selectedId) ??
    selectedResolved?.alert ??
    open[0] ??
    null;
  const resolvedAt = selected === selectedResolved?.alert ? selectedResolved.resolvedAt : null;
  const events = useLifecycleEvents(selected?.pipelineId ?? null);
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
          {resolved.length} resolved in the last {resolvedWindow.label}
        </span>
        <label className="flex items-center gap-1 text-sm">
          Resolved window
          <select
            className="select select-bordered select-xs"
            value={windowIndex}
            onChange={(event) => setWindowIndex(Number(event.target.value))}
          >
            {RESOLVED_WINDOWS.map((option, index) => (
              <option key={option.label} value={index}>
                {option.label}
              </option>
            ))}
          </select>
        </label>
        <span className="ml-auto">
          <Freshness atMs={alerts.atMs} />
        </span>
      </div>
      {alerts.value?.resolvedCompleteFrom !== undefined &&
        alerts.atMs !== null &&
        resolvedMayBeIncomplete(
          alerts.atMs - resolvedWindow.ms,
          alerts.value.resolvedCompleteFrom,
        ) && (
          <p className="text-xs opacity-70">
            The engine's resolved history is complete only from{" "}
            {new Date(alerts.value.resolvedCompleteFrom).toLocaleString()} (it
            restarted then, or dropped older ones): earlier closings in this
            window may be missing.
          </p>
        )}
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
                resolvedAt={resolvedAt}
                events={events}
                names={names}
              />
              <Evidence
                alert={selected}
                resolvedAt={resolvedAt}
                events={events.value}
                names={names}
              />
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
  resolved: readonly ResolvedAlert[];
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
              {props.resolved.map(({ alert, resolvedAt }) =>
                item(alert, `resolved ${formatAge(Date.parse(resolvedAt), now)} ago`),
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
  resolvedAt: string | null;
  events: Polled<LifecycleEventsSnapshot>;
  names: Map<string, string>;
}) {
  const { alert, events } = props;
  const isOpen = props.resolvedAt === null;
  const timeline = useMemo(
    () => (events.value ? incidentTimeline(alert, events.value.events) : null),
    [alert, events.value],
  );
  const incomplete =
    events.value !== null && timelineMayBeIncomplete(alert, events.value.events);
  const span = correlationWindow(alert, props.resolvedAt);
  const fleet = useFleetHistory(span.fromMs, span.toMs);
  return (
    <section className="card bg-base-200" aria-labelledby="incident-detail">
      <div className="card-body gap-3 p-4">
        <div className="flex flex-wrap items-center gap-2">
          <h3 id="incident-detail" className="card-title text-base">
            {alert.title}
          </h3>
          <span className={`badge ${isOpen ? "badge-warning" : "badge-success"}`}>
            {isOpen ? "open" : "resolved"}
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
          <b>{isOpen ? "Current judgement" : "Last judgement"}</b>
          <p>{alert.cause}</p>
        </div>
        <Correlation fleet={fleet} fromMs={span.fromMs} marks={incidentMarks(alert, props.resolvedAt)} />
        <div className="flex items-center justify-between">
          <h4 className="font-medium">Timeline</h4>
          <Freshness atMs={events.atMs} />
        </div>
        {incomplete && (
          <p className="text-xs opacity-70">
            The engine keeps only its most recent events: part of the minute
            before this opened may no longer be kept.
          </p>
        )}
        {timeline === null ? (
          <p className="opacity-70">Loading events…</p>
        ) : timeline.length === 0 ? (
          <p className="opacity-70">
            No kept lifecycle event for {affected(alert, props.names)} since a
            minute before this opened.
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

/** The fleet's signals around the incident from the engine's history,
 * cursors at its open and close. */
function Correlation(props: {
  fleet: Polled<FleetHistoryWindow>;
  fromMs: number;
  marks: readonly number[];
}) {
  const kept = props.fleet.value;
  const samples = kept?.samples ?? [];
  const first = samples[0];
  return (
    <div aria-label="Correlation">
      <div className="flex items-center justify-between">
        <h4 className="font-medium">Correlation</h4>
        <Freshness atMs={props.fleet.atMs} staleAfterMs={props.fleet.staleAfterMs} />
      </div>
      <p className="text-xs opacity-70">
        The engine's fleet samples from a minute before the incident opened
        {first && ` (first at ${new Date(first.atMs).toLocaleTimeString()})`}; the
        cursor marks when it opened and, once it did, closed.
      </p>
      {kept !== null && props.fromMs <= kept.completeFromMs && (
        <p className="text-xs opacity-70">
          The engine's fleet history is complete only from{" "}
          {new Date(kept.completeFromMs).toLocaleString()} (it restarted then, or
          dropped older samples): the chart starts there.
        </p>
      )}
      {kept === null ? (
        <p className="opacity-70">Loading the fleet history…</p>
      ) : samples.length === 0 ? (
        <p className="opacity-70">No fleet sample in this span yet.</p>
      ) : (
        <dl className="grid grid-cols-[7rem_1fr] items-center gap-x-2 text-sm">
          <dt>Delivering</dt>
          <dd>
            <Sparkline
              history={samples}
              pick={(sample) => sample.deliveringPercent}
              label="Delivering over time"
              top={100}
              marks={props.marks}
            />
          </dd>
          <dt>Headroom</dt>
          <dd>
            <Sparkline
              history={samples}
              pick={(sample) => sample.headroomPercent}
              label="Headroom over time"
              top={100}
              marks={props.marks}
            />
          </dd>
          <dt>Degraded</dt>
          <dd>
            <Sparkline
              history={samples}
              pick={degradedTotal}
              label="Degraded outputs over time"
              marks={props.marks}
            />
          </dd>
        </dl>
      )}
    </div>
  );
}

function Evidence(props: {
  alert: OperatorAlert;
  resolvedAt: string | null;
  events: LifecycleEventsSnapshot | null;
  names: Map<string, string>;
}) {
  const packet = evidencePacket(props.alert);
  const span = correlationWindow(props.alert, props.resolvedAt);
  // The chart's query: one fetch for both.
  const fleet = useFleetHistory(span.fromMs, span.toMs);
  const download = () => {
    const record = evidenceExport({
      alert: props.alert,
      resolvedAt: props.resolvedAt,
      events: props.events?.events ?? null,
      history: fleet.value?.samples ?? [],
      nowMs: Date.now(),
    });
    const url = URL.createObjectURL(
      new Blob([JSON.stringify(record, null, 2)], { type: "application/json" }),
    );
    const link = document.createElement("a");
    link.href = url;
    link.download = `incident-${props.alert.id.replace(/[^A-Za-z0-9._-]+/g, "_")}.json`;
    link.click();
    // Some browsers read the URL after `click` returns.
    setTimeout(() => URL.revokeObjectURL(url), 0);
  };
  return (
    <section className="card bg-base-200" aria-labelledby="incident-evidence">
      <div className="card-body gap-3 p-4 text-sm">
        <div className="flex items-center justify-between gap-2">
          <h3 id="incident-evidence" className="card-title text-base">
            Evidence packet
          </h3>
          <button type="button" className="btn btn-xs" onClick={download}>
            Export evidence
          </button>
        </div>
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
          <ul className="space-y-1 break-words">
            {packet.evidence.map((line, index) => (
              // Two alerts may repeat a line: the position keeps keys unique.
              <li key={`${index}:${line.signal}`} className="flex items-start gap-2">
                <span
                  className={`badge badge-xs shrink-0 ${line.confidence === "measured" ? "badge-info" : "badge-ghost"}`}
                  title={
                    line.confidence === "measured"
                      ? "A reading the engine took."
                      : "A conclusion the engine's rules drew from readings."
                  }
                >
                  {line.confidence}
                </span>
                <span>{line.signal}</span>
              </li>
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
