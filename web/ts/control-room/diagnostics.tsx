import { useState } from "react";

import { runPipelineDiagnostics } from "../core/api.js";
import type { DiagnosticsReport } from "../types.js";
import { Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
import { diagnosticsFile, diagnosticsVerdict } from "./diagnose-model.js";

/** Runs the engine's checks on the pipeline's live input (probe, GOP, ring,
 * outputs) on request: a run takes seconds and reads the stream. */
export function Diagnostics(props: { pipeline: { id: string; name: string } }) {
  const [running, setRunning] = useState(false);
  const [run, setRun] = useState<{ report: DiagnosticsReport; at: Date } | null>(null);
  const verdict = run ? diagnosticsVerdict(run.report) : null;
  return (
    <Panel
      id="inspect-diagnostics"
      title="Diagnostics"
      subtitle={run ? `${run.report.protocol.toUpperCase()} · ${(run.report.totalDurationMs / 1000).toFixed(1)} s` : undefined}
      actions={
        <>
          {run && verdict && (
            <span role="status">
              <StatusBadge tone={verdict.failed > 0 ? "bad" : verdict.withIssues > 0 ? "warn" : "ok"} dot>
                {run.report.checks.length} checks · {verdict.failed} failed · {verdict.withIssues} with issues
              </StatusBadge>
            </span>
          )}
          <button
            type="button"
            className="btn btn-sm btn-primary"
            disabled={running}
            onClick={async () => {
              setRunning(true);
              try {
                const report = await runPipelineDiagnostics(props.pipeline.id);
                if (report) setRun({ report, at: new Date() });
              } finally {
                setRunning(false);
              }
            }}
          >
            {running ? "Running checks…" : run ? "Run again" : "Run diagnostics"}
          </button>
          {run && (
              <button
                type="button"
                className="btn btn-sm btn-outline border-base-content/20"
                onClick={() => {
                  const file = diagnosticsFile(props.pipeline, run.report, run.at);
                  const url = URL.createObjectURL(new Blob([file.json], { type: "application/json" }));
                  const link = document.createElement("a");
                  link.href = url;
                  link.download = file.name;
                  link.click();
                  URL.revokeObjectURL(url);
                }}
              >
                Download report
              </button>
          )}
        </>
      }
    >
        {run ? (
          <ol className="space-y-2" aria-label="Diagnostic checks">
            {run.report.checks.map((check) => (
              <li key={check.index} aria-label={`Check ${check.name}`} className="rounded-box border border-base-content/10 bg-base-100 p-2 text-sm">
                <div className="flex flex-wrap items-center gap-2">
                  <StatusBadge tone={check.exitCode !== 0 ? "bad" : check.issues.length > 0 ? "warn" : "ok"}>
                    {check.exitCode !== 0
                      ? `failed (exit ${check.exitCode})`
                      : check.issues.length > 0
                        ? `${check.issues.length} issue${check.issues.length === 1 ? "" : "s"}`
                        : "ok"}
                  </StatusBadge>
                  <span className="font-medium">{check.name}</span>
                  <span className="text-xs text-base-content/70">{check.durationMs} ms</span>
                </div>
                <p className="text-xs text-base-content/70">{check.description}</p>
                {check.issues.length > 0 && (
                  <ul className="list-disc pl-5 text-warning">
                    {check.issues.map((issue, index) => (
                      <li key={index}>{issue}</li>
                    ))}
                  </ul>
                )}
                {check.help && <p className="text-xs italic text-base-content/70">{check.help}</p>}
                <details>
                  <summary className="cursor-pointer text-xs text-base-content/70">Output</summary>
                  <pre className="max-h-80 overflow-auto whitespace-pre-wrap text-xs">
                    $ {check.command}
                    {"\n"}
                    {check.stdout}
                    {check.stderr && `\n[stderr] ${check.stderr}`}
                  </pre>
                </details>
              </li>
            ))}
          </ol>
        ) : (
          <p className="text-sm text-base-content/70">Runs the engine's checks on the live input: probe, keyframes, ring and outputs. A run takes seconds.</p>
        )}
    </Panel>
  );
}
