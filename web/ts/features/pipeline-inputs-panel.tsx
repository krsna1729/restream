import { useEffect, useState } from "react";

import type { PipelineInput } from "../types.js";
import { PipelineInputRow } from "./pipeline-input-row.js";
import type { PipelineInputsPanelActions } from "./pipeline-inputs-contract.js";

const MAX_PIPELINE_INPUTS = 4;

export function PipelineInputsPanel({
  actions,
  pipelineId,
}: {
  actions: PipelineInputsPanelActions;
  pipelineId: string;
}): React.JSX.Element {
  const [inputs, setInputs] = useState<PipelineInput[] | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);
  const [busyInputId, setBusyInputId] = useState<string | null>(null);
  const [createExpanded, setCreateExpanded] = useState(false);
  const [newLabel, setNewLabel] = useState("");

  useEffect(() => {
    let active = true;
    setInputs(null);
    void actions.listInputs(pipelineId).then((response) => {
      if (active) setInputs(response?.inputs ?? []);
    });
    return () => {
      active = false;
    };
  }, [actions, pipelineId, refreshKey]);

  useEffect(() => {
    setCreateExpanded(false);
    setNewLabel("");
  }, [pipelineId]);

  const refresh = (): void => setRefreshKey((current) => current + 1);
  const runInputMutation = async (
    inputId: string,
    mutation: () => Promise<unknown>,
  ): Promise<void> => {
    setBusyInputId(inputId);
    try {
      await mutation();
      refresh();
    } finally {
      setBusyInputId(null);
    }
  };

  const create = async (): Promise<void> => {
    const label = newLabel.trim();
    if (!label) return;
    setBusyInputId("create");
    try {
      const response = await actions.createInput(pipelineId, label);
      if (!response) return;
      setCreateExpanded(false);
      setNewLabel("");
      refresh();
    } finally {
      setBusyInputId(null);
    }
  };

  return (
    <section
      aria-labelledby="pipeline-inputs-title"
      className="border-base-content/10 mt-4 border-t pt-3"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div>
          <h3
            className="text-base-content/70 text-xs font-semibold uppercase tracking-wide"
            id="pipeline-inputs-title"
          >
            Pipeline inputs
          </h3>
          <p className="text-base-content/55 mt-1 text-xs tabular-nums">
            {inputs === null
              ? "Loading"
              : `${inputs.length}/${MAX_PIPELINE_INPUTS} configured`}
          </p>
        </div>
        <button
          className="btn btn-xs btn-accent btn-outline"
          disabled={
            inputs === null ||
            inputs.length >= MAX_PIPELINE_INPUTS ||
            busyInputId !== null
          }
          onClick={() => setCreateExpanded((expanded) => !expanded)}
          type="button"
        >
          Add input
        </button>
      </div>

      {createExpanded ? (
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <input
            aria-label="New pipeline input label"
            autoFocus
            className="input input-bordered input-sm min-w-48 flex-1"
            maxLength={128}
            onChange={(event) => setNewLabel(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void create();
              if (event.key === "Escape") setCreateExpanded(false);
            }}
            placeholder="Encoder B"
            value={newLabel}
          />
          <button
            className="btn btn-sm btn-accent"
            disabled={!newLabel.trim() || busyInputId === "create"}
            onClick={() => void create()}
            type="button"
          >
            {busyInputId === "create" ? "Adding" : "Add"}
          </button>
          <button
            className="btn btn-sm btn-ghost"
            disabled={busyInputId === "create"}
            onClick={() => setCreateExpanded(false)}
            type="button"
          >
            Cancel
          </button>
        </div>
      ) : null}

      {inputs === null ? (
        <div className="dashboard-empty mt-3" role="status">
          Loading inputs...
        </div>
      ) : (
        <div className="border-base-content/10 divide-base-content/10 mt-3 divide-y border-y">
          {inputs.map((input) => {
            return (
              <PipelineInputRow
                actions={actions}
                busy={busyInputId === input.id}
                input={input}
                key={input.id}
                pipelineId={pipelineId}
                runMutation={runInputMutation}
              />
            );
          })}
        </div>
      )}
    </section>
  );
}
