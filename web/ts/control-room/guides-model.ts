// Task guides: the everyday jobs, step by step, each step naming the page it
// happens on. Pure data plus the role rule; guides.tsx draws them and can
// light a step's page up with that page's tour.

import type { Role } from "../core/api-types.js";
import type { Workspace } from "./route.js";
import { atLeast } from "./team-model.js";

export interface GuideStep {
  readonly text: string;
  /** The page the step happens on. */
  readonly workspace?: Workspace;
}

export interface Guide {
  readonly id: string;
  readonly title: string;
  readonly summary: string;
  /** The least role that can follow every step. */
  readonly role: Role;
  readonly topic: "Go live" | "Keep it running" | "Plan" | "Set up";
  readonly steps: readonly GuideStep[];
}

export const GUIDES: readonly Guide[] = [
  {
    id: "go-live",
    title: "Go live on every platform",
    summary: "Start every wanted destination at once and confirm each one delivers.",
    role: "operator",
    topic: "Go live",
    steps: [
      { text: "Check the source is on air: the pipeline's source deck shows it with a green badge.", workspace: "operate" },
      { text: "Open Channels and narrow the scope with the platform cards or the pipeline filter if needed.", workspace: "channels" },
      { text: "Press Go live and confirm. The progress bar counts each destination the engine accepts.", workspace: "channels" },
      { text: "Watch the rows turn live. A row that retries shows its attempt and a countdown; Retry failed restarts the ones that gave up.", workspace: "channels" },
    ],
  },
  {
    id: "add-destination",
    title: "Add a destination",
    summary: "Send a pipeline to one more platform or server.",
    role: "operator",
    topic: "Go live",
    steps: [
      { text: "Open the pipeline in Operate and press Add destination above the destination matrix.", workspace: "operate" },
      { text: "Pick a platform preset (YouTube, Twitch, Facebook…) or a custom RTMP, SRT or HLS URL, and paste the stream key." },
      { text: "Choose Source to pass the media through untouched, or a transcode profile. Every destination on the same profile shares one encode." },
      { text: "Save, then start it from its row. For many destinations at once with a reason in the audit log, use Changes.", workspace: "changes" },
    ],
  },
  {
    id: "recover-destination",
    title: "Recover a failing destination",
    summary: "Find out why a destination is not delivering and bring it back.",
    role: "operator",
    topic: "Keep it running",
    steps: [
      { text: "In Operate, the destination matrix sorts trouble first; the Cause column says what the engine saw.", workspace: "operate" },
      { text: "If the cause is on the platform side (refused, unreachable), check the stream key and the platform's dashboard, then press Stop and Start." },
      { text: "If every destination of a pipeline suffers, look at the publisher: Stream health judges keyframes, bitrate and frame rate.", workspace: "health" },
      { text: "Open the incident for the engine's evidence and the recommended action.", workspace: "incidents" },
    ],
  },
  {
    id: "switch-source",
    title: "Switch to a backup source",
    summary: "Put another input on air without stopping destinations.",
    role: "operator",
    topic: "Keep it running",
    steps: [
      { text: "In Operate's source deck, every input shows whether it is connected and a preview.", workspace: "operate" },
      { text: "Press Promote on the input you want. Destinations keep their connections; the engine switches what feeds them." },
      { text: "Add a backup input with Add input; publish to it with the URLs under Publish URLs." },
    ],
  },
  {
    id: "playlist",
    title: "Play pre-recorded files as a live show",
    summary: "Loop one file or play a playlist back to back as the pipeline's source.",
    role: "operator",
    topic: "Go live",
    steps: [
      { text: "Upload the files in Media and select each one: its readiness says whether it plays as is or needs Live Optimized, a re-encode toward 2 s keyframes.", workspace: "media" },
      { text: "In Operate, open Settings and add the files to the pipeline's playlist; choose whether it loops.", workspace: "operate" },
      { text: "Press Play file (Play playlist for several). The pipeline bar shows what is playing now; destinations see one continuous stream." },
    ],
  },
  {
    id: "schedule",
    title: "Schedule a show",
    summary: "Have the engine go live, stop or play a file at a set time, even with no dashboard open.",
    role: "operator",
    topic: "Plan",
    steps: [
      { text: "Open Schedule and fill in Schedule an event: a name, the pipeline, the action and the time in your local zone.", workspace: "schedule" },
      { text: "Repeat it daily or weekly if the show recurs." },
      { text: "Check The week ahead. After each run the event shows its result; a time the engine was down for is skipped, never run late." },
    ],
  },
  {
    id: "incident",
    title: "Investigate an incident",
    summary: "From an alert to a cause, with evidence to share.",
    role: "viewer",
    topic: "Keep it running",
    steps: [
      { text: "Open Incidents; critical, widest and oldest come first.", workspace: "incidents" },
      { text: "Read the judgement and the correlation chart: what the plant did around the moment the incident opened." },
      { text: "Use Monitor to watch the pipeline's signals on one clock, and Logs for the raw lifecycle record.", workspace: "monitor" },
      { text: "Export evidence for a ticket or a post-mortem." },
    ],
  },
  {
    id: "capacity",
    title: "Check the host can take more",
    summary: "Price new destinations before adding them.",
    role: "viewer",
    topic: "Plan",
    steps: [
      { text: "Open Capacity: each service center's load against its calibrated limit.", workspace: "capacity" },
      { text: "In Scenario, enter how many outputs to add, their protocol and encoding, and press Project." },
      { text: "Keep the hottest center below 80%: past it, latency grows faster than load." },
    ],
  },
  {
    id: "storms",
    title: "Understand retry storms",
    summary: "See why backoff and jitter let restream recover from a platform outage.",
    role: "viewer",
    topic: "Plan",
    steps: [
      { text: "Open Stability Lab and pick A platform blips for 20 s.", workspace: "stability" },
      { text: "Play it under Engine backoff, then under Retry at once: the second keeps the shards busy with handshakes long after the outage." },
      { text: "Replay a pipeline to see the same view built from what the engine recorded; drag across its history, or open an incident and press Replay, to pick the span." },
    ],
  },
  {
    id: "share",
    title: "Share a watch link",
    summary: "Let someone without an account watch one pipeline, or embed it on a site.",
    role: "admin",
    topic: "Set up",
    steps: [
      { text: "In Operate, find Share under the destination matrix.", workspace: "operate" },
      { text: "Give the link a label and a lifetime, and press Create link." },
      { text: "Copy the link or the embed code. Revoke it there; it stops working at once." },
    ],
  },
  {
    id: "alerts",
    title: "Get alerts in Slack, Discord or this browser",
    summary: "Be told when something breaks, without watching the dashboard.",
    role: "viewer",
    topic: "Set up",
    steps: [
      { text: "Open Notifications. Anyone can turn on push for their own browser.", workspace: "notifications" },
      { text: "Admins add Slack, Discord or webhook channels with a severity filter, and test each one." },
      { text: "An alert must hold for two checks (10 s) before anyone is told, so a flicker pages nobody." },
    ],
  },
  {
    id: "team",
    title: "Add a team member",
    summary: "Give a colleague their own sign-in and the right role.",
    role: "admin",
    topic: "Set up",
    steps: [
      { text: "Open Team and add a member with a username, an initial password and a role.", workspace: "team" },
      { text: "Viewers watch, operators run the plant, admins also own settings and the team." },
      { text: "To check what a role sees, use View as in your account menu; the engine enforces the narrower role." },
      { text: "Every change anyone makes lands in the Audit log.", workspace: "audit" },
    ],
  },
];

export const GUIDE_TOPICS: readonly Guide["topic"][] = ["Go live", "Keep it running", "Plan", "Set up"];

/** Whether a role can follow every step of a guide. */
export function canFollow(role: Role, guide: Guide): boolean {
  return atLeast(role, guide.role);
}
