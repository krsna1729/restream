import { useEffect, useMemo, useState } from "react";

import {
  advanceOperation,
  createOperation,
  getConfig,
  getOperation,
  planChange,
} from "../core/api.js";
import type { ChangeOperation, ChangePlan } from "../core/api-types.js";
import {
  CHANGE_KINDS,
  NEW_DRAFT,
  STAGES,
  changeRequest,
  nextStep,
  progress,
  verification,
} from "./changes-model.js";
import type { ChangeDraft, Gate, StageState } from "./changes-model.js";
import { Convergence, Impact } from "./changes-impact.js";
import type { LiveSnapshot } from "./store.js";

const STAGE_BADGE: Record<StageState, string> = {
  done: "badge-success",
  current: "badge-info",
  failed: "badge-error",
  todo: "badge-ghost",
};

/** The operation this tab is working on, so leaving the workspace or
 * reloading does not strand an approved but unapplied change. */
const OPERATION_KEY = "control-room.change-operation";

/** Change Studio: describe a change, let the engine judge it without touching
 * anything, then approve, apply and verify it as one operation. */
export function Changes({ live }: { live: LiveSnapshot }) {
  const [operation, setOperation] = useState<ChangeOperation | null>(null);
  const [plan, setPlan] = useState<ChangePlan | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const operationId = sessionStorage.getItem(OPERATION_KEY);
    if (operationId === null) return;
    // A 404 (engine restarted or evicted it) alerts once; the page starts over.
    void getOperation(operationId).then((stored) => {
      if (stored) setOperation(stored);
      else sessionStorage.removeItem(OPERATION_KEY);
    });
  }, []);

  const track = (next: ChangeOperation | null) => {
    setOperation(next);
    if (next) sessionStorage.setItem(OPERATION_KEY, next.operationId);
    else sessionStorage.removeItem(OPERATION_KEY);
  };
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    try {
      await action();
    } finally {
      setBusy(false);
    }
  };
  const stages = progress(plan, operation);

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3">
        <h2 className="text-xl font-semibold">Change Studio</h2>
        <ol className="flex gap-2" aria-label="Change progress">
          {STAGES.map((stage, index) => (
            <li key={stage} className={`badge ${STAGE_BADGE[stages[index]]}`} data-state={stages[index]}>
              {index + 1} · {stage}
            </li>
          ))}
        </ol>
      </div>
      {operation === null ? (
        <DefineChange
          live={live}
          busy={busy}
          run={run}
          onJudged={setPlan}
          onCreated={track}
        />
      ) : (
        <Operation
          live={live}
          operation={operation}
          busy={busy}
          run={run}
          onChange={track}
          onDone={() => {
            track(null);
            setPlan(null);
          }}
        />
      )}
    </div>
  );
}

type Run = (action: () => Promise<void>) => Promise<void>;

function DefineChange(props: {
  live: LiveSnapshot;
  busy: boolean;
  run: Run;
  onJudged: (plan: ChangePlan | null) => void;
  onCreated: (operation: ChangeOperation) => void;
}) {
  const { live, busy, run } = props;
  const pipelines = live.state?.pipelines ?? [];
  const [intent, setIntent] = useState("");
  const [pipelineId, setPipelineId] = useState("");
  const [drafts, setDrafts] = useState<readonly ChangeDraft[]>([NEW_DRAFT]);
  const [profiles, setProfiles] = useState<readonly string[]>([]);
  const [plan, setPlan] = useState<{ key: string; plan: ChangePlan } | null>(null);

  useEffect(() => {
    // Profile names for the video choice; read once, like Settings.
    void getConfig().then((config) => setProfiles(Object.keys(config?.transcodeProfiles ?? {})));
  }, []);

  const request = useMemo(
    () => changeRequest(intent, pipelineId, drafts),
    [intent, pipelineId, drafts],
  );
  const requestKey = JSON.stringify(request);
  // A preview speaks only for the exact change it judged.
  const currentPlan = plan?.key === requestKey ? plan.plan : null;
  const { onJudged } = props;
  useEffect(() => onJudged(currentPlan), [onJudged, currentPlan]);
  const outputs = (live.state?.outputs ?? []).filter((output) => output.pipelineId === pipelineId);
  const edit = (index: number, next: Partial<ChangeDraft>) =>
    setDrafts(drafts.map((draft, at) => (at === index ? { ...draft, ...next } : draft)));

  return (
    <>
      <section className="card bg-base-200" aria-labelledby="changes-define">
        <div className="card-body gap-3 p-4">
          <h3 id="changes-define" className="card-title text-base">
            Define the change
          </h3>
          <label className="form-control">
            <span className="label-text">Why (kept in the audit log)</span>
            <input
              className="input input-sm input-bordered"
              value={intent}
              onChange={(event) => setIntent(event.target.value)}
            />
          </label>
          <label className="form-control">
            <span className="label-text">Pipeline</span>
            <select
              className="select select-sm select-bordered"
              value={pipelineId}
              onChange={(event) => setPipelineId(event.target.value)}
            >
              <option value="">Choose a pipeline</option>
              {pipelines.map((pipeline) => (
                <option key={pipeline.id} value={pipeline.id}>
                  {pipeline.name}
                </option>
              ))}
            </select>
          </label>
          <ul className="space-y-2">
            {drafts.map((draft, index) => (
              <li key={index} className="rounded bg-base-300 p-3" aria-label={`Change ${index + 1}`}>
                <div className="grid gap-2 md:grid-cols-3">
                  <label className="form-control">
                    <span className="label-text text-xs">Change</span>
                    <select
                      className="select select-sm select-bordered"
                      value={draft.kind}
                      onChange={(event) =>
                        edit(index, { kind: event.target.value as ChangeDraft["kind"] })
                      }
                    >
                      {CHANGE_KINDS.map(([kind, label]) => (
                        <option key={kind} value={kind}>
                          {label}
                        </option>
                      ))}
                    </select>
                  </label>
                  {draft.kind !== "addOutput" && (
                    <label className="form-control">
                      <span className="label-text text-xs">Destination</span>
                      <select
                        className="select select-sm select-bordered"
                        value={draft.outputId}
                        onChange={(event) => edit(index, { outputId: event.target.value })}
                      >
                        <option value="">Choose a destination</option>
                        {outputs.map((output) => (
                          <option key={output.id} value={output.id}>
                            {output.name}
                          </option>
                        ))}
                      </select>
                    </label>
                  )}
                  {(draft.kind === "addOutput" || draft.kind === "updateOutput") && (
                    <>
                      <label className="form-control">
                        <span className="label-text text-xs">
                          Name{draft.kind === "updateOutput" ? " (empty keeps it)" : ""}
                        </span>
                        <input
                          className="input input-sm input-bordered"
                          value={draft.name}
                          onChange={(event) => edit(index, { name: event.target.value })}
                        />
                      </label>
                      <label className="form-control">
                        <span className="label-text text-xs">
                          URL{draft.kind === "updateOutput" ? " (empty keeps it)" : ""}
                        </span>
                        <input
                          className="input input-sm input-bordered font-mono"
                          value={draft.url}
                          onChange={(event) => edit(index, { url: event.target.value })}
                        />
                      </label>
                    </>
                  )}
                  {draft.kind === "addOutput" && (
                    <>
                      <label className="form-control">
                        <span className="label-text text-xs">Video</span>
                        <select
                          className="select select-sm select-bordered"
                          value={draft.profile}
                          onChange={(event) => edit(index, { profile: event.target.value })}
                        >
                          <option value="">Source (no transcode)</option>
                          {profiles.map((name) => (
                            <option key={name} value={name}>
                              Profile {name}
                            </option>
                          ))}
                        </select>
                      </label>
                      <label className="form-control">
                        <span className="label-text text-xs">After applying</span>
                        <select
                          className="select select-sm select-bordered"
                          value={draft.desiredState}
                          onChange={(event) =>
                            edit(index, {
                              desiredState: event.target.value as ChangeDraft["desiredState"],
                            })
                          }
                        >
                          <option value="stopped">stopped</option>
                          <option value="running">running</option>
                        </select>
                      </label>
                    </>
                  )}
                </div>
                {drafts.length > 1 && (
                  <button
                    type="button"
                    className="btn btn-ghost btn-xs mt-2"
                    onClick={() => setDrafts(drafts.filter((_, at) => at !== index))}
                  >
                    Remove this change
                  </button>
                )}
              </li>
            ))}
          </ul>
          <div className="flex flex-wrap gap-2">
            <button
              type="button"
              className="btn btn-sm btn-ghost"
              onClick={() => setDrafts([...drafts, NEW_DRAFT])}
            >
              Add a change
            </button>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              disabled={busy || !pipelineId}
              onClick={() =>
                run(async () => {
                  const judged = await planChange(request);
                  if (judged) setPlan({ key: requestKey, plan: judged });
                })
              }
            >
              Preview
            </button>
          </div>
        </div>
      </section>

      {currentPlan && <Judgement plan={currentPlan} />}
      {currentPlan?.graphPreview.topology && (
        <Impact topology={currentPlan.graphPreview.topology} pipelineId={pipelineId} />
      )}

      {currentPlan?.validation.valid && (
        <button
          type="button"
          className="btn btn-sm btn-primary"
          disabled={busy}
          onClick={() =>
            run(async () => {
              // The operation carries exactly the previewed request.
              const created = await createOperation(request);
              if (created) props.onCreated(created);
            })
          }
        >
          Create operation
        </button>
      )}
    </>
  );
}

function Operation(props: {
  live: LiveSnapshot;
  operation: ChangeOperation;
  busy: boolean;
  run: Run;
  onChange: (operation: ChangeOperation) => void;
  onDone: () => void;
}) {
  const { operation, busy, run } = props;
  const [reason, setReason] = useState("");
  const step = nextStep(operation.status);
  const label =
    step === "approve"
      ? "Approve"
      : step === "apply"
        ? "Apply"
        : operation.status === "verificationFailed"
          ? "Verify again"
          : "Verify";
  return (
    <section className="card bg-base-200" aria-labelledby="changes-operation">
      <div className="card-body gap-3 p-4">
        <div className="flex flex-wrap items-center gap-2">
          <h3 id="changes-operation" className="card-title text-base">
            Operation
          </h3>
          <span className="font-mono text-xs">{operation.operationId}</span>
          <span className="badge" data-status={operation.status}>
            {operation.status}
          </span>
        </div>
        <p className="text-sm">{operation.intentSummary}</p>
        <ul className="text-sm" aria-label="Requested changes">
          {operation.request.proposedChanges.map((change, index) => (
            <li key={index}>
              {change.kind} <span className="font-mono">{change.outputId ?? change.name ?? ""}</span>
              {change.desiredState ? ` (then ${change.desiredState})` : ""}
            </li>
          ))}
        </ul>
        <p className="text-xs opacity-70">
          Approval confirms the change from this dashboard session; it is not a
          second person's sign-off. The engine keeps operations in memory only, so
          a restart forgets them.
        </p>
        {step === "approve" && (
          <label className="form-control">
            <span className="label-text">Approval reason (audited)</span>
            <input
              className="input input-sm input-bordered"
              value={reason}
              onChange={(event) => setReason(event.target.value)}
            />
          </label>
        )}
        {operation.status === "verificationFailed" && (
          <p className="text-sm">
            Verification checks the engine at one moment; a destination that
            was just started can need a few seconds to connect.
          </p>
        )}
        <div className="flex gap-2">
          {step && (
            <button
              type="button"
              className="btn btn-sm btn-primary"
              disabled={busy}
              onClick={() =>
                run(async () => {
                  // A refused step (a failed apply answers 400 with the
                  // failed record) still changed the record: read it back.
                  const next =
                    (await advanceOperation(operation.operationId, step, reason)) ??
                    (await getOperation(operation.operationId));
                  // Both null (alerted): a network failure and a forgotten
                  // operation look alike, so the card stays and the
                  // operator chooses to start over.
                  if (next) props.onChange(next);
                })
              }
            >
              {label}
            </button>
          )}
          <button
            type="button"
            className="btn btn-sm btn-ghost"
            disabled={busy}
            onClick={props.onDone}
          >
            Start a new change
          </button>
        </div>
        <OperationResult operation={operation} />
        <Convergence operation={operation} live={props.live} />
      </div>
    </section>
  );
}

function Judgement({ plan }: { plan: ChangePlan }) {
  const { validation, impact } = plan;
  const notes = [...plan.graphPreview.notes, ...impact.engineeringNotes];
  return (
    <section className="card bg-base-200" aria-labelledby="changes-judgement">
      <div className="card-body gap-2 p-4">
        <div className="flex items-center gap-2">
          <h3 id="changes-judgement" className="card-title text-base">
            Engine judgement
          </h3>
          <span className={`badge ${validation.valid ? "badge-success" : "badge-error"}`}>
            {validation.valid ? "valid" : "invalid"}
          </span>
        </div>
        {[...validation.errors, ...validation.warnings].map((issue, index) => (
          <p
            key={index}
            className={`text-sm ${issue.severity === "error" ? "text-error" : "text-warning"}`}
          >
            {issue.message}
            {issue.field ? <span className="opacity-60"> ({issue.field})</span> : null}
          </p>
        ))}
        <p className="text-sm">{impact.operatorSummary}</p>
        {impact.sharedStageCandidates.length > 0 && (
          <p className="text-sm">
            Stages it might share with other outputs: {impact.sharedStageCandidates.join(", ")}
          </p>
        )}
        {notes.length > 0 && (
          <ul className="list-disc pl-5 text-xs opacity-70">
            {notes.map((note, index) => (
              <li key={index}>{note}</li>
            ))}
          </ul>
        )}
      </div>
    </section>
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
            <td className="opacity-70">{row.detail}</td>
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
        <p key={index} className="text-xs opacity-70">
          {line}
        </p>
      ))}
    </>
  );
}
