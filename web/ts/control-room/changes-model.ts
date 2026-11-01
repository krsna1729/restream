// The Change Studio's view of the engine's planner and operations: what the
// operator asks for, how far the change has come, and what proved it. The
// engine validates every change; nothing here repeats its rules.

import type {
  ChangeOperation,
  ChangePlan,
  ChangeRequest,
  OperationAction,
  OperationStatus,
  ProposedChange,
  StateTransition,
  TopologyDiff,
  VerifyGate,
} from "../core/agent-types.js";
import type { Role } from "../core/api-types.js";
import type { ChangeIntent } from "./route.js";
import type { Convergence, EgressProtocol, OutputState } from "../core/generated/runtime-state.js";
import { atLeast } from "./team-model.js";

export const CHANGE_KINDS: ReadonlyArray<[ProposedChange["kind"], string]> = [
  ["addOutput", "Add a destination"],
  ["updateOutput", "Change a destination"],
  ["startOutput", "Start a destination"],
  ["stopOutput", "Stop a destination"],
  ["removeOutput", "Remove a destination"],
];

/** One row of the change form. Empty strings mean "not given". */
export interface ChangeDraft {
  readonly kind: ProposedChange["kind"];
  readonly outputId: string;
  readonly name: string;
  readonly url: string;
  /** "" keeps the source; otherwise a transcode profile name. */
  readonly profile: string;
  readonly desiredState: "running" | "stopped";
}

export const NEW_DRAFT: ChangeDraft = {
  kind: "addOutput",
  outputId: "",
  name: "",
  url: "",
  profile: "",
  desiredState: "stopped",
};

/** The draft a palette command opens Changes with: the same draft the form
 * makes when that change and destination are picked. */
export function draftFor(change: ChangeIntent): ChangeDraft {
  return { ...NEW_DRAFT, kind: change.kind, outputId: change.outputId };
}

function proposedChange(draft: ChangeDraft): ProposedChange {
  const name = draft.name.trim();
  const url = draft.url.trim();
  const video = draft.profile
    ? ({ mode: "preset", preset: draft.profile } as const)
    : ({ mode: "source" } as const);
  switch (draft.kind) {
    case "addOutput":
      return {
        kind: "addOutput",
        name,
        url,
        config: { video, audio: { mode: "all" } },
        desiredState: draft.desiredState,
      };
    case "updateOutput":
      // Only what the operator filled in changes; the engine keeps the rest.
      return {
        kind: "updateOutput",
        outputId: draft.outputId,
        ...(name ? { name } : {}),
        ...(url ? { url } : {}),
      };
    default:
      return { kind: draft.kind, outputId: draft.outputId };
  }
}

/** The body for both a plan and an operation, so an operation is created from
 * exactly the change that was previewed. */
export function changeRequest(
  intent: string,
  pipelineId: string,
  drafts: readonly ChangeDraft[],
): ChangeRequest {
  return { intent: intent.trim(), pipelineId, proposedChanges: drafts.map(proposedChange) };
}

export const STAGES = ["Define", "Validate", "Apply", "Verify"] as const;

export type StageState = "done" | "current" | "failed" | "todo";

/** How far a change has come, one state per stage. */
export function progress(plan: ChangePlan | null, operation: ChangeOperation | null): StageState[] {
  const marks = (current: number, failed = false): StageState[] =>
    STAGES.map((_, index) =>
      index < current ? "done" : index === current ? (failed ? "failed" : "current") : "todo",
    );
  if (operation !== null) {
    switch (operation.status) {
      case "invalid":
        return marks(1, true);
      case "awaitingApproval":
      case "approved":
      case "applying":
        return marks(2);
      case "failed":
        return marks(2, true);
      case "applied":
        return marks(3);
      case "verificationFailed":
        return marks(3, true);
      case "verified":
        return marks(STAGES.length);
    }
  }
  if (plan === null) return marks(0);
  return plan.validation.valid ? marks(1) : marks(1, true);
}

/** The one step an operator can take next, or null when the operation is
 * finished (or cannot go on). Verification is a point-in-time check, so a
 * failed one can run again: a started output needs seconds to connect. */
export function nextStep(status: OperationStatus): "approve" | "apply" | "verify" | null {
  switch (status) {
    case "awaitingApproval":
      return "approve";
    case "approved":
      return "apply";
    case "applied":
    case "verificationFailed":
      return "verify";
    default:
      return null;
  }
}

/** What one change shows while its operation awaits approval: who approved
 * it, or whether this session's role may. The server decides; this only
 * keeps the dashboard from offering what it would refuse. */
export function actionApproval(
  action: OperationAction,
  role: Role,
): { label: string; canApprove: boolean; refusal: string | null } {
  if (action.approval) {
    return {
      label: `approved by ${action.approval.approvedBy} (${action.approval.role})`,
      canApprove: false,
      refusal: null,
    };
  }
  const allowed = atLeast(role, action.requiredRole);
  return {
    label: `needs ${action.requiredRole}`,
    canApprove: allowed,
    refusal: allowed
      ? null
      : `Needs the ${action.requiredRole} role; this session acts as ${role}.`,
  };
}

export interface Gate {
  readonly label: string;
  readonly passed: boolean;
  readonly detail: string;
}

const GATE_LABEL: Record<VerifyGate["gate"], string> = {
  desiredState: "Desired state converged",
  outputsRunning: "Outputs it started are running",
  newCriticalAlerts: "No new critical alerts",
  capacityState: "Host not pushed past safe utilization",
};

/** What the engine proved after applying: the fixed gates, which alone
 * decide "verified", and the per-change checks the first gate counts. The
 * alert delta and active graph nodes are reported, not judged. */
export function verification(operation: ChangeOperation): {
  readonly gates: readonly Gate[];
  readonly checks: readonly Gate[];
  readonly observations: readonly string[];
} {
  const result = operation.verificationResult;
  if (!result) return { gates: [], checks: [], observations: [] };
  const gates = (result.gates ?? []).map((gate) => ({
    label: GATE_LABEL[gate.gate] ?? gate.gate,
    passed: gate.passed,
    detail: gate.detail,
  }));
  const checks = (result.checks ?? []).map((check) => ({
    label: `${check.kind}${check.outputId ? ` ${check.outputId}` : ""}`,
    passed: check.passed,
    detail: check.reason ?? "",
  }));
  const observations: string[] = [];
  if (result.incidentDelta) {
    const delta = result.incidentDelta.delta;
    observations.push(
      delta === 0
        ? "Open alerts: as many as before the change."
        : `Open alerts: ${Math.abs(delta)} ${delta > 0 ? "more" : "fewer"} than before the change.`,
    );
  }
  if (result.graphConvergence?.activeNodes !== undefined) {
    observations.push(`Active graph nodes after the change: ${result.graphConvergence.activeNodes}.`);
  }
  return { gates, checks, observations };
}

/** One stage and protocol, before and after the change. */
export interface TopologyLine {
  readonly stage: string;
  readonly protocol: EgressProtocol;
  readonly before: number;
  readonly after: number;
  /** The change starts this stage; otherwise its outputs reuse one. */
  readonly newStage: boolean;
}

/** The before and after rows side by side, largest after first. */
export function topologyLines(diff: TopologyDiff): TopologyLine[] {
  const lines = new Map<string, { stage: string; protocol: EgressProtocol; before: number; after: number }>();
  const line = (stage: string, protocol: EgressProtocol) => {
    const key = `${stage}\u0000${protocol}`;
    let found = lines.get(key);
    if (!found) {
      found = { stage, protocol, before: 0, after: 0 };
      lines.set(key, found);
    }
    return found;
  };
  for (const row of diff.before) line(row.stage, row.protocol).before += row.outputs;
  for (const row of diff.after) line(row.stage, row.protocol).after += row.outputs;
  const added = new Set(diff.addedStages);
  return [...lines.values()]
    .map((entry) => ({ ...entry, newStage: added.has(entry.stage) }))
    .sort(
      (a, b) =>
        b.after - a.after ||
        b.before - a.before ||
        a.stage.localeCompare(b.stage) ||
        a.protocol.localeCompare(b.protocol),
    );
}

/** What the change adds for the capacity scenario, which prices additions
 * only: removals are reported, not credited. Counted per stage and protocol,
 * so an add and a removal elsewhere do not cancel out. The scenario prices
 * SRT by its shard sizing prior and every other output as RTMP. */
export function scenarioLoad(diff: TopologyDiff): {
  readonly addOutputs: number;
  readonly addByProtocol: { readonly rtmp: number; readonly srt: number };
  readonly addStages: number;
  readonly removedOutputs: number;
} {
  let rtmp = 0;
  let srt = 0;
  let removedOutputs = 0;
  for (const line of topologyLines(diff)) {
    const added = Math.max(0, line.after - line.before);
    if (line.protocol === "srt") srt += added;
    else rtmp += added;
    removedOutputs += Math.max(0, line.before - line.after);
  }
  return {
    addOutputs: rtmp + srt,
    addByProtocol: { rtmp, srt },
    addStages: diff.addedStages.length,
    removedOutputs,
  };
}

export interface TimelineStep {
  readonly atMs: number;
  /** Since the operation was created. */
  readonly offsetMs: number;
  readonly label: string;
}

function transitionLabel(transition: StateTransition): string {
  const output = transition.outputId ?? "output";
  if (!transition.from) return `${output} created, desired ${transition.to?.desiredState ?? "unknown"}`;
  if (!transition.to) return `${output} removed`;
  const before = transition.from.desiredState;
  const after = transition.to.desiredState;
  return before === after ? `${output} updated` : `${output} desired ${before} → ${after}`;
}

/** The operation's audit events and each output change its apply made, in
 * time order; an event without a readable time is left out. */
export function convergenceTimeline(operation: ChangeOperation): TimelineStep[] {
  const steps: Array<{ atMs: number; label: string }> = [
    ...operation.auditLog.map((event) => ({
      atMs: Date.parse(event.at),
      label: `${event.kind}: ${event.summary}`,
    })),
    ...(operation.stateTransitions ?? []).map((transition) => ({
      atMs: Date.parse(transition.at),
      label: transitionLabel(transition),
    })),
  ].filter((step) => Number.isFinite(step.atMs));
  steps.sort((a, b) => a.atMs - b.atMs);
  const first = steps[0]?.atMs ?? 0;
  return steps.map((step) => ({ ...step, offsetMs: step.atMs - first }));
}

/** Where the outputs the apply touched stand now, from the live state: a
 * removed output is not in it, so it is not counted. */
export function liveConvergence(
  operation: ChangeOperation,
  outputs: readonly OutputState[],
): { readonly converged: number; readonly total: number; readonly states: ReadonlyArray<[string, Convergence]> } {
  const touched = new Set((operation.stateTransitions ?? []).map((transition) => transition.outputId));
  const states: Array<[string, Convergence]> = outputs
    .filter((output) => touched.has(output.id))
    .map((output) => [output.name, output.convergence]);
  return {
    converged: states.filter(([, state]) => state === "converged").length,
    total: states.length,
    states,
  };
}
