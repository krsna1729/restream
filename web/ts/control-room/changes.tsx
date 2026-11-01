import { useEffect, useMemo, useState } from "react";

import {
  createOperation,
  getConfig,
  getOperation,
  planChange,
} from "../core/api.js";
import type { ChangeOperation, ChangePlan } from "../core/agent-types.js";
import {
  CHANGE_KINDS,
  NEW_DRAFT,
  STAGES,
  changeRequest,
  draftFor,
  progress,
} from "./changes-model.js";
import type { ChangeDraft, StageState } from "./changes-model.js";
import { Impact } from "./changes-impact.js";
import { Judgement, Operation } from "./change-operation.js";
import type { Run } from "./change-operation.js";
import type { ChangeIntent } from "./route.js";
import { useGo, usePlace } from "./router.js";
import type { LiveSnapshot } from "./store.js";
import { PageHeader } from "../ui/page-header.js";
import { Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";
import { Field } from "../ui/controls.js";

const STAGE_TONE: Record<StageState, Tone> = {
  done: "ok",
  current: "info",
  failed: "bad",
  todo: "neutral",
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
  // A command from the palette arrives as a change to draft (`c`).
  const place = usePlace();
  const go = useGo();
  const seed = place.change ? { change: place.change, pipelineId: place.pipelineId } : null;

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
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="Change Studio"
        title="Plan, approve, apply and verify"
        subtitle="Destination changes judged by the engine before anything mutates."
      />
      <div className="flex flex-wrap items-center gap-3">
        <ol className="flex gap-2" aria-label="Change progress">
          {STAGES.map((stage, index) => (
            <li key={stage} data-state={stages[index]}>
              <StatusBadge tone={STAGE_TONE[stages[index]]}>
                {index + 1} · {stage}
              </StatusBadge>
            </li>
          ))}
        </ol>
      </div>
      {operation === null ? (
        <DefineChange
          key={seed ? `${seed.change.kind}:${seed.change.outputId}` : "blank"}
          seed={seed}
          live={live}
          busy={busy}
          run={run}
          onJudged={setPlan}
          onCreated={(created) => {
            track(created);
            // The drafted command is now this operation; a new change starts blank.
            if (seed) go({ workspace: "changes", pipelineId: place.pipelineId });
          }}
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

/** A palette command: the change to draft and the pipeline it is on. */
interface Seed {
  readonly change: ChangeIntent;
  readonly pipelineId: string | null;
}

function DefineChange(props: {
  seed: Seed | null;
  live: LiveSnapshot;
  busy: boolean;
  run: Run;
  onJudged: (plan: ChangePlan | null) => void;
  onCreated: (operation: ChangeOperation) => void;
}) {
  const { seed, live, busy, run } = props;
  const pipelines = live.state?.pipelines ?? [];
  const [intent, setIntent] = useState(() => {
    if (!seed) return "";
    const label = CHANGE_KINDS.find(([kind]) => kind === seed.change.kind)?.[1] ?? seed.change.kind;
    const output = live.state?.outputs.find((candidate) => candidate.id === seed.change.outputId);
    return `${label}: ${output?.name ?? seed.change.outputId}`;
  });
  const [pipelineId, setPipelineId] = useState(seed?.pipelineId ?? "");
  const [drafts, setDrafts] = useState<readonly ChangeDraft[]>(() =>
    seed ? [draftFor(seed.change)] : [NEW_DRAFT],
  );
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
  // A drafted command is judged at once (read-only): its plan shows before
  // anything can be created or approved.
  useEffect(() => {
    if (!seed) return;
    void run(async () => {
      const judged = await planChange(request);
      if (judged) setPlan({ key: requestKey, plan: judged });
    });
    // Once per drafted command; the component is keyed on it.
  }, []);
  // A preview speaks only for the exact change it judged.
  const currentPlan = plan?.key === requestKey ? plan.plan : null;
  const { onJudged } = props;
  useEffect(() => onJudged(currentPlan), [onJudged, currentPlan]);
  const outputs = (live.state?.outputs ?? []).filter((output) => output.pipelineId === pipelineId);
  const edit = (index: number, next: Partial<ChangeDraft>) =>
    setDrafts(drafts.map((draft, at) => (at === index ? { ...draft, ...next } : draft)));

  return (
    <>
      <Panel id="changes-define" title="Define the change">
        <div className="flex flex-col gap-3">
          <Field label="Why (kept in the audit log)">
            <input
              className="input input-sm"
              value={intent}
              onChange={(event) => setIntent(event.target.value)}
            />
          </Field>
          <Field label="Pipeline">
            <select
              className="select select-sm"
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
          </Field>
          <ul className="space-y-2">
            {drafts.map((draft, index) => (
              <li key={index} className="rounded bg-base-300 p-3" aria-label={`Change ${index + 1}`}>
                <div className="grid gap-2 md:grid-cols-3">
                  <Field label="Change">
                    <select
                      className="select select-sm"
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
                  </Field>
                  {draft.kind !== "addOutput" && (
                    <Field label="Destination">
                      <select
                        className="select select-sm"
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
                    </Field>
                  )}
                  {(draft.kind === "addOutput" || draft.kind === "updateOutput") && (
                    <>
                      <Field label={<>Name{draft.kind === "updateOutput" ? " (empty keeps it)" : ""}</>}>
                        <input
                          className="input input-sm"
                          value={draft.name}
                          onChange={(event) => edit(index, { name: event.target.value })}
                        />
                      </Field>
                      <Field label={<>URL{draft.kind === "updateOutput" ? " (empty keeps it)" : ""}</>}>
                        <input
                          className="input input-sm font-mono"
                          value={draft.url}
                          onChange={(event) => edit(index, { url: event.target.value })}
                        />
                      </Field>
                    </>
                  )}
                  {draft.kind === "addOutput" && (
                    <>
                      <Field label="Video">
                        <select
                          className="select select-sm"
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
                      </Field>
                      <Field label="After applying">
                        <select
                          className="select select-sm"
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
                      </Field>
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
      </Panel>

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
