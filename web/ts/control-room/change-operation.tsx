// One agent-plane operation, from approval to verification: each change is
// approved on its own by a role that reaches it, then the operation is
// applied and verified. Change Studio and an explained incident both drive
// their operations through this card.

import { useState } from "react";

import { advanceOperation, approveAction, getOperation } from "../core/api.js";
import type { ChangeOperation, ChangePlan } from "../core/agent-types.js";
import { actionApproval, nextStep, verification } from "./changes-model.js";
import type { Gate } from "./changes-model.js";
import { Convergence } from "./changes-impact.js";
import { useSession } from "./session.js";
import type { LiveSnapshot } from "./store.js";
import { Field } from "../ui/controls.js";
import { Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";

export type Run = (action: () => Promise<void>) => Promise<void>;

export function Operation(props: {
  live: LiveSnapshot;
  operation: ChangeOperation;
  busy: boolean;
  run: Run;
  onChange: (operation: ChangeOperation) => void;
  onDone: () => void;
  doneLabel?: string;
}) {
  const { operation, busy, run } = props;
  const session = useSession();
  const [reason, setReason] = useState("");
  const step = nextStep(operation.status);
  // A refused step (a failed apply answers 400 with the failed record, a
  // refused approval 403) may still have changed the record: read it back.
  // Both null (alerted): a network failure and a forgotten operation look
  // alike, so the card stays and the operator chooses to start over.
  const advance = (request: () => Promise<ChangeOperation | null>) =>
    run(async () => {
      const next = (await request()) ?? (await getOperation(operation.operationId));
      if (next) props.onChange(next);
    });
  return (
    <Panel
      id="changes-operation"
      title="Operation"
      subtitle={<span className="font-mono">{operation.operationId}</span>}
      actions={
        <span data-status={operation.status}>
          <StatusBadge tone={/failed/i.test(operation.status) ? "bad" : operation.status === "verified" ? "ok" : "neutral"} dot>
            {operation.status}
          </StatusBadge>
        </span>
      }
    >
      <div className="flex flex-col gap-3">
        <p className="text-sm">{operation.intentSummary}</p>
        <ol className="flex flex-col gap-1 text-sm" aria-label="Requested changes">
          {operation.actions.map((action) => {
            const change = operation.request.proposedChanges[action.index];
            const shown = actionApproval(action, session.role);
            return (
              <li key={action.index} className="flex flex-wrap items-center gap-2" data-approved={action.approval ? "yes" : "no"}>
                <span>
                  {action.kind} <span className="font-mono">{change?.outputId ?? change?.name ?? ""}</span>
                  {change?.desiredState ? ` (then ${change.desiredState})` : ""}
                </span>
                <StatusBadge tone={action.approval ? "ok" : "neutral"}>{shown.label}</StatusBadge>
                {step === "approve" && !action.approval && (
                  <button
                    type="button"
                    className="btn btn-xs btn-primary"
                    disabled={busy || !shown.canApprove}
                    title={shown.refusal ?? undefined}
                    onClick={() => advance(() => approveAction(operation.operationId, action.index, reason))}
                  >
                    Approve
                  </button>
                )}
              </li>
            );
          })}
        </ol>
        <p className="text-xs text-base-content/70">
          Each change is approved by a member whose role reaches it (removing a
          destination needs an admin); the engine records who approved each one
          and audits refusals. The engine keeps operations in memory only, so a
          restart forgets them.
        </p>
        {step === "approve" && (
          <Field label="Approval reason (audited)">
            <input
              className="input input-sm"
              value={reason}
              onChange={(event) => setReason(event.target.value)}
            />
          </Field>
        )}
        {operation.status === "verificationFailed" && (
          <p className="text-sm">
            Verification checks the engine at one moment; a destination that
            was just started can need a few seconds to connect.
          </p>
        )}
        <div className="flex gap-2">
          {(step === "apply" || step === "verify") && (
            <button
              type="button"
              className="btn btn-sm btn-primary"
              disabled={busy}
              onClick={() => advance(() => advanceOperation(operation.operationId, step))}
            >
              {step === "apply"
                ? "Apply"
                : operation.status === "verificationFailed"
                  ? "Verify again"
                  : "Verify"}
            </button>
          )}
          <button
            type="button"
            className="btn btn-sm btn-ghost"
            disabled={busy}
            onClick={props.onDone}
          >
            {props.doneLabel ?? "Start a new change"}
          </button>
        </div>
        <OperationResult operation={operation} />
        <Convergence operation={operation} live={props.live} />
      </div>
    </Panel>
  );
}

export function Judgement({ plan }: { plan: ChangePlan }) {
  const { validation, impact } = plan;
  const notes = [...plan.graphPreview.notes, ...impact.engineeringNotes];
  return (
    <Panel
      id="changes-judgement"
      title="Engine judgement"
      actions={
        <StatusBadge tone={validation.valid ? "ok" : "bad"} dot>
          {validation.valid ? "valid" : "invalid"}
        </StatusBadge>
      }
    >
      <div className="flex flex-col gap-2">
        {[...validation.errors, ...validation.warnings].map((issue, index) => (
          <p
            key={index}
            className={`text-sm ${issue.severity === "error" ? "text-error" : "text-warning"}`}
          >
            {issue.message}
            {issue.field ? <span className="text-base-content/60"> ({issue.field})</span> : null}
          </p>
        ))}
        <p className="text-sm">{impact.operatorSummary}</p>
        {impact.sharedStageCandidates.length > 0 && (
          <p className="text-sm">
            Stages it might share with other outputs: {impact.sharedStageCandidates.join(", ")}
          </p>
        )}
        {notes.length > 0 && (
          <ul className="list-disc pl-5 text-xs text-base-content/70">
            {notes.map((note, index) => (
              <li key={index}>{note}</li>
            ))}
          </ul>
        )}
      </div>
    </Panel>
  );
}

function GateTable(props: { label: string; rows: readonly Gate[] }) {
  return (
    <table className="table table-sm" aria-label={props.label}>
      <tbody>
        {props.rows.map((row, index) => (
          <tr key={index}>
            <td>{row.label}</td>
            <td className={row.passed ? "text-success" : "text-error"}>
              {row.passed ? "passed" : "failed"}
            </td>
            <td className="text-base-content/70">{row.detail}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function OperationResult({ operation }: { operation: ChangeOperation }) {
  const { gates, checks, observations } = verification(operation);
  const execution = operation.executionResult;
  return (
    <>
      {execution?.success === false && (
        <p className="text-sm text-error" role="alert">
          Apply failed: {execution.error}. Changes before the failing one may already
          be applied; check the destinations before trying again.
        </p>
      )}
      {execution?.success && (
        <ul className="text-sm" aria-label="Applied changes">
          {execution.changeResults.map((change, index) => (
            <li key={index}>
              {change.kind} <span className="font-mono">{change.outputId ?? ""}</span>: {change.status}
            </li>
          ))}
        </ul>
      )}
      {gates.length > 0 && <GateTable label="Verification gates" rows={gates} />}
      {checks.length > 0 && <GateTable label="Per-change checks" rows={checks} />}
      {operation.verificationResult?.explanation && (
        <p className="text-sm">{operation.verificationResult.explanation}</p>
      )}
      {observations.map((line, index) => (
        <p key={index} className="text-xs text-base-content/70">
          {line}
        </p>
      ))}
    </>
  );
}
