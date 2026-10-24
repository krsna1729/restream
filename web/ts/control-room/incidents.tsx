import { useMemo, useState } from "react";
import type { ReactNode } from "react";

import type { LifecycleEventsSnapshot, OperatorAlert, ResolvedAlert } from "../core/api-types.js";
import type { FleetHistoryWindow } from "../core/generated/runtime-state.js";
import { degradedTotal } from "./fleet-history.js";
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
import { ChipGroup } from "../ui/controls.js";
import { PageHeader } from "../ui/page-header.js";
import { EmptyState, Panel } from "../ui/panel.js";
import { RawJson } from "../ui/raw-json.js";
import { TimeSeries } from "../ui/time-series.js";
import { StatusBadge } from "../ui/tone.js";
import { useNow } from "./store.js";
import type { LiveSnapshot } from "./store.js";
import { When } from "./when-view.js";
import { absoluteTime, duration } from "./when.js";
import { incidentWindow } from "./stability/replay-window.js";

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

  const critical = open.filter((alert) => alert.severity === "critical").length;
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="Incidents"
        title={open.length === 0 ? "No open incident" : `${open.length} open incident${open.length === 1 ? "" : "s"}`}
        tone={critical > 0 ? "bad" : open.length > 0 ? "warn" : "ok"}
        subtitle={`${critical} critical · ${open.length - critical} warning · ${resolved.length} resolved in the last ${resolvedWindow.label}`}
        actions={
          <>
            <label className="flex items-center gap-2 text-sm text-base-content/70">
              Resolved window
              <select className="select select-sm w-24" value={windowIndex} onChange={(event) => setWindowIndex(Number(event.target.value))}>
                {RESOLVED_WINDOWS.map((option, index) => (
                  <option key={option.label} value={index}>
                    {option.label}
                  </option>
                ))}
              </select>
            </label>
            <Freshness atMs={alerts.atMs} />
          </>
        }
      />
      {alerts.value?.resolvedCompleteFrom !== undefined &&
        alerts.atMs !== null &&
        resolvedMayBeIncomplete(
          alerts.atMs - resolvedWindow.ms,
          alerts.value.resolvedCompleteFrom,
        ) && (
          <p className="text-xs text-base-content/70">
            The engine's resolved history is complete only from{" "}
            <When atMs={new Date(alerts.value.resolvedCompleteFrom).getTime()} show="at" /> (it
            restarted then, or dropped older ones): earlier closings in this
            window may be missing.
          </p>
        )}
      {alerts.value === null ? (
        <EmptyState title="Loading alerts…" />
      ) : (
        <div className="grid gap-4 lg:grid-cols-[20rem_minmax(0,1fr)] 2xl:grid-cols-[20rem_minmax(0,1fr)_24rem]">
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
            <div className="lg:col-span-2">
              <EmptyState tone="ok" title="All clear" detail="No open incident. Every alert the engine raises appears here, with its evidence." />
            </div>
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
  const [severity, setSeverity] = useState<"all" | "critical" | "warning">("all");
  const shownOpen = props.open.filter((alert) => severity === "all" || alert.severity === severity);
  const item = (alert: OperatorAlert, sub: ReactNode) => (
    <li key={alert.id}>
      <button
        type="button"
        aria-pressed={alert.id === props.selectedId}
        className={`flex w-full flex-col items-start gap-1 border-l-2 px-3 py-2 text-left transition-colors ${alert.severity === "critical" ? "border-l-error" : "border-l-warning"} ${
          alert.id === props.selectedId ? "bg-primary/10" : "hover:bg-base-content/5"
        }`}
        onClick={() => props.onSelect(alert.id)}
      >
        <span className="flex w-full items-start justify-between gap-2">
          <b className="min-w-0 text-sm break-words">{alert.title}</b>
          <StatusBadge tone={alert.severity === "critical" ? "bad" : "warn"} className="shrink-0">
            {alert.severity}
          </StatusBadge>
        </span>
        <span className="text-xs text-base-content/70">{sub}</span>
      </button>
    </li>
  );
  const critical = props.open.filter((alert) => alert.severity === "critical").length;
  return (
    <Panel id="incident-queue" title="Open" subtitle="critical, widest, oldest" flush>
      <div className="border-b border-base-content/10 px-3 py-2">
        <ChipGroup<"all" | "critical" | "warning">
          label="Severity"
          value={severity}
          onChange={setSeverity}
          chips={[
            { value: "all", label: "All", count: props.open.length },
            { value: "critical", label: "Critical", count: critical, alert: true },
            { value: "warning", label: "Warning", count: props.open.length - critical },
          ]}
        />
      </div>
      <div className="scrollbar-thin max-h-[70dvh] overflow-y-auto">
        <ul className="divide-y divide-base-content/5">
          {shownOpen.map((alert) =>
            item(
              alert,
              alert.firstSeen ? (
                <>
                  open {duration(now - Date.parse(alert.firstSeen))} · since <When atMs={Date.parse(alert.firstSeen)} show="at" />
                </>
              ) : (
                "open"
              ),
            ),
          )}
        </ul>
        {props.resolved.length > 0 && (
          <>
            <h3 className="kicker border-t border-base-content/10 px-3 pt-3 pb-1">Resolved</h3>
            <ul className="divide-y divide-base-content/5">
              {props.resolved.map(({ alert, resolvedAt }) =>
                item(
                  alert,
                  <>
                    resolved <When atMs={Date.parse(resolvedAt)} show="ago" />
                  </>,
                ),
              )}
            </ul>
          </>
        )}
      </div>
    </Panel>
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
    <Panel
      id="incident-detail"
      title={alert.title}
      stripe={isOpen ? (alert.severity === "critical" ? "bad" : "warn") : "ok"}
      actions={
        <>
          <StatusBadge tone={isOpen ? "warn" : "ok"} dot>
            {isOpen ? "open" : "resolved"}
          </StatusBadge>
          {alert.pipelineId && (
            <>
              <PlaceLink place={{ workspace: "logs", pipelineId: alert.pipelineId, outputId: alert.outputId ?? null }} className="btn btn-sm btn-outline">
                {alert.outputId ? "Destination logs" : "Pipeline logs"}
              </PlaceLink>
              {alert.firstSeen && (
                <PlaceLink
                  place={{
                    workspace: "stability",
                    pipelineId: alert.pipelineId,
                    replay: incidentWindow(Date.parse(alert.firstSeen), props.resolvedAt ? Date.parse(props.resolvedAt) : null),
                  }}
                  className="btn btn-sm btn-outline"
                >
                  Replay
                </PlaceLink>
              )}
              <PlaceLink place={{ workspace: "operate", pipelineId: alert.pipelineId }} className="btn btn-sm btn-outline">
                Operate pipeline
              </PlaceLink>
            </>
          )}
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <div className="rounded-box bg-base-100 p-3 text-sm">
          <b>{isOpen ? "Current judgement" : "Last judgement"}</b>
          <p>{alert.cause}</p>
        </div>
        <Correlation fleet={fleet} fromMs={span.fromMs} marks={incidentMarks(alert, props.resolvedAt)} />
        <div className="flex items-center justify-between">
          <h4 className="kicker">Timeline</h4>
          <Freshness atMs={events.atMs} />
        </div>
        {incomplete && (
          <p className="text-xs text-base-content/70">
            The engine keeps only its most recent events: part of the minute
            before this opened may no longer be kept.
          </p>
        )}
        {timeline === null ? (
          <p className="text-sm text-base-content/70">Loading events…</p>
        ) : timeline.length === 0 ? (
          <p className="text-sm text-base-content/70">
            No kept lifecycle event for {affected(alert, props.names)} since a
            minute before this opened.
          </p>
        ) : (
          <ol className="space-y-1 text-sm" aria-label="Incident timeline">
            {timeline.map((event) => (
              <li key={event.seq} className="grid grid-cols-[7rem_1fr] gap-2">
                <When atMs={Date.parse(event.timestamp)} show="at" seconds noZone className="text-base-content/70" />
                <span>
                  {event.kind}
                  {event.outputId && ` · ${props.names.get(event.outputId) ?? event.outputId}`}
                  {event.phase && ` · ${event.phase}`}
                  {event.error && <span className="text-base-content/70"> · {event.error}</span>}
                </span>
              </li>
            ))}
          </ol>
        )}
      </div>
    </Panel>
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
        <h4 className="kicker">Correlation</h4>
        <Freshness atMs={props.fleet.atMs} staleAfterMs={props.fleet.staleAfterMs} />
      </div>
      <p className="text-xs text-base-content/70">
        The engine's fleet samples from a minute before the incident opened
        {first && ` (first at ${absoluteTime(first.atMs, Date.now(), { seconds: true })})`}; the
        cursor marks when it opened and, once it did, closed.
      </p>
      {kept !== null && props.fromMs <= kept.completeFromMs && (
        <p className="text-xs text-base-content/70">
          The engine's fleet history is complete only from{" "}
          <When atMs={kept.completeFromMs} show="at" /> (it restarted then, or
          dropped older samples): the chart starts there.
        </p>
      )}
      {kept === null ? (
        <p className="text-sm text-base-content/70">Loading the fleet history…</p>
      ) : samples.length === 0 ? (
        <p className="text-sm text-base-content/70">No fleet sample in this span yet.</p>
      ) : (
        <div className="flex flex-col gap-4">
          <TimeSeries
            samples={samples}
            label="Delivering and headroom around the incident"
            top={100}
            height={120}
            marks={props.marks.map((atMs, index) => ({ atMs, kind: "incident", label: index === 0 ? "opened" : "closed", tone: index === 0 ? ("bad" as const) : ("ok" as const) }))}
            bands={props.marks.length > 0 ? [{ fromMs: props.marks[0], toMs: props.marks[1] ?? Number.POSITIVE_INFINITY, tone: "warn" as const }] : []}
            series={[
              { id: "delivering", label: "Delivering %", pick: (sample) => sample.deliveringPercent, color: "text-success", area: true, format: (value) => `${value.toFixed(1)}%` },
              { id: "headroom", label: "Headroom %", pick: (sample) => sample.headroomPercent, color: "text-info", format: (value) => `${value.toFixed(0)}%` },
            ]}
          />
          <TimeSeries
            samples={samples}
            label="Degraded outputs over time"
            height={70}
            marks={props.marks.map((atMs) => ({ atMs, kind: "incident", tone: "warn" as const }))}
            series={[{ id: "degraded", label: "Degraded destinations", pick: degradedTotal, color: "text-warning", area: true }]}
          />
        </div>
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
    <Panel
      id="incident-evidence"
      title="Evidence packet"
      className="lg:col-span-2 2xl:col-span-1"
      actions={
        <button type="button" className="btn btn-sm" onClick={download}>
          Export evidence
        </button>
      }
    >
      <div className="flex flex-col gap-4 text-sm">
        <div>
          <h4 className="kicker">Affected</h4>
          <p>{affected(props.alert, props.names)}</p>
        </div>
        <div>
          <h4 className="kicker">What failed</h4>
          <p>{packet.failed ?? "Not diagnosed by layer; see the evidence."}</p>
        </div>
        <div>
          <h4 className="kicker">What did not fail</h4>
          {packet.notFailed.length === 0 ? (
            <p className="text-sm text-base-content/70">Nothing ruled out.</p>
          ) : (
            <ul className="list-inside list-disc">
              {packet.notFailed.map((layer) => (
                <li key={layer}>{layer}</li>
              ))}
            </ul>
          )}
        </div>
        <div>
          <h4 className="kicker">Evidence</h4>
          <ul className="space-y-1 break-words">
            {packet.evidence.map((line, index) => (
              // Two alerts may repeat a line: the position keeps keys unique.
              <li key={`${index}:${line.signal}`} className="flex items-start gap-2">
                <span
                  className={`shrink-0 rounded-full px-1.5 text-[0.7rem] font-semibold ${line.confidence === "measured" ? "bg-info/15 text-info" : "bg-base-content/10 text-base-content/80"}`}
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
        <div className="rounded-box border-l-4 border-accent bg-base-100 p-3">
          <h4 className="kicker">Operator action</h4>
          <p className="mt-1">{packet.action}</p>
        </div>
        <RawJson value={props.alert} label="alert payload" />
      </div>
    </Panel>
  );
}
