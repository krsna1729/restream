// Agent-plane API types: planned changes, approval-gated operations and
// incident explanations. Split from api-types.ts.

import type { OutputConfig } from "../types.js";
import type { OperatorAlert, Role } from "./api-types.js";
import type { EgressProtocol, Layer } from "./generated/runtime-state.js";

/** One output change the engine's planner understands (`ProposedChange`). */
export interface ProposedChange {
  kind: "addOutput" | "updateOutput" | "removeOutput" | "startOutput" | "stopOutput";
  outputId?: string;
  name?: string;
  url?: string;
  config?: OutputConfig;
  desiredState?: "running" | "stopped";
}

export interface ChangeRequest {
  intent: string;
  pipelineId: string;
  proposedChanges: ProposedChange[];
  /** On create: the alert this operation answers, kept in its audit log. */
  incidentId?: string;
}

export interface PlanIssue {
  severity: string;
  code: string;
  message: string;
  field?: string;
}

/** Wanted-running outputs that read one stage over one protocol. */
export interface TopologyRow {
  stage: string;
  protocol: EgressProtocol;
  outputs: number;
}

/** The plan pipeline's wanted-running outputs before and after a change,
 * planned for the live ingest's codec (`null`: no live ingest). */
export interface TopologyDiff {
  pipelineId: string;
  ingestCodec: string | null;
  before: TopologyRow[];
  after: TopologyRow[];
  addedStages: string[];
  retiredStages: string[];
}

/** `POST /agent/plans`: the engine's judgement of a change, made without
 * mutating anything. */
export interface ChangePlan {
  planId: string;
  executionEnabled: boolean;
  validation: { valid: boolean; errors: PlanIssue[]; warnings: PlanIssue[] };
  graphPreview: { notes: string[]; topology: TopologyDiff | null };
  impact: {
    affectedPipelines: string[];
    affectedOutputs: string[];
    sharedStageCandidates: string[];
    operatorSummary: string;
    engineeringNotes: string[];
  };
}

export type OperationStatus =
  | "invalid"
  | "awaitingApproval"
  | "approved"
  | "applying"
  | "applied"
  | "verified"
  | "verificationFailed"
  | "failed";

/** A fixed verification gate; all four must pass for `verified`. */
export interface VerifyGate {
  gate: "desiredState" | "outputsRunning" | "newCriticalAlerts" | "capacityState";
  passed: boolean;
  detail: string;
}

/** One change the apply made, as the engine recorded it. */
export interface StateTransition {
  at: string;
  kind: ProposedChange["kind"];
  pipelineId: string;
  outputId?: string | null;
  /** The output record before and after; `null` when there was none. */
  from?: { desiredState?: string } | null;
  to?: { desiredState?: string } | null;
}

export interface OperationCheck {
  kind: string;
  outputId?: string | null;
  passed: boolean;
  reason?: string | null;
}

/** One change of an operation and its approval. Each is approved on its own
 * (`POST …/actions/{index}/approve`) by a session whose role reaches
 * `requiredRole`; the operation is approved with its last change. */
export interface OperationAction {
  index: number;
  kind: ProposedChange["kind"];
  /** Removing a destination needs an admin; every other change an operator. */
  requiredRole: Role;
  approval: { approvedBy: string; role: Role; reason: string | null; approvedAt: string } | null;
}

/** An approval-gated operation (`public_record`): redacted, in memory only. */
export interface ChangeOperation {
  operationId: string;
  status: OperationStatus;
  intentSummary: string;
  /** The request it was created from; secrets such as URLs are redacted. */
  request: ChangeRequest;
  /** `approvedBy` is the member whose approval completed it. */
  approval: { approvedBy: string; claimedBy: string; reason: string | null; approvedAt: string } | null;
  actions: OperationAction[];
  auditLog: { at: string; kind: string; summary: string }[];
  stateTransitions: StateTransition[];
  /** A failed apply stops at the first rejected change; earlier ones stay applied. */
  executionResult:
    | {
        success: true;
        changeResults: { kind: string; outputId?: string | null; status: string }[];
      }
    | { success: false; error: string }
    | null;
  /** `gates` decide `verified`; the alert delta and graph are reported. */
  verificationResult: {
    explanation?: string;
    gates?: VerifyGate[];
    checks?: OperationCheck[];
    incidentDelta?: { delta: number };
    graphConvergence?: { activeNodes?: number };
  } | null;
}

/** `GET /agent/incidents/explanation`: why an incident happened, read from
 * the pipeline's flight record over it, and the fix it proposes. */
export interface IncidentExplanation {
  alertId: string;
  pipelineId: string | null;
  outputId: string | null;
  window: {
    fromMs: number;
    toMs: number;
    incidentFromMs: number;
    samples: number;
    incidentSamples: number;
  };
  /** The broken layer; null when neither the window nor the alert names one. */
  layer: Layer | null;
  summary: string;
  evidence: string[];
  /** Upstream layers the window shows healthy, and why. */
  ruledOut: { layer: Layer; reason: string }[];
  /** A plant change that fixes the cause; null when none does. */
  proposal: ChangeRequest | null;
  recommendation: string;
}

export interface IncidentExplanationResponse {
  alert: OperatorAlert;
  explanation: IncidentExplanation;
  /** The proposal judged by the planner; null without a proposal. */
  plan: ChangePlan | null;
}
