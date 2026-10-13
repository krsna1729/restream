// The Change Studio's view of the engine's planner and operations: what the
// operator asks for, how far the change has come, and what proved it. The
// engine validates every change; nothing here repeats its rules.

import type {
  ChangeOperation,
  ChangePlan,
  ChangeRequest,
  OperationStatus,
  ProposedChange,
} from "../core/api-types.js";

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
 * finished (or cannot go on). */
export function nextStep(status: OperationStatus): "approve" | "apply" | "verify" | null {
  switch (status) {
    case "awaitingApproval":
      return "approve";
    case "approved":
      return "apply";
    case "applied":
      return "verify";
    default:
      return null;
  }
}

export interface Gate {
  readonly label: string;
  readonly passed: boolean;
  readonly detail: string;
}

/** What the engine proved after applying. Only the per-change checks decide
 * "verified"; the alert delta and active graph nodes are reported, not
 * judged, so they are observations, never gates. */
export function verification(operation: ChangeOperation): {
  readonly gates: readonly Gate[];
  readonly observations: readonly string[];
} {
  const result = operation.verificationResult;
  if (!result) return { gates: [], observations: [] };
  const gates = (result.checks ?? []).map((check) => ({
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
  return { gates, observations };
}
