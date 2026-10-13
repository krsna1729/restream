import { useState } from "react";

import { runPipelineDiagnostics } from "../core/api.js";
import type { DiagnosticsReport } from "../types.js";
import { diagnosticsFile, diagnosticsVerdict } from "./diagnose-model.js";

/** Runs the engine's checks on the pipeline's live input (probe, GOP, ring,
 * outputs) on request: a run takes seconds and reads the stream. */
export function Diagnostics(props: { pipeline: { id: string; name: string } }) {
  const [running, setRunning] = useState(false);
  const [run, setRun] = useState<{ report: DiagnosticsReport; at: Date } | null>(null);
  const verdict = run ? diagnosticsVerdict(run.report) : null;
  return (
    <section className="card bg-base-200" aria-labelledby="inspect-diagnostics">
      <div className="card-body gap-3 p-4">
        <div className="flex flex-wrap items-center gap-3">
          <h3 id="inspect-diagnostics" className="card-title text-base">
            Diagnostics
          </h3>
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
          {run && verdict && (
            <>
              <span
                role="status"
                className={`badge ${
                  verdict.failed > 0 ? "badge-error" : verdict.withIssues > 0 ? "badge-warning" : "badge-success"
                }`}
              >
                {run.report.checks.length} checks · {verdict.failed} failed · {verdict.withIssues} with issues
              </span>
              <span className="text-xs opacity-70">
                {run.report.protocol.toUpperCase()} · {(run.report.totalDurationMs / 1000).toFixed(1)} s
              </span>
              <button
                type="button"
                className="btn btn-sm btn-ghost"
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
            </>
          )}
        </div>
        {run && (
          <ol className="space-y-2" aria-label="Diagnostic checks">
            {run.report.checks.map((check) => (
              <li key={check.index} aria-label={`Check ${check.name}`} className="rounded bg-base-100 p-2 text-sm">
                <div className="flex flex-wrap items-center gap-2">
                  <span
                    className={`badge badge-sm ${
                      check.exitCode !== 0 ? "badge-error" : check.issues.length > 0 ? "badge-warning" : "badge-success"
                    }`}
                  >
                    {check.exitCode !== 0
                      ? `failed (exit ${check.exitCode})`
                      : check.issues.length > 0
                        ? `${check.issues.length} issue${check.issues.length === 1 ? "" : "s"}`
                        : "ok"}
                  </span>
                  <span className="font-medium">{check.name}</span>
                  <span className="text-xs opacity-70">{check.durationMs} ms</span>
                </div>
                <p className="text-xs opacity-70">{check.description}</p>
                {check.issues.length > 0 && (
                  <ul className="list-disc pl-5 text-warning">
                    {check.issues.map((issue) => (
                      <li key={issue}>{issue}</li>
                    ))}
                  </ul>
                )}
                {check.help && <p className="text-xs italic opacity-70">{check.help}</p>}
                <details>
                  <summary className="cursor-pointer text-xs opacity-70">Output</summary>
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
        )}
      </div>
    </section>
  );
}
