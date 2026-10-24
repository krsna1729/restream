// What every workspace is for, and the steps of its on-screen tour: pure
// data and the rules that pick and order steps. The shell draws the help
// drawer and the spotlight (shell/help.tsx, shell/tour.tsx).
//
// A step's target is a CSS selector; the tour lights the first visible
// match and falls back to a centred card when the page has nothing to point
// at (an empty fleet, a pipeline not yet picked).

import type { Role } from "../core/api-types.js";
import type { Workspace } from "./route.js";
import { atLeast, canOpen } from "./team-model.js";

export interface TourStep {
  /** Where the step lives; the tour moves there first. Page tours omit it. */
  readonly workspace?: Workspace;
  readonly target?: string;
  readonly title: string;
  readonly body: string;
  /** Shown only to sessions acting with at least this role. */
  readonly role?: Role;
}

export interface PageHelp {
  /** One sentence: what the page answers. */
  readonly purpose: string;
  /** Who reaches for it, and when. */
  readonly when: string;
  /** Terms the page uses, defined once. */
  readonly terms: ReadonlyArray<readonly [string, string]>;
  readonly tour: readonly TourStep[];
}

/** A Panel's section, by the id its title carries. */
export const panel = (id: string): string => `section[aria-labelledby="${id}"]`;
const HEADER = '[data-tour="page-header"]';
const PIPELINE_TABS = 'nav[aria-label="Pipeline views"]';

export const PAGE_HELP: Record<Workspace, PageHelp> = {
  fleet: {
    purpose: "Is the whole plant healthy right now, and if not, where should I look first?",
    when: "The home screen. Open it at the start of a shift and whenever a badge turns amber or red.",
    terms: [
      ["Delivering", "A destination that is connected and sending the media it is fed."],
      ["Headroom", "How far the busiest service center is from its safe limit (80% of calibrated capacity)."],
    ],
    tour: [
      { target: HEADER, title: "The verdict first", body: "The headline states the plant's condition in one line; its colour turns amber or red when something needs a person." },
      { target: panel("fleet-attention"), title: "Needs attention", body: "Pipelines with a degraded, retrying or stalled destination, worst first. Click one to operate it." },
      { target: panel("fleet-headroom"), title: "Headroom", body: "Host CPU, memory, NIC and egress shards against their safe limits, so you see a capacity problem before it becomes an outage." },
      { target: panel("fleet-history"), title: "Fleet history", body: "Delivery and headroom over the last hour, sampled by the engine even when no dashboard is open." },
      { target: panel("fleet-table"), title: "Every pipeline", body: "Sort, filter and jump into any pipeline. On a phone this table becomes cards." },
    ],
  },
  channels: {
    purpose: "Which platforms am I live on, and how do I take many destinations live or off at once?",
    when: "Before and after a show: go live everywhere, retry the ones that failed, stop when it ends.",
    terms: [
      ["Channel", "One destination (a YouTube, Twitch or custom RTMP/SRT endpoint) of one pipeline."],
      ["Retry budget", "How many reconnect attempts a destination makes, with growing waits, before it gives up."],
    ],
    tour: [
      { target: HEADER, title: "Live everywhere?", body: "The headline counts wanted channels that are live. The buttons act on every channel the filters below select.", role: "operator" },
      { target: "[data-tour=page-header] button", title: "Bulk actions", body: "Go live, Retry failed and Stop ask for confirmation and report each destination's result.", role: "operator" },
      { target: panel("channels-list"), title: "One row per channel", body: "Trouble sorts first. A retrying channel shows its attempt and a countdown to the next one." },
    ],
  },
  pipelines: {
    purpose: "What pipelines exist, how are they built, and which ones share work?",
    when: "To find a pipeline, compare them, or add a new one.",
    terms: [
      ["Pipeline", "One live source (with optional backups) fanned out to many destinations."],
      ["Shared stage", "Work such as a transcode or a TS mux done once and read by every destination that needs it."],
    ],
    tour: [
      { target: panel("operational-filters"), title: "Filters that matter", body: "Each card is a filter: click one to list only the pipelines in that condition." },
      { target: panel("protocol-mix"), title: "Protocol mix", body: "Every destination by protocol, across the plant." },
      { target: panel("pipelines-list"), title: "The list", body: "Search by name or id; open a pipeline to operate, monitor or inspect it." },
      { target: panel("stage-sharing"), title: "Shared work", body: "How many outputs each shared stage serves: the reason one host carries thousands of destinations." },
    ],
  },
  operate: {
    purpose: "Run one pipeline: its source, its media path, and every destination.",
    when: "During a show, or whenever one pipeline needs hands-on work.",
    terms: [
      ["Primary / backup", "A pipeline's inputs. The one on air feeds every destination; promote another to switch."],
      ["Media path", "The stages media crosses inside the engine, from ingest to each egress."],
    ],
    tour: [
      { target: PIPELINE_TABS, title: "One pipeline, five views", body: "Operate, Stream health, Monitor, Inspect and Logs all show the pipeline picked here." },
      { target: '[aria-label="Pipeline controls"]', title: "Controls", body: "Record, play or stop a file source, edit the pipeline's settings, or delete it.", role: "operator" },
      { target: panel("operate-sources"), title: "Source deck", body: "Every input, which one is on air, and a live preview. Promote a backup to switch sources without stopping destinations." },
      { target: panel("operate-graph"), title: "Media path", body: "Click any stage to inspect it: its consumers, packet rate and the destinations behind it. The tabs switch to the destination list, a timeline and the preview." },
      { target: panel("operate-destinations"), title: "Destination matrix", body: "State, rate and the cause of any trouble, with start, stop and edit per destination." },
    ],
  },
  health: {
    purpose: "Is the publisher sending what platforms expect?",
    when: "When a platform complains about a stream, or before a show to check the encoder.",
    terms: [
      ["Keyframe interval", "Seconds between keyframes. Platforms want 2 s and reject streams past 4 s."],
      ["Contribution link", "The network path from the publisher to restream; a file source has none."],
    ],
    tour: [
      { target: panel("health-verdicts"), title: "Verdicts in plain words", body: "Each check says whether the encoder is fine and what to change if not, judged over the last minute." },
      { target: panel("health-bitrate"), title: "Bitrate", body: "What the publisher sends, every 2 seconds. Swings over 35% usually mean an encoder in variable-bitrate mode." },
      { target: panel("health-keyframes"), title: "Keyframes", body: "Mean and longest interval against the 2 s target and the 4 s limit." },
      { target: panel("health-link"), title: "Contribution link", body: "Round trip and loss for SRT and RTMP publishers." },
    ],
  },
  monitor: {
    purpose: "How do this pipeline's signals move together?",
    when: "While something is going wrong, to see cause and effect on one clock.",
    terms: [
      ["Feed lag", "How far an egress reader is behind the newest packet in its ring."],
      ["Source ring", "The pipeline's in-memory packet buffer that every stage and egress reads."],
    ],
    tour: [
      { target: panel("monitor-signals"), title: "One clock", body: "Ingest, egress, delivery, ring fill, feed lag and shard progress share a time axis. Pause freezes the window; Compare overlays the live one on it." },
    ],
  },
  inspect: {
    purpose: "Which resource is this pipeline really waiting on?",
    when: "When something is slow or stuck and you need to know which resource is the bottleneck.",
    terms: [
      ["Confidence", "Measured (read from the OS or engine), derived (computed from measurements) or estimated (modelled)."],
      ["Egress shard", "A thread that sends to many destinations; the unit of egress capacity."],
    ],
    tour: [
      { target: panel("inspect-map"), title: "Resource map", body: "Every node this pipeline touches, under pressure first. Click one for its detail." },
      { target: panel("inspect-node"), title: "Selected node", body: "Its metrics and whether it is the bottleneck, judged against calibrated limits." },
      { target: panel("inspect-shards"), title: "Egress shards", body: "Queue depth and progress per shard: a stuck shard shows here first." },
    ],
  },
  schedule: {
    purpose: "What will the engine do on its own, and when?",
    when: "To plan a show: go live at a time, stop after, or play a file source.",
    terms: [["Skipped run", "A time the engine was down for is skipped, never run late."]],
    tour: [
      { target: panel("schedule-week"), title: "The week ahead", body: "Every event that will run in the next seven days, in your local time." },
      { target: panel("schedule-add"), title: "Schedule an event", body: "Pick a pipeline, an action and a time; repeat daily or weekly.", role: "operator" },
      { target: panel("schedule-events"), title: "Events", body: "Each event's next run and the result of its last one. Run now tests an event without waiting." },
    ],
  },
  incidents: {
    purpose: "What broke, what did not, and what should I do?",
    when: "When an alert fires, or to review what happened after a show.",
    terms: [
      ["Evidence", "Measured facts the judgement rests on, each labelled with how it was obtained."],
      ["Resolved window", "How far back closed incidents are listed."],
    ],
    tour: [
      { target: panel("incident-queue"), title: "Open first", body: "Critical, widest and oldest incidents first." },
      { target: panel("incident-detail"), title: "Judgement and correlation", body: "The engine's verdict, a chart of the plant around the moment it opened, and the lifecycle events of the affected pipeline." },
      { target: panel("incident-evidence"), title: "Evidence packet", body: "What failed, what was ruled out, and the operator action. Export it for a ticket or a post-mortem." },
    ],
  },
  capacity: {
    purpose: "How much more can this host carry?",
    when: "Before adding destinations or a pipeline, and when headroom shrinks.",
    terms: [
      ["Service center", "A resource that serves work: ingest, an egress shard, the NIC, FFmpeg, memory, disk."],
      ["Calibration prior", "A measured per-host limit for one service center; load is priced against it."],
    ],
    tour: [
      { target: panel("capacity-now"), title: "Load now", body: "Each service center's projected load against its prior." },
      { target: panel("capacity-scenario"), title: "What if", body: "Price N more outputs of a protocol and encoding before you add them." },
      { target: panel("capacity-calibration"), title: "Calibration", body: "The limits the model uses, and where each one came from." },
    ],
  },
  stability: {
    purpose: "Why does a short failure sometimes leave a system stuck long after it ends, and how does restream avoid it?",
    when: "To learn how retry policy shapes recovery, or to replay what a pipeline actually did.",
    terms: [
      ["Metastable failure", "A state where recovery work (reconnects, re-sent copies) keeps the system overloaded after the trigger has gone."],
      ["Backoff", "Waiting longer after each failed attempt, with jitter, so reconnects spread out instead of arriving together."],
    ],
    tour: [
      { target: HEADER, title: "Simulate or replay", body: "Pick a scenario and a retry policy, or replay a real pipeline from its flight recorder." },
      { target: '[aria-label="Destinations now"]', title: "Destinations now", body: "Wanted, not delivering and delivering at the playhead." },
      { target: panel("stability-queues"), title: "Shard queues", body: "Each cell is queued work: blue is new media, red is re-sent copies, grey is handshakes. A storm turns the queues red." },
      { target: panel("stability-chart"), title: "Over time", body: "Delivering against wanted, with reconnects per second. The shaded band is the trigger." },
    ],
  },
  media: {
    purpose: "What files and recordings do I have, and can they feed a live pipeline?",
    when: "To upload a file for a playlist or loop, or to fetch a recording.",
    terms: [["Live Optimized", "A re-encode toward 2 s keyframes, for a file whose keyframes are too far apart to join a live stream as is."]],
    tour: [
      { target: panel("media-library"), title: "Library", body: "Recordings and source files. Select one to see its streams." },
      { target: panel("media-storage"), title: "Storage", body: "How full the media disk is." },
    ],
  },
  changes: {
    purpose: "Change destinations safely: plan, let the engine judge, apply, verify.",
    when: "For planned changes to many destinations, with a reason kept in the audit log.",
    terms: [["Judgement", "The engine's check of a change against live state and capacity before anything mutates."]],
    tour: [
      { target: HEADER, title: "Four steps", body: "Define the change, let the engine validate it against live state, apply it, then verify each destination came up." },
      { target: panel("changes-define"), title: "Define", body: "Say why, pick a pipeline and list the changes. Preview asks the engine to judge them.", role: "operator" },
    ],
  },
  notifications: {
    purpose: "Who hears about alerts, and how?",
    when: "To wire alerts into Slack, Discord, a webhook or this browser.",
    terms: [["Confirmed alert", "An alert must hold for two 5-second checks before anyone is told, so a flicker pages nobody."]],
    tour: [
      { target: panel("notify-browser"), title: "This browser", body: "Push notifications to this device, even with no tab open, or in-tab ones while it is." },
      { target: panel("notify-add"), title: "Add a channel", body: "Slack, Discord or a signed webhook, with a severity filter.", role: "admin" },
      { target: panel("notify-deliveries"), title: "Deliveries", body: "Every send and its outcome; failures retry twice." },
    ],
  },
  team: {
    purpose: "Who can sign in, and what may each person do?",
    when: "To add a colleague, change a role or disable an account.",
    terms: [["View as", "An admin or operator can see the dashboard as a lower role; the engine enforces it."]],
    tour: [
      { target: '[aria-label="Roles"]', title: "Three roles", body: "Viewers watch, operators run the plant, admins own settings and the team." },
      { target: panel("team-add"), title: "Add a member", body: "A username, an initial password and a role." },
      { target: panel("team-members"), title: "Members", body: "Change a role, reset a password or disable an account; disabling ends their sessions." },
    ],
  },
  audit: {
    purpose: "Who changed what, and when?",
    when: "After an unexpected change, or for a compliance record.",
    terms: [["Refused", "A change the engine denied because the role was too low."]],
    tour: [{ target: HEADER, title: "Every change", body: "Each change records who, the role they acted with, and the result. Filter by text or outcome." }],
  },
  settings: {
    purpose: "Engine-wide defaults: names, SRT, transcode profiles, security.",
    when: "At install, or when a default needs to change for every pipeline.",
    terms: [["Profile", "A named transcode recipe destinations can use instead of passing the source through."]],
    tour: [
      { target: panel("settings-general"), title: "Each card saves alone", body: "Saving one card never touches another." },
      { target: panel("settings-profiles"), title: "Transcode profiles", body: "Built-in and custom encodings. A shared stage runs each profile once per pipeline." },
    ],
  },
  logs: {
    purpose: "What exactly did the engine do?",
    when: "To read the raw lifecycle record behind a judgement.",
    terms: [["Lifecycle event", "A state change of an input, stage or output: started, failed, stopped."]],
    tour: [{ target: '[aria-label="Log rows"]', title: "Newest first", body: "Rows stream in live; pause to read. Expand fields for the structured detail." }],
  },
  system: {
    purpose: "Is this host set up for production media?",
    when: "At install, after an upgrade, or when a host check turns amber.",
    terms: [["Execution class", "The thread family that runs a kind of work: control, media, ingest owners, egress shards, FFmpeg."]],
    tour: [
      { target: panel("system-host"), title: "Host readiness", body: "Kernel and process limits the media path depends on, each with what it is needed for." },
      { target: panel("system-topology"), title: "Execution topology", body: "How many threads and processes each execution class runs." },
      { target: panel("system-build"), title: "Build and provenance", body: "Versions of every media library, and the SBOM." },
    ],
  },
  guides: {
    purpose: "How do I get a task done, step by step?",
    when: "When you are new, or doing something for the first time.",
    terms: [],
    tour: [{ target: HEADER, title: "Guides", body: "Each guide links to the page where a step happens and can light up that page with a tour." }],
  },
  architecture: {
    purpose: "How does restream work as a whole?",
    when: "To understand what happens to a stream between the encoder and the platforms.",
    terms: [],
    tour: [{ target: '[aria-label="System diagram"]', title: "Click a block", body: "Each block explains its job and links to the page that shows it live." }],
  },
};

/** The first-run tour: the shell, then one stop per everyday page. */
export const ONBOARDING: readonly TourStep[] = [
  { workspace: "fleet", title: "Welcome to the control room", body: "A two-minute tour of where things are. Leave it any time with Esc; restart it from the help button (?) in the top bar." },
  { workspace: "fleet", target: 'nav[aria-label="Workspaces"]', title: "Workspaces", body: "Operate runs the plant, Understand explains it, Manage configures it, Learn teaches it. More tools opens capacity, stability, inspection, logs and the rest when you need them." },
  { workspace: "fleet", target: HEADER, title: "Fleet: the home screen", body: "One verdict for the whole plant, the pipelines that need attention and the host's headroom." },
  { workspace: "channels", target: panel("channels-list"), title: "Channels", body: "Every destination on every platform, with bulk go-live, retry and stop." },
  { workspace: "operate", target: PIPELINE_TABS, title: "One pipeline in depth", body: "Operate, Stream health, Monitor, Inspect and Logs all look at the pipeline you pick." },
  { workspace: "operate", target: panel("operate-graph"), title: "The media path", body: "Click a stage to inspect it. This is where a pipeline's shared work is visible." },
  { workspace: "incidents", target: HEADER, title: "Incidents", body: "When something breaks, the engine judges it, gathers evidence and says what to do." },
  { workspace: "fleet", target: "header button[aria-label='Search and commands']", title: "Search and commands", body: "Press ⌘K (Ctrl K) to jump to any pipeline, destination or incident, or run an action." },
  { workspace: "fleet", target: 'button[aria-label="Help for this page"]', title: "Help on every page", body: "This button explains the page you are on and can light it up with a short tour. Guides and How it works live under Learn." },
];

/** The steps a role sees, in order: none it may not act on or open. */
export function stepsFor(steps: readonly TourStep[], role: Role): TourStep[] {
  return steps.filter(
    (step) => (!step.role || atLeast(role, step.role)) && (!step.workspace || canOpen(role, step.workspace)),
  );
}

/** Where Next (+1) or Back (-1) goes from `index`; null ends the tour. */
export function moveStep(index: number, delta: 1 | -1, length: number): number | null {
  const next = index + delta;
  if (next >= length) return null;
  return Math.max(0, next);
}
