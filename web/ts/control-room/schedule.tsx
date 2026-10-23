// Schedule: when the engine starts and stops destinations or file sources
// on its own. A week ahead by day (a list on phones), the events with their
// next time and how the last run went, and a form to add one. Times are
// entered and shown in this browser's local time; the engine keeps UTC.

import { useCallback, useEffect, useMemo, useState } from "react";

import { createSchedule, deleteSchedule, listSchedules, runSchedule, updateSchedule } from "../core/api.js";
import type { Schedule, ScheduleAction, ScheduleDraft, ScheduleRepeat } from "../core/api-types.js";
import { DataTable } from "../ui/data-table.js";
import type { Column } from "../ui/data-table.js";
import { PageHeader } from "../ui/page-header.js";
import { EmptyState, Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
import { ConfirmDelete } from "./confirm.js";
import { PlaceLink } from "./router.js";
import {
  ACTION_LABEL,
  REPEAT_LABEL,
  agenda,
  fromLocalInput,
  localDay,
  nextUp,
  resultTone,
  toLocalInput,
  untilLabel,
} from "./schedule-model.js";
import { useSession } from "./session.js";
import type { LiveSnapshot } from "./store.js";
import { useNow } from "./store.js";
import { When } from "./when-view.js";
import { absoluteTime, fullTime, zoneName } from "./when.js";

const DAYS = 7;
// 24-hour, as every time in the console; the zone is in the panel title.
const TIME = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
const DAY = new Intl.DateTimeFormat(undefined, { weekday: "short", day: "numeric", month: "short" });

const ACTION_TONE: Record<ScheduleAction, string> = {
  startOutputs: "border-l-success",
  stopOutputs: "border-l-error",
  startFile: "border-l-info",
  stopFile: "border-l-warning",
};

function ScheduleForm(props: { live: LiveSnapshot; onAdded: (schedule: Schedule) => void }) {
  const pipelines = props.live.state?.pipelines ?? [];
  const [pipelineId, setPipelineId] = useState("");
  const [name, setName] = useState("");
  const [action, setAction] = useState<ScheduleAction>("startOutputs");
  const [chosen, setChosen] = useState<string[]>([]);
  const [when, setWhen] = useState(() => toLocalInput(Math.ceil((Date.now() + 15 * 60_000) / 300_000) * 300_000));
  const [repeat, setRepeat] = useState<ScheduleRepeat>("once");
  const [busy, setBusy] = useState(false);
  const outputs = useMemo(
    () => (props.live.state?.outputs ?? []).filter((output) => output.pipelineId === pipelineId),
    [props.live.state, pipelineId],
  );
  const atMs = fromLocalInput(when);
  const forOutputs = action === "startOutputs" || action === "stopOutputs";
  const problem = !pipelineId
    ? "Choose a pipeline."
    : !name.trim()
      ? "Name the event."
      : Number.isNaN(atMs)
        ? "Choose when."
        : repeat === "once" && atMs <= Date.now()
          ? "A one-off event must be in the future."
          : null;

  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={async (event) => {
        event.preventDefault();
        if (problem) return;
        setBusy(true);
        const draft: ScheduleDraft = {
          pipelineId,
          name: name.trim(),
          action,
          outputIds: forOutputs ? chosen : [],
          atMs,
          repeat,
          enabled: true,
        };
        const created = await createSchedule(draft);
        setBusy(false);
        if (created) {
          props.onAdded(created);
          setName("");
          setChosen([]);
        }
      }}
    >
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-[1.2fr_1fr_1fr_1fr_9rem]">
        <label className="flex min-w-0 flex-col gap-1 text-sm">
          <span className="kicker">Event</span>
          <input className="input input-sm w-full" aria-label="Event name" placeholder="Sunday service" value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className="flex min-w-0 flex-col gap-1 text-sm">
          <span className="kicker">Pipeline</span>
          <select className="select select-sm w-full" aria-label="Pipeline to schedule" value={pipelineId} onChange={(e) => { setPipelineId(e.target.value); setChosen([]); }}>
            <option value="">Choose…</option>
            {pipelines.map((pipeline) => (
              <option key={pipeline.id} value={pipeline.id}>
                {pipeline.name}
              </option>
            ))}
          </select>
        </label>
        <label className="flex min-w-0 flex-col gap-1 text-sm">
          <span className="kicker">Do</span>
          <select className="select select-sm w-full" aria-label="Scheduled action" value={action} onChange={(e) => setAction(e.target.value as ScheduleAction)}>
            {(Object.keys(ACTION_LABEL) as ScheduleAction[]).map((value) => (
              <option key={value} value={value}>
                {ACTION_LABEL[value]}
              </option>
            ))}
          </select>
        </label>
        <label className="flex min-w-0 flex-col gap-1 text-sm">
          <span className="kicker">When (local time)</span>
          <input className="input input-sm w-full" type="datetime-local" aria-label="When" value={when} onChange={(e) => setWhen(e.target.value)} />
        </label>
        <label className="flex min-w-0 flex-col gap-1 text-sm">
          <span className="kicker">Repeat</span>
          <select className="select select-sm w-full" aria-label="Repeat" value={repeat} onChange={(e) => setRepeat(e.target.value as ScheduleRepeat)}>
            {(Object.keys(REPEAT_LABEL) as ScheduleRepeat[]).map((value) => (
              <option key={value} value={value}>
                {REPEAT_LABEL[value]}
              </option>
            ))}
          </select>
        </label>
      </div>
      {forOutputs && pipelineId && (
        <fieldset className="flex flex-col gap-1">
          <legend className="kicker mb-1">Destinations · {chosen.length === 0 ? "all of them" : `${chosen.length} chosen`}</legend>
          <div className="flex max-h-32 flex-wrap gap-1.5 overflow-y-auto">
            {outputs.length === 0 && <span className="text-sm text-base-content/65">This pipeline has no destinations yet.</span>}
            {outputs.map((output) => {
              const on = chosen.includes(output.id);
              return (
                <button
                  key={output.id}
                  type="button"
                  aria-pressed={on}
                  onClick={() => setChosen((current) => (on ? current.filter((id) => id !== output.id) : [...current, output.id]))}
                  className={`h-8 max-w-56 truncate rounded-full border px-3 text-sm ${on ? "border-primary bg-primary/15 font-semibold" : "border-base-content/15 hover:border-base-content/35"}`}
                >
                  {output.name}
                </button>
              );
            })}
          </div>
        </fieldset>
      )}
      <div className="flex flex-wrap items-center gap-3">
        <button type="submit" className="btn btn-primary btn-sm" disabled={busy || problem !== null}>
          Schedule it
        </button>
        {problem && <span className="text-sm text-base-content/70">{problem}</span>}
        <span className="ml-auto text-xs text-base-content/60">
          A time the engine was down for is skipped, never run late.
        </span>
      </div>
    </form>
  );
}

function Week(props: { schedules: readonly Schedule[]; pipelineNames: Map<string, string>; now: number }) {
  const days = useMemo(() => agenda(props.schedules, props.now, DAYS), [props.schedules, props.now]);
  const today = localDay(props.now);
  return (
    <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4 2xl:grid-cols-7" role="list" aria-label="The week ahead">
      {days.map((day) => (
        <section key={day.dayMs} role="listitem" aria-label={DAY.format(day.dayMs)} className={`min-w-0 rounded-box border bg-base-100 p-2 ${day.dayMs === today ? "border-primary/50" : "border-base-content/10"}`}>
          <h3 className={`kicker mb-2 ${day.dayMs === today ? "text-primary" : ""}`}>{day.dayMs === today ? "Today" : DAY.format(day.dayMs)}</h3>
          {day.items.length === 0 ? (
            <p className="text-xs text-base-content/50">Nothing</p>
          ) : (
            <ol className="flex flex-col gap-1.5">
              {day.items.slice(0, 12).map((item) => (
                <li
                  key={`${item.schedule.id}-${item.atMs}`}
                  className={`min-w-0 rounded-field border border-l-4 border-base-content/10 bg-base-200 px-2 py-1 text-xs ${ACTION_TONE[item.schedule.action]} ${item.atMs < props.now ? "opacity-55" : ""}`}
                >
                  <div className="tabular font-semibold">{TIME.format(item.atMs)}</div>
                  <div className="truncate" title={item.schedule.name}>{item.schedule.name}</div>
                  <div className="truncate text-base-content/65">
                    {ACTION_LABEL[item.schedule.action]} · {props.pipelineNames.get(item.schedule.pipelineId) ?? item.schedule.pipelineId}
                  </div>
                </li>
              ))}
              {day.items.length > 12 && <li className="text-xs text-base-content/60">+{day.items.length - 12} more</li>}
            </ol>
          )}
        </section>
      ))}
    </div>
  );
}

export function ScheduleView({ live }: { live: LiveSnapshot }) {
  const now = useNow(30_000);
  const canOperate = useSession().can("operator");
  const [schedules, setSchedules] = useState<Schedule[] | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const reload = useCallback(() => void listSchedules().then(setSchedules), []);
  useEffect(() => {
    reload();
    const timer = window.setInterval(reload, 30_000);
    return () => window.clearInterval(timer);
  }, [reload]);
  const pipelineNames = useMemo(() => new Map((live.state?.pipelines ?? []).map((pipeline) => [pipeline.id, pipeline.name])), [live.state]);
  const next = nextUp(schedules ?? []);

  const columns: Column<Schedule>[] = [
    {
      id: "event",
      header: "Event",
      width: "minmax(12rem,1.6fr)",
      sort: (schedule) => schedule.name,
      cell: (schedule) => (
        <span className="flex min-w-0 flex-col">
          <span className="truncate font-semibold">{schedule.name}</span>
          <span className="truncate text-xs text-base-content/65">
            {ACTION_LABEL[schedule.action]}
            {schedule.outputIds.length > 0 ? ` · ${schedule.outputIds.length} destination${schedule.outputIds.length === 1 ? "" : "s"}` : schedule.action.endsWith("Outputs") ? " · all destinations" : ""}
          </span>
        </span>
      ),
    },
    {
      id: "pipeline",
      header: "Pipeline",
      width: "minmax(9rem,1fr)",
      priority: 3,
      sort: (schedule) => pipelineNames.get(schedule.pipelineId) ?? schedule.pipelineId,
      cell: (schedule) => (
        <PlaceLink className="truncate hover:underline" place={{ workspace: "operate", pipelineId: schedule.pipelineId }}>
          {pipelineNames.get(schedule.pipelineId) ?? schedule.pipelineId}
        </PlaceLink>
      ),
    },
    {
      id: "next",
      header: "Next",
      width: "minmax(11rem,1.2fr)",
      sort: (schedule) => schedule.nextAtMs ?? Number.MAX_SAFE_INTEGER,
      cell: (schedule) =>
        !schedule.enabled ? (
          <StatusBadge tone="neutral">Paused</StatusBadge>
        ) : schedule.nextAtMs === null ? (
          <span className="text-sm text-base-content/60">Done</span>
        ) : (
          <span className="flex min-w-0 flex-col text-sm">
            <span className="tabular" title={fullTime(schedule.nextAtMs)}>{absoluteTime(schedule.nextAtMs, now)}</span>
            <span className="text-xs text-base-content/65">
              {untilLabel(schedule.nextAtMs, now)} · {REPEAT_LABEL[schedule.repeat].toLowerCase()}
            </span>
          </span>
        ),
    },
    {
      id: "last",
      header: "Last run",
      width: "minmax(12rem,1.6fr)",
      priority: 3,
      cell: (schedule) =>
        schedule.lastResult ? (
          <span className="flex min-w-0 items-center gap-2" title={schedule.lastResult}>
            <StatusBadge tone={resultTone(schedule.lastResult)}>{schedule.lastResult.split(":")[0]}</StatusBadge>
            <span className="truncate text-xs text-base-content/65">
              {schedule.lastRunMs ? <When atMs={schedule.lastRunMs} show="ago" /> : null}
              {schedule.lastRunMs ? " · " : ""}
              {schedule.lastResult.split(": ").slice(1).join(": ")}
            </span>
          </span>
        ) : (
          <span className="text-sm text-base-content/55">Not yet</span>
        ),
    },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      width: "minmax(13rem,1.3fr)",
      bare: true,
      cell: (schedule) =>
        canOperate ? (
          <span className="flex flex-wrap items-center gap-1">
            <button
              type="button"
              className="btn btn-ghost btn-xs"
              onClick={async () => {
                const ran = await runSchedule(schedule.id);
                setMessage(ran?.ok ? `${schedule.name}: ${ran.result}` : `${schedule.name} did not run.`);
                reload();
              }}
            >
              Run now
            </button>
            <button
              type="button"
              className="btn btn-ghost btn-xs"
              onClick={async () => {
                const { id: _id, lastRunMs: _l, lastResult: _r, createdAtMs: _c, nextAtMs: _n, ...draft } = schedule;
                if (await updateSchedule(schedule.id, { ...draft, enabled: !schedule.enabled })) reload();
              }}
            >
              {schedule.enabled ? "Pause" : "Resume"}
            </button>
            <ConfirmDelete
              label={`Delete ${schedule.name}`}
              question="Delete this event?"
              onDelete={async () => {
                const deleted = await deleteSchedule(schedule.id);
                if (deleted) reload();
                return deleted;
              }}
            />
          </span>
        ) : null,
    },
  ];

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow={`Operate · Schedule · ${(schedules ?? []).filter((schedule) => schedule.enabled).length} active`}
        title={next && next.nextAtMs !== null ? `Next: ${next.name} ${untilLabel(next.nextAtMs, now)}` : "Nothing scheduled"}
        subtitle="The engine starts and stops destinations and file sources at these times, with or without a dashboard open."
      />
      {message && (
        <p role="status" className="rounded-box border border-base-content/10 bg-base-200 px-4 py-2 text-sm">
          {message}
        </p>
      )}
      <Panel id="schedule-week" title="The week ahead" subtitle={`Times in ${zoneName(Date.now())}`}>
        <Week schedules={schedules ?? []} pipelineNames={pipelineNames} now={now} />
      </Panel>
      {canOperate && (
        <Panel id="schedule-add" title="Schedule an event">
          <ScheduleForm live={live} onAdded={(schedule) => setSchedules((current) => [...(current ?? []), schedule])} />
        </Panel>
      )}
      <Panel id="schedule-events" title="Events" flush>
        <DataTable
          label="Scheduled events"
          rows={schedules ?? []}
          columns={canOperate ? columns : columns.filter((column) => column.id !== "actions")}
          rowKey={(schedule) => schedule.id}
          rowHeight={56}
          cardHeight={190}
          initialSort={{ id: "next", dir: "asc" }}
          empty={<EmptyState title={schedules === null ? "Loading…" : "No events yet."} detail="Schedule a go-live, a stop, or a file source above." />}
        />
      </Panel>
    </div>
  );
}
