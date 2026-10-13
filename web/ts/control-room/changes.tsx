import { useEffect, useMemo, useState } from "react";

import { advanceOperation, createOperation, getConfig, planChange } from "../core/api.js";
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
import type { ChangeDraft, StageState } from "./changes-model.js";
import type { LiveSnapshot } from "./store.js";

const STAGE_BADGE: Record<StageState, string> = {
  done: "badge-success",
  current: "badge-info",
  failed: "badge-error",
  todo: "badge-ghost",
};

const STEP_LABEL = { approve: "Approve", apply: "Apply", verify: "Verify" } as const;

/** Change Studio: describe a change, let the engine judge it without touching
 * anything, then approve, apply and verify it as one operation. */
export function Changes({ live }: { live: LiveSnapshot }) {
  const pipelines = live.state?.pipelines ?? [];
  const [intent, setIntent] = useState("");
  const [pipelineId, setPipelineId] = useState("");
  const [drafts, setDrafts] = useState<readonly ChangeDraft[]>([NEW_DRAFT]);
  const [profiles, setProfiles] = useState<readonly string[]>([]);
  const [plan, setPlan] = useState<{ key: string; plan: ChangePlan } | null>(null);
  const [operation, setOperation] = useState<ChangeOperation | null>(null);
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);

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
  const outputs = (live.state?.outputs ?? []).filter((output) => output.pipelineId === pipelineId);
  const stages = progress(currentPlan, operation);
  const step = operation ? nextStep(operation.status) : null;
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    try {
      await action();
    } finally {
      setBusy(false);
    }
  };
  const edit = (index: number, next: Partial<ChangeDraft>) =>
    setDrafts(drafts.map((draft, at) => (at === index ? { ...draft, ...next } : draft)));

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
              disabled={operation !== null}
              onChange={(event) => setIntent(event.target.value)}
            />
          </label>
          <label className="form-control">
            <span className="label-text">Pipeline</span>
            <select
              className="select select-sm select-bordered"
              value={pipelineId}
              disabled={operation !== null}
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
                      disabled={operation !== null}
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
                        disabled={operation !== null}
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
                          disabled={operation !== null}
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
                          disabled={operation !== null}
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
                          disabled={operation !== null}
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
                          disabled={operation !== null}
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
                {drafts.length > 1 && operation === null && (
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
              disabled={operation !== null}
              onClick={() => setDrafts([...drafts, NEW_DRAFT])}
            >
              Add a change
            </button>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              disabled={busy || !pipelineId || operation !== null}
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

      {currentPlan?.validation.valid && operation === null && (
        <button
          type="button"
          className="btn btn-sm btn-primary"
          disabled={busy}
          onClick={() =>
            run(async () => {
              // The operation carries exactly the previewed request.
              const created = await createOperation(request);
              if (created) setOperation(created);
            })
          }
        >
          Create operation
        </button>
      )}

      {operation && (
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
            <div className="flex gap-2">
              {step && (
                <button
                  type="button"
                  className="btn btn-sm btn-primary"
                  disabled={busy}
                  onClick={() =>
                    run(async () => {
                      const next = await advanceOperation(operation.operationId, step, reason);
                      if (next) setOperation(next);
                    })
                  }
                >
                  {STEP_LABEL[step]}
                </button>
              )}
              {step === null && (
                <button
                  type="button"
                  className="btn btn-sm btn-ghost"
                  onClick={() => {
                    setOperation(null);
                    setPlan(null);
                    setDrafts([NEW_DRAFT]);
                    setIntent("");
                    setReason("");
                  }}
                >
                  Start a new change
                </button>
              )}
            </div>
            <OperationResult operation={operation} />
          </div>
        </section>
      )}
    </div>
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
            key={`${index}:${issue.code}`}
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
            {notes.map((note) => (
              <li key={note}>{note}</li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

function OperationResult({ operation }: { operation: ChangeOperation }) {
  const { gates, observations } = verification(operation);
  return (
    <>
      {operation.executionResult && (
        <ul className="text-sm" aria-label="Applied changes">
          {operation.executionResult.changeResults.map((change, index) => (
            <li key={`${index}:${change.kind}`}>
              {change.kind} <span className="font-mono">{change.outputId ?? ""}</span>: {change.status}
            </li>
          ))}
        </ul>
      )}
      {gates.length > 0 && (
        <table className="table table-sm" aria-label="Verification">
          <tbody>
            {gates.map((gate) => (
              <tr key={gate.label}>
                <td>{gate.label}</td>
                <td className={gate.passed ? "text-success" : "text-error"}>
                  {gate.passed ? "passed" : "failed"}
                </td>
                <td className="opacity-70">{gate.detail}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {operation.verificationResult?.explanation && (
        <p className="text-sm">{operation.verificationResult.explanation}</p>
      )}
      {observations.map((line) => (
        <p key={line} className="text-xs opacity-70">
          {line}
        </p>
      ))}
      <details>
        <summary className="cursor-pointer text-xs opacity-70">
          Audit log ({operation.auditLog.length})
        </summary>
        <ol className="mt-1 text-xs">
          {operation.auditLog.map((event, index) => (
            <li key={`${index}:${event.kind}`}>
              <span className="font-mono">{event.at}</span> {event.kind}: {event.summary}
            </li>
          ))}
        </ol>
      </details>
    </>
  );
}
