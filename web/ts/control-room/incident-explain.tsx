// "Explain this incident": the engine reads the pipeline's flight record over
// the incident, names the layer that broke and what it ruled out, and
// proposes a fix as a plan. The operator creates the operation and each
// change is approved by a role that reaches it (`change-operation.tsx`).

import { useState } from "react";

import { createOperation, explainIncident } from "../core/api.js";
import type { ChangeOperation, IncidentExplanationResponse } from "../core/agent-types.js";
import type { OperatorAlert } from "../core/api-types.js";
import { Judgement, Operation } from "./change-operation.js";
import type { Run } from "./change-operation.js";
import type { LiveSnapshot } from "./store.js";
import { Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";

export function Explain(props: { alert: OperatorAlert; live: LiveSnapshot }) {
  const { alert } = props;
  const [result, setResult] = useState<IncidentExplanationResponse | null>(null);
  const [operation, setOperation] = useState<ChangeOperation | null>(null);
  const [busy, setBusy] = useState(false);
  const run: Run = async (action) => {
    setBusy(true);
    try {
      await action();
    } finally {
      setBusy(false);
    }
  };

  if (operation) {
    return (
      <Operation
        live={props.live}
        operation={operation}
        busy={busy}
        run={run}
        onChange={setOperation}
        onDone={() => setOperation(null)}
        doneLabel="Back to the explanation"
      />
    );
  }
  return (
    <Panel
      id="incident-explain"
      title="Explain this incident"
      subtitle="From the pipeline's flight record over the incident"
      actions={
        <button
          type="button"
          className="btn btn-sm btn-primary"
          disabled={busy}
          onClick={() =>
            run(async () => setResult(await explainIncident(alert.id, alert.firstSeen ?? null)))
          }
        >
          {result ? "Explain again" : "Explain"}
        </button>
      }
    >
      {result === null ? (
        <p className="text-sm text-base-content/70">
          Reads every recorded sample from a minute before this opened to its
          end, walks ingest, feed, shard and destination in that order, and
          proposes a fix when a plant change can make one.
        </p>
      ) : (
        <Explanation
          result={result}
          busy={busy}
          onCreate={() =>
            run(async () => {
              const proposal = result.explanation.proposal;
              if (!proposal) return;
              const created = await createOperation({ ...proposal, incidentId: alert.id });
              if (created) setOperation(created);
            })
          }
        />
      )}
    </Panel>
  );
}

function Explanation(props: {
  result: IncidentExplanationResponse;
  busy: boolean;
  onCreate: () => void;
}) {
  const { explanation, plan } = props.result;
  const { window } = explanation;
  return (
    <div className="flex flex-col gap-3" data-layer={explanation.layer ?? "unknown"}>
      <div className="flex flex-wrap items-center gap-2">
        <StatusBadge tone={explanation.layer ? "bad" : "neutral"} dot>
          {explanation.layer ? `${explanation.layer} layer` : "layer not determined"}
        </StatusBadge>
        <span className="text-xs text-base-content/70">
          {window.incidentSamples} samples over the incident, {window.samples} read
        </span>
      </div>
      <p className="text-sm">{explanation.summary}</p>
      {explanation.evidence.length > 0 && (
        <ul className="list-disc pl-5 text-sm" aria-label="Evidence">
          {explanation.evidence.map((line, index) => (
            <li key={index}>{line}</li>
          ))}
        </ul>
      )}
      {explanation.ruledOut.length > 0 && (
        <ul className="text-sm" aria-label="Ruled out">
          {explanation.ruledOut.map((ruled) => (
            <li key={ruled.layer}>
              <StatusBadge tone="ok">{ruled.layer} ruled out</StatusBadge>{" "}
              <span className="text-base-content/70">{ruled.reason}</span>
            </li>
          ))}
        </ul>
      )}
      <p className="text-sm">
        <b>What to do:</b> {explanation.recommendation}
      </p>
      {plan === null ? (
        <p className="text-sm text-base-content/70">No plant change fixes this cause.</p>
      ) : (
        <>
          <p className="text-sm">
            <b>Proposed:</b> {explanation.proposal?.intent}
          </p>
          <Judgement plan={plan} />
          {plan.validation.valid && (
            <button
              type="button"
              className="btn btn-sm btn-primary self-start"
              disabled={props.busy}
              onClick={props.onCreate}
            >
              Create operation
            </button>
          )}
        </>
      )}
    </div>
  );
}
