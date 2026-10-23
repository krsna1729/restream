// How it works: the system as one picture. The media path left to right,
// the control plane under it, and the background tasks that watch both.
// Every block opens a short explanation and a link to the page that shows
// that part live. Facts here follow docs/architecture.md.

import { useState } from "react";

import { PageHeader } from "../ui/page-header.js";
import { Panel } from "../ui/panel.js";
import { WORKSPACE_LABELS } from "./route.js";
import type { Workspace } from "./route.js";
import { PlaceLink, usePlace } from "./router.js";

interface Block {
  readonly id: string;
  readonly title: string;
  readonly line: string;
  readonly detail: readonly string[];
  readonly see: readonly Workspace[];
}

const MEDIA_PATH: readonly Block[] = [
  {
    id: "publishers",
    title: "Publishers",
    line: "RTMP, SRT, HLS, MoQ, files",
    detail: [
      "An encoder pushes RTMP (port 1935) or SRT (port 10080), or restream pulls an HLS playlist, or plays a file from the media library through an FFmpeg child.",
      "Each pipeline has up to four inputs: a primary and backups, each with its own stream key. Backups stay warm so a switch is instant.",
    ],
    see: ["health", "operate"],
  },
  {
    id: "ingress",
    title: "Ingress owners",
    line: "listeners on owner threads",
    detail: [
      "Each protocol's listener runs on dedicated owner threads (Compio). They check the stream key, demux the container and hand over media packets.",
      "Ingest media never takes a per-packet hop through the async control runtime, so a busy API cannot slow a stream down.",
    ],
    see: ["system"],
  },
  {
    id: "gate",
    title: "Input gate",
    line: "which input is on air",
    detail: [
      "The gate lets one input of a pipeline through. Promoting a backup switches what feeds the pipeline without restarting a single destination.",
    ],
    see: ["operate"],
  },
  {
    id: "ring",
    title: "Source ring",
    line: "one per pipeline, 1024 slots",
    detail: [
      "A lock-free ring of media packets. Every stage and destination reads it at its own pace; a packet is stored once and shared, never copied per reader.",
      "A reader that falls a whole ring behind jumps forward to the newest keyframe instead of holding the writer back: one slow destination cannot stall the others.",
    ],
    see: ["monitor", "inspect"],
  },
  {
    id: "stages",
    title: "Shared stages",
    line: "transcode, audio, TS mux, HLS",
    detail: [
      "Work a destination needs, such as a 720p transcode, an audio track pick or MPEG-TS muxing, runs once per pipeline and writes its own ring. Every destination that needs the same result reads it.",
      "Transcodes run in FFmpeg child processes, outside the engine: if one fails, the engine keeps running and the failure shows on the destinations that read that stage.",
    ],
    see: ["pipelines", "inspect"],
  },
  {
    id: "fabric",
    title: "Egress fabric",
    line: "shard threads send to many",
    detail: [
      "A small number of shard threads each send to many destinations: RTMP and RTMPS, SRT, HLS PUT, MoQ, or another pipeline's input.",
      "A shard is the unit of egress capacity. Capacity prices new destinations against each shard's calibrated limit.",
    ],
    see: ["inspect", "capacity"],
  },
  {
    id: "destinations",
    title: "Destinations",
    line: "platforms and servers",
    detail: [
      "A destination that drops is retried with exponential backoff, from 5 s up to 5 min with jitter, for 10 attempts by default. Spreading reconnects out keeps a platform outage from becoming a retry storm.",
    ],
    see: ["channels", "stability"],
  },
];

const CONTROL: readonly Block[] = [
  {
    id: "dashboard",
    title: "Dashboard",
    line: "this app",
    detail: [
      "Reads the engine's state as a live stream of changes (server-sent events) and sends changes over the HTTP API. It never touches media.",
    ],
    see: ["fleet"],
  },
  {
    id: "api",
    title: "API and roles",
    line: "every request checked",
    detail: [
      "Each request is checked against the least role it needs (viewer, operator or admin) and every change is written to the audit log with who made it and how it ended.",
    ],
    see: ["team", "audit"],
  },
  {
    id: "db",
    title: "SQLite",
    line: "the desired state",
    detail: [
      "Pipelines, inputs, destinations, settings, members, schedules and notification channels: what should be running, not what is.",
    ],
    see: ["settings"],
  },
  {
    id: "reconciler",
    title: "Reconciler",
    line: "desired versus running",
    detail: [
      "Compares the stored state with what the media engine is running, every second and after every change, and starts, stops or retries work until they match.",
    ],
    see: ["changes"],
  },
  {
    id: "engine",
    title: "Media engine",
    line: "owns the running media",
    detail: [
      "Runs the media path above and reports every lifecycle change (an input connected, a destination failed) to the state hub that feeds the dashboard and the log.",
    ],
    see: ["logs"],
  },
];

const WATCHERS: readonly Block[] = [
  {
    id: "flight",
    title: "Flight recorder",
    line: "every 2 s; a day kept",
    detail: [
      "Keeps each pipeline's counters so Stream health, the Stability Lab replay and incident evidence can look back: the last 10 minutes every 2 s in memory, and a sample every 10 s on disk for a day (RESTREAM_FLIGHT_HISTORY_HOURS).",
    ],
    see: ["health", "stability"],
  },
  {
    id: "fleet-history",
    title: "Fleet history",
    line: "every 5 s, 24 h",
    detail: ["Samples delivery and headroom for the whole plant, even with no dashboard open."],
    see: ["fleet", "incidents"],
  },
  {
    id: "alerts",
    title: "Alert notifier",
    line: "checks every 5 s",
    detail: ["Derives alerts from live state. An alert must hold for two checks before Slack, Discord, webhooks or browser push hear about it."],
    see: ["incidents", "notifications"],
  },
  {
    id: "scheduler",
    title: "Scheduler",
    line: "checks every 5 s",
    detail: ["Runs scheduled go-lives, stops and file plays. A time the engine was down for is skipped, never run late."],
    see: ["schedule"],
  },
];

const ALL = [...MEDIA_PATH, ...CONTROL, ...WATCHERS];

export function Architecture() {
  const [selected, setSelected] = useState<string>("ring");
  const block = ALL.find((candidate) => candidate.id === selected) ?? MEDIA_PATH[0];
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="Learn · How it works"
        title="One process, two planes"
        subtitle="Restream is a single program. The media path moves packets from publishers to destinations; the control plane decides what runs. Click a block."
      />
      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_22rem]">
        <div className="flex min-w-0 flex-col gap-4" aria-label="System diagram" role="group">
          <Lane title="Media path" subtitle="left to right, per packet" blocks={MEDIA_PATH} selected={selected} onSelect={setSelected} flow />
          <Lane title="Control plane" subtitle="decides what runs; never carries media" blocks={CONTROL} selected={selected} onSelect={setSelected} flow />
          <Lane title="Always watching" subtitle="background tasks, with or without a dashboard open" blocks={WATCHERS} selected={selected} onSelect={setSelected} />
        </div>
        <BlockDetail block={block} />
      </div>
      <div className="grid gap-4 lg:grid-cols-2">
        <Panel id="arch-change" title="Life of a change" subtitle="you press Start on a destination">
          <Steps
            items={[
              "The dashboard sends the request; the API checks your role and records it in the audit log.",
              "The destination's wanted state is saved in SQLite.",
              "The reconciler sees wanted and running differ and asks the media engine to start the destination.",
              "An egress shard opens the connection and reads the pipeline's ring (or a shared stage's).",
              "The engine reports the change; the dashboard's live stream shows it within a second.",
            ]}
          />
        </Panel>
        <Panel id="arch-trouble" title="When something breaks" subtitle="isolation, then recovery">
          <Steps
            items={[
              "A failure stays where it starts: a slow destination resyncs alone, and an FFmpeg failure stays in its child process.",
              "Destinations retry with growing, jittered waits so a platform outage does not become a storm.",
              "The alert notifier confirms the problem over two checks and tells your channels.",
              "Incidents gather the evidence: what failed, what did not, and what to do.",
            ]}
          />
        </Panel>
      </div>
    </div>
  );
}

function Lane(props: { title: string; subtitle: string; blocks: readonly Block[]; selected: string; onSelect: (id: string) => void; flow?: boolean }) {
  // A flow lane numbers its blocks: the order is the path media or a change takes.
  return (
    <Panel title={props.title} subtitle={props.subtitle}>
      <ol className="grid list-none grid-cols-[repeat(auto-fill,minmax(10.5rem,1fr))] gap-2">
        {props.blocks.map((block, index) => (
          <li key={block.id} className="min-w-0">
            <button
              type="button"
              aria-pressed={props.selected === block.id}
              onClick={() => props.onSelect(block.id)}
              className={`flex h-full w-full items-start gap-2 rounded-field border px-3 py-2 text-left transition-colors ${props.selected === block.id ? "border-primary bg-primary/10" : "border-base-content/15 bg-base-100 hover:border-base-content/40"}`}
            >
              {props.flow && (
                <span aria-hidden="true" className="tabular mt-0.5 grid size-5 shrink-0 place-items-center rounded-full bg-base-content/10 text-[0.7rem] font-semibold">
                  {index + 1}
                </span>
              )}
              <span className="flex min-w-0 flex-col">
                <span className="text-sm font-semibold">{block.title}</span>
                <span className="text-xs text-base-content/70">{block.line}</span>
              </span>
            </button>
          </li>
        ))}
      </ol>
    </Panel>
  );
}

function BlockDetail(props: { block: Block }) {
  const place = usePlace();
  return (
    <Panel id="arch-detail" title={props.block.title} subtitle={props.block.line} className="xl:sticky xl:top-20 xl:self-start">
      <div className="flex flex-col gap-3 text-sm" aria-live="polite">
        {props.block.detail.map((paragraph) => (
          <p key={paragraph}>{paragraph}</p>
        ))}
        <div className="flex flex-wrap gap-2 border-t border-base-content/10 pt-3">
          <span className="kicker w-full">See it live</span>
          {props.block.see.map((workspace) => (
            <PlaceLink key={workspace} place={{ workspace, pipelineId: place.pipelineId }} className="btn btn-outline btn-xs">
              {WORKSPACE_LABELS[workspace]}
            </PlaceLink>
          ))}
        </div>
      </div>
    </Panel>
  );
}

function Steps(props: { items: readonly string[] }) {
  return (
    <ol className="flex list-none flex-col gap-2 text-sm">
      {props.items.map((item, index) => (
        <li key={item} className="grid grid-cols-[1.75rem_minmax(0,1fr)] gap-2">
          <span aria-hidden="true" className="tabular grid size-6 place-items-center rounded-full bg-primary/15 text-xs font-semibold text-primary">
            {index + 1}
          </span>
          <span>{item}</span>
        </li>
      ))}
    </ol>
  );
}
