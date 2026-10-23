// Channels: every destination as a channel card, grouped by platform, with
// one-click go-live / stop per channel and bulk actions over the filtered
// set. Stored URLs (for the platform) come from /settings, read once and
// after each bulk run; state and rates come from the live store.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { getConfig, startOut, stopOut } from "../core/api.js";
import { useSession } from "./session.js";
import type { ConfigData } from "../types.js";
import { ChipGroup, SearchField } from "../ui/controls.js";
import { Icon } from "../ui/icons.js";
import { Kpi, KpiRow, Meter } from "../ui/kpi.js";
import { PageHeader } from "../ui/page-header.js";
import { DetailToggle, useSectionDetail } from "../ui/prefs.js";
import { EmptyState, Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";
import { buildChannels, bulkTargets, impactWords, platformSummaries, runBulk } from "./channels-model.js";
import type { BulkAction, Channel, ChannelState } from "./channels-model.js";
import { formatBps } from "./format.js";
import { PlaceLink } from "./router.js";
import { PlatformMark } from "./platform-mark.js";
import { useNow } from "./store.js";
import type { LiveSnapshot } from "./store.js";
import { LogsLink } from "./logs-link.js";
import { outputTrouble, troubleTitle } from "./trouble-model.js";
import { retryWords } from "./when.js";
import { Since } from "./when-view.js";
import { NextEvent } from "./next-event.js";

const STATE_TONE: Record<ChannelState, Tone> = {
  live: "ok",
  starting: "info",
  waiting: "info",
  retrying: "warn",
  degraded: "warn",
  failed: "bad",
  off: "neutral",
};

const STATE_LABEL: Record<ChannelState, string> = {
  live: "live",
  starting: "starting",
  waiting: "waiting for source",
  retrying: "retrying",
  degraded: "degraded",
  failed: "gave up",
  off: "off",
};

type StateFilter = "all" | "live" | "trouble" | "off";

const PAGE = 60;
/** Bulk actions in flight at once: enough to be quick, few enough not to
 * stampede the engine's reconciler. */
const BULK_CONCURRENCY = 6;

export function Channels({ live }: { live: LiveSnapshot }) {
  const canOperate = useSession().can("operator");
  const [detail, setDetail] = useSectionDetail("channels");
  const [config, setConfig] = useState<ConfigData | null>(null);
  // Settings hold stream keys: a viewer neither reads them nor needs them.
  const reload = useCallback(() => void (canOperate ? getConfig().then(setConfig) : setConfig(null)), [canOperate]);
  useEffect(reload, [reload]);
  const urls = useMemo(() => new Map((config?.outputs ?? []).map((output) => [output.id, output.url])), [config]);
  const watchUrls = useMemo(() => new Map((config?.outputs ?? []).flatMap((output) => (output.monitoringUrl ? [[output.id, output.monitoringUrl] as const] : []))), [config]);
  const channels = useMemo(() => (live.state ? buildChannels(live.state, urls) : null), [live.state, urls]);
  const [platform, setPlatform] = useState<string>("all");
  const [state, setState] = useState<StateFilter>("all");
  const [pipeline, setPipeline] = useState<string>("all");
  const [query, setQuery] = useState("");
  const [shown, setShown] = useState(PAGE);
  const [pending, setPending] = useState<BulkAction | null>(null);
  const [progress, setProgress] = useState<{ action: BulkAction; done: number; failed: number; total: number } | null>(null);
  const [undo, setUndo] = useState<Undo | null>(null);

  const summaries = useMemo(() => (channels ? platformSummaries(channels) : []), [channels]);
  const filtered = useMemo(() => {
    if (!channels) return [];
    const needle = query.trim().toLowerCase();
    const rank: Record<ChannelState, number> = { failed: 0, retrying: 1, degraded: 2, waiting: 3, starting: 4, live: 5, off: 6 };
    return channels
      .filter(
        (channel) =>
          (platform === "all" || channel.platform.id === platform) &&
          (pipeline === "all" || channel.output.pipelineId === pipeline) &&
          (state === "all" ||
            (state === "live" && channel.state === "live") ||
            (state === "off" && channel.state === "off") ||
            (state === "trouble" && channel.state !== "live" && channel.state !== "off")) &&
          (needle === "" ||
            channel.output.name.toLowerCase().includes(needle) ||
            channel.pipelineName.toLowerCase().includes(needle)),
      )
      .sort((a, b) => rank[a.state] - rank[b.state] || a.output.name.localeCompare(b.output.name));
  }, [channels, platform, pipeline, state, query]);

  if (!channels || !live.state) return <EmptyState title="Waiting for runtime state…" />;

  const total = channels.length;
  const liveCount = channels.filter((channel) => channel.state === "live").length;
  const trouble = channels.filter((channel) => channel.state !== "live" && channel.state !== "off").length;
  const off = total - liveCount - trouble;
  const counts = { all: filtered.length, start: bulkTargets(filtered, "start").length, stop: bulkTargets(filtered, "stop").length, retry: bulkTargets(filtered, "retry").length };

  const run = async (action: BulkAction) => {
    const targets = bulkTargets(filtered, action);
    setPending(null);
    setUndo(null);
    setProgress({ action, done: 0, failed: 0, total: targets.length });
    const stopped: OutputRef[] = [];
    await runBulk(
      targets,
      async (channel) => {
        const { pipelineId, id } = channel.output;
        if (action === "start") return (await startOut(pipelineId, id)) !== null;
        if (action === "stop") {
          const ok = (await stopOut(pipelineId, id)) !== null;
          if (ok) stopped.push({ pipelineId, id });
          return ok;
        }
        // Retry: a stop and a start reset the engine's retry budget.
        await stopOut(pipelineId, id);
        return (await startOut(pipelineId, id)) !== null;
      },
      BULK_CONCURRENCY,
      (done, failed) => setProgress({ action, done, failed, total: targets.length }),
    );
    if (stopped.length > 0) setUndo({ label: `Stopped ${stopped.length.toLocaleString()} channel${stopped.length === 1 ? "" : "s"}`, items: stopped, untilMs: Date.now() + UNDO_MS });
    reload();
  };

  const scopeLabel =
    platform === "all" && pipeline === "all" && state === "all" && !query ? "every channel" : `${filtered.length.toLocaleString()} filtered channels`;

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow={`Channels · ${summaries.length} platforms`}
        title={`${liveCount.toLocaleString()} of ${(total - off).toLocaleString()} wanted channels live`}
        tone={trouble > 0 ? "warn" : liveCount > 0 ? "ok" : "neutral"}
        subtitle={`${trouble.toLocaleString()} need attention · ${off.toLocaleString()} off · bulk actions act on ${scopeLabel}`}
        actions={
          canOperate && <div className="flex flex-wrap gap-2">
            <button type="button" className="btn btn-accent btn-sm" disabled={counts.start === 0 || progress !== null && progress.done < progress.total} onClick={() => setPending("start")}>
              <Icon name="play" className="size-3.5" /> Go live ({counts.start.toLocaleString()})
            </button>
            <button type="button" className="btn btn-sm" disabled={counts.retry === 0 || progress !== null && progress.done < progress.total} onClick={() => setPending("retry")}>
              <Icon name="refresh" className="size-3.5" /> Retry failed ({counts.retry.toLocaleString()})
            </button>
            <button type="button" className="btn btn-sm btn-ghost border-base-content/20" disabled={counts.stop === 0 || progress !== null && progress.done < progress.total} onClick={() => setPending("stop")}>
              <Icon name="stop" className="size-3.5" /> Stop ({counts.stop.toLocaleString()})
            </button>
          </div>
        }
      />
      <NextEvent pipelineId={pipeline === "all" ? null : pipeline} />
      {undo && <UndoBar undo={undo} onDone={() => setUndo(null)} onUndone={reload} />}
      {progress && (
        <div role="status" className="flex flex-wrap items-center gap-3 rounded-box border border-base-content/10 bg-base-200 px-4 py-3 text-sm">
          <span className="font-medium">
            {progress.action === "start" ? "Going live" : progress.action === "stop" ? "Stopping" : "Retrying"} · {progress.done} / {progress.total}
            {progress.failed > 0 && <span className="text-error"> · {progress.failed} refused</span>}
          </span>
          <Meter value={progress.total === 0 ? 1 : progress.done / progress.total} tone={progress.failed > 0 ? "warn" : "ok"} label="Bulk progress" className="max-w-xs flex-1" />
          {progress.done === progress.total && (
            <button type="button" className="btn btn-ghost btn-xs" onClick={() => setProgress(null)}>
              Dismiss
            </button>
          )}
        </div>
      )}
      <KpiRow max={6}>
        {summaries.slice(0, 6).map((summary) => (
          <Kpi
            key={summary.platform.id}
            label={summary.platform.label}
            value={`${summary.live} / ${summary.total}`}
            meter={summary.total === 0 ? null : summary.live / summary.total}
            meterTone={summary.troubled > 0 ? "warn" : "ok"}
            foot={summary.troubled > 0 ? `${summary.troubled} need attention` : "all wanted live"}
            onClick={() => setPlatform(platform === summary.platform.id ? "all" : summary.platform.id)}
          />
        ))}
      </KpiRow>
      <Panel
        flush
        id="channels-list"
        title="Channels"
        subtitle={`${filtered.length.toLocaleString()} of ${total.toLocaleString()} · trouble first`}
        actions={
          <>
            <select className="select select-sm w-full sm:w-48" aria-label="Pipeline" value={pipeline} onChange={(event) => setPipeline(event.target.value)}>
              <option value="all">All pipelines</option>
              {live.state.pipelines.map((candidate) => (
                <option key={candidate.id} value={candidate.id}>
                  {candidate.name}
                </option>
              ))}
            </select>
            <SearchField value={query} onChange={setQuery} placeholder="Channel or pipeline" label="Search channels" className="w-full sm:w-56" />
            <DetailToggle open={detail} onChange={setDetail} />
          </>
        }
      >
        <div className="flex flex-col gap-2 border-b border-base-content/10 px-4 py-2">
          <ChipGroup<StateFilter>
            label="State"
            value={state}
            onChange={setState}
            chips={[
              { value: "all", label: "All", count: total },
              { value: "trouble", label: "Needs attention", count: trouble, alert: true },
              { value: "live", label: "Live", count: liveCount },
              { value: "off", label: "Off", count: off },
            ]}
          />
          <div className="min-w-0">
            <ChipGroup<string>
              label="Platform"
              value={platform}
              onChange={setPlatform}
              chips={[{ value: "all", label: "Any platform" }, ...summaries.map((summary) => ({ value: summary.platform.id, label: summary.platform.label, count: summary.total }))]}
            />
          </div>
        </div>
        {filtered.length === 0 ? (
          <EmptyState title="No channel matches" detail="Clear a filter, or add destinations from a pipeline's Operate view." />
        ) : (
          <>
            <ul className="grid gap-3 p-4 sm:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-4">
              {filtered.slice(0, shown).map((channel) => (
                <ChannelCard
                  key={channel.output.id}
                  channel={channel}
                  detail={detail}
                  bps={live.rates?.egressBps.get(channel.output.id) ?? null}
                  watchUrl={watchUrls.get(channel.output.id) ?? null}
                  onStopped={(ref) => setUndo({ label: `Stopped ${channel.output.name}`, items: [ref], untilMs: Date.now() + UNDO_MS })}
                />
              ))}
            </ul>
            {filtered.length > shown && (
              <div className="border-t border-base-content/10 p-3 text-center">
                <button type="button" className="btn btn-sm" onClick={() => setShown(shown + PAGE * 2)}>
                  Show more · {(filtered.length - shown).toLocaleString()} left
                </button>
              </div>
            )}
          </>
        )}
      </Panel>
      <ConfirmBulk
        action={pending}
        count={pending ? counts[pending] : 0}
        scope={scopeLabel}
        impact={pending ? impactWords(bulkTargets(filtered, pending)) : ""}
        onCancel={() => setPending(null)}
        onConfirm={() => pending && void run(pending)}
      />
    </div>
  );
}


function ChannelCard(props: { channel: Channel; detail: boolean; bps: number | null; watchUrl: string | null; onStopped: (ref: OutputRef) => void }) {
  const canOperate = useSession().can("operator");
  const { channel } = props;
  const { output } = channel;
  const [busy, setBusy] = useState(false);
  const on = output.desired === "running";
  const trouble = outputTrouble(output);
  return (
    <li
      aria-label={`Channel ${output.name}`}
      className={`flex min-w-0 flex-col gap-2 rounded-box border bg-base-100 p-3 ${channel.state === "failed" ? "border-error/40" : channel.state === "retrying" || channel.state === "degraded" ? "border-warning/40" : "border-base-content/10"}`}
    >
      <div className="flex items-center gap-3">
        <PlatformMark platform={channel.platform} size={40} />
        <div className="min-w-0 flex-1">
          <div className="truncate font-semibold" title={output.name}>
            {output.name}
          </div>
          <div className="truncate text-xs text-base-content/70">
            {channel.platform.label} ·{" "}
            <PlaceLink className="hover:underline" place={{ workspace: "operate", pipelineId: output.pipelineId }}>
              {channel.pipelineName}
            </PlaceLink>
          </div>
        </div>
        {/* The label widens the switch's hit area to a touch target. */}
        <label className="touch-36 -my-1 grid shrink-0 place-items-center px-1">
          <button
            type="button"
            role="switch"
            aria-checked={on}
            aria-label={`${on ? "Stop" : "Go live on"} ${output.name}`}
            title={canOperate ? undefined : "Starting and stopping needs the operator role"}
            disabled={busy || !canOperate}
            onClick={async () => {
              setBusy(true);
              try {
                const ok = (await (on ? stopOut : startOut)(output.pipelineId, output.id)) !== null;
                if (ok && on) props.onStopped({ pipelineId: output.pipelineId, id: output.id });
              } finally {
                setBusy(false);
              }
            }}
            className={`toggle toggle-sm shrink-0 ${on ? "toggle-success" : ""}`}
          />
        </label>
      </div>
      <div className="flex flex-wrap items-center gap-2 text-xs">
        <StatusBadge tone={STATE_TONE[channel.state]} dot>
          {STATE_LABEL[channel.state]}
        </StatusBadge>
        {output.retry ? <RetryIn atMs={output.retry.nextAttemptAtMs} attempts={output.retry.attempts} /> : <Since sinceMs={output.stateSinceMs} />}
        <span className="ml-auto tabular text-base-content/70">{channel.state === "live" ? formatBps(props.bps) : ""}</span>
        {props.watchUrl && (
          <a className="link link-hover touch-36 inline-flex items-center" href={props.watchUrl} target="_blank" rel="noreferrer" aria-label={`Watch ${output.name} on ${channel.platform.label}`}>
            Watch ↗
          </a>
        )}
      </div>
      {trouble && channel.state !== "live" && (
        <div className="flex min-w-0 flex-col gap-0.5 text-xs" title={troubleTitle(trouble)}>
          <span className="flex min-w-0 items-baseline gap-2">
            <span className="truncate font-medium text-base-content/80">{trouble.summary}</span>
            <LogsLink pipelineId={output.pipelineId} outputId={output.id} />
          </span>
          {trouble.fix && <span className="line-clamp-2 text-base-content/65">{trouble.fix}</span>}
        </div>
      )}
      {props.detail && (
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 border-t border-base-content/10 pt-2 font-mono text-[0.7rem] text-base-content/70">
          <dt>id</dt>
          <dd className="truncate select-all">{output.id}</dd>
          <dt>attempt</dt>
          <dd className="truncate">
            {output.actual ?? "none"} · {output.convergence} · flow {output.flow}
            {output.shard !== null && ` · shard ${output.shard}`}
          </dd>
          {output.stage && (
            <>
              <dt>stage</dt>
              <dd className="truncate">{output.stage}</dd>
            </>
          )}
          {output.evidence && (
            <>
              <dt>error</dt>
              <dd className="break-words">
                {output.evidence.phase ? `[${output.evidence.phase}] ` : ""}
                {output.evidence.error}
              </dd>
            </>
          )}
        </dl>
      )}
    </li>
  );
}

function RetryIn(props: { atMs: number; attempts: number }) {
  const now = useNow(1000);
  const seconds = Math.max(0, Math.round((props.atMs - now) / 1000));
  return <span className="tabular text-base-content/70">{retryWords(props.attempts, seconds * 1000)}</span>;
}

interface OutputRef {
  readonly pipelineId: string;
  readonly id: string;
}

interface Undo {
  readonly label: string;
  readonly items: readonly OutputRef[];
  readonly untilMs: number;
}

/** How long a stop can be undone from the page. */
const UNDO_MS = 60_000;

/** "Stopped 12 channels · Undo (48 s)": starts exactly those again. */
function UndoBar(props: { undo: Undo; onDone: () => void; onUndone: () => void }) {
  const now = useNow(1000);
  const [busy, setBusy] = useState(false);
  const left = Math.ceil((props.undo.untilMs - now) / 1000);
  useEffect(() => {
    if (left <= 0 && !busy) props.onDone();
  }, [left, busy]);
  if (left <= 0 && !busy) return null;
  return (
    <div role="status" aria-label="Undo" className="flex flex-wrap items-center gap-3 rounded-box border border-base-content/10 bg-base-200 px-4 py-2 text-sm">
      <span className="font-medium">{props.undo.label}</span>
      <button
        type="button"
        className="btn btn-sm btn-outline"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          await runBulk(props.undo.items, async (ref) => (await startOut(ref.pipelineId, ref.id)) !== null, BULK_CONCURRENCY, () => undefined);
          setBusy(false);
          props.onDone();
          props.onUndone();
        }}
      >
        {busy ? "Starting again…" : "Undo"}
      </button>
      <span className="tabular text-xs text-base-content/60">{busy ? "" : `${left} s`}</span>
      <button type="button" className="btn btn-ghost btn-sm ml-auto" aria-label="Dismiss undo" onClick={props.onDone}>
        ✕
      </button>
    </div>
  );
}

function ConfirmBulk(props: { action: BulkAction | null; count: number; scope: string; impact: string; onCancel: () => void; onConfirm: () => void }) {
  const dialog = useRef<HTMLDialogElement | null>(null);
  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    if (props.action && !element.open) element.showModal();
    if (!props.action && element.open) element.close();
  }, [props.action]);
  const verb = props.action === "start" ? "Go live on" : props.action === "stop" ? "Stop" : "Retry";
  return (
    <dialog ref={dialog} className="modal" onClose={props.onCancel} aria-label="Confirm bulk action">
      <div className="modal-box">
        <h3 className="text-lg font-semibold">
          {verb} {props.count.toLocaleString()} channel{props.count === 1 ? "" : "s"}?
        </h3>
        <p className="mt-2 text-sm text-base-content/70">
          This acts on {props.scope}. {props.action === "stop" ? "Viewers on these channels lose the stream." : "Each channel connects to its platform with its stored key."}
        </p>
        {props.impact && <p className="mt-2 text-sm font-medium">{props.impact}</p>}
        {props.action === "stop" && <p className="mt-2 text-xs text-base-content/60">You can undo it for a minute after.</p>}
        <div className="modal-action">
          <button type="button" className="btn btn-ghost" onClick={props.onCancel}>
            Cancel
          </button>
          <button type="button" className={`btn ${props.action === "stop" ? "btn-error" : "btn-accent"}`} onClick={props.onConfirm}>
            {verb} {props.count.toLocaleString()}
          </button>
        </div>
      </div>
    </dialog>
  );
}
