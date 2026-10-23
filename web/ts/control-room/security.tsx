import { useState } from "react";

import {
  changePassword,
  dismissPasswordChangePrompt,
  getRateLimitState,
  logout,
  resetRateLimitState,
} from "../core/api.js";
import type { ConfigData, IngestSecurityConfig } from "../types.js";
import { Panel } from "../ui/panel.js";
import { Field } from "../ui/controls.js";
import { Freshness, usePolled } from "./polled.js";
import { SettingsForm } from "./settings-form.js";
import type { Saver } from "./settings-form.js";
import { banLabel, passwordProblem, resetOne, scopeLabel } from "./settings-model.js";

/** Who may sign in and publish: the dashboard password, the failed-attempt
 * limits, and the clients those limits are tracking or banning now. */
export function Security({ config, save }: { config: ConfigData; save: Saver }) {
  return (
    <>
      <Password recommended={config.dashboardPasswordChangeRecommended ?? false} />
      {config.ingestSecurity && <IngestSecurity current={config.ingestSecurity} save={save} />}
      <Attempts />
    </>
  );
}

function Password({ recommended }: { recommended: boolean }) {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [prompt, setPrompt] = useState(recommended);
  return (
    <SettingsForm
      id="settings-password"
      title="Dashboard password"
      problem={passwordProblem(current, next, confirm)}
      onSave={async () => {
        if ((await changePassword(current, next)) === null) return false;
        setCurrent("");
        setNext("");
        setConfirm("");
        setPrompt(false);
        return true;
      }}
    >
      {prompt && (
        <div role="alert" className="alert alert-warning text-sm">
          <span>This dashboard still uses its first password. Change it.</span>
          <button
            type="button"
            className="btn btn-xs"
            onClick={async () => {
              if ((await dismissPasswordChangePrompt()) !== null) setPrompt(false);
            }}
          >
            Keep it
          </button>
        </div>
      )}
      {(
        [
          ["Current password", current, setCurrent, "current-password"],
          ["New password", next, setNext, "new-password"],
          ["New password again", confirm, setConfirm, "new-password"],
        ] as const
      ).map(([label, value, set, autoComplete]) => (
        <Field key={label} label={label}>
          <input
            type="password"
            className="input input-sm"
            autoComplete={autoComplete}
            value={value}
            onChange={(event) => set(event.target.value)}
          />
        </Field>
      ))}
      <button
        type="button"
        className="btn btn-sm btn-outline self-start"
        onClick={async () => {
          await logout();
          window.location.reload();
        }}
      >
        Log out
      </button>
    </SettingsForm>
  );
}

const LIMIT_FIELDS: ReadonlyArray<[keyof IngestSecurityConfig, string]> = [
  ["failureLimit", "Failures before a ban"],
  ["failureWindowMs", "Counting window (ms)"],
  ["banMs", "Ban length (ms)"],
  ["trackedIpLimit", "Clients tracked at most"],
];

function IngestSecurity(props: { current: IngestSecurityConfig; save: Saver }) {
  const [limits, setLimits] = useState(props.current);
  return (
    <SettingsForm
      id="settings-limits"
      title="Failed-attempt limits"
      onSave={() => props.save({ ingestSecurity: limits })}
    >
      <p className="text-xs text-base-content/70">
        A client that fails to sign in or publish this often within the window is
        banned for the ban length.
      </p>
      <div className="grid grid-cols-2 gap-2">
        {LIMIT_FIELDS.map(([field, label]) => (
          <Field key={field} label={label}>
            <input
              type="number"
              min={1}
              required
              className="input input-sm"
              value={limits[field]}
              onChange={(event) => setLimits({ ...limits, [field]: Number(event.target.value) })}
            />
          </Field>
        ))}
      </div>
    </SettingsForm>
  );
}

function Attempts() {
  const state = usePolled(["rate-limit-state"], getRateLimitState);
  const attempts = state.value?.attempts ?? null;
  return (
    <Panel
      id="settings-attempts"
      title="Failed attempts"
      actions={
        <>
          <Freshness atMs={state.atMs} />
          {attempts !== null && attempts.length > 0 && (
            <button
              type="button"
              className="btn btn-xs btn-outline border-base-content/20"
              onClick={async () => {
                // No scope: the engine forgets every client in every scope.
                if ((await resetRateLimitState({})) !== null) state.refresh();
              }}
            >
              Forget all
            </button>
          )}
        </>
      }
    >
      <div className="flex flex-col gap-3">
        {attempts === null ? (
          <p className="text-sm text-base-content/70">Loading…</p>
        ) : attempts.length === 0 ? (
          <p className="text-sm text-base-content/70">No client has failed recently.</p>
        ) : (
          <table className="table table-sm" aria-label="Failed attempts">
            <thead>
              <tr>
                <th>Where</th>
                <th>Client</th>
                <th>Failures</th>
                <th>State</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {attempts.map((attempt) => (
                <tr key={`${attempt.scope}|${attempt.ip}`}>
                  <td>{scopeLabel(attempt.scope)}</td>
                  <td className="font-mono">{attempt.ip}</td>
                  <td className="tabular-nums">{attempt.failureCount}</td>
                  <td className={attempt.banned ? "text-warning" : undefined}>
                    {banLabel(attempt)}
                  </td>
                  <td>
                    <button
                      type="button"
                      className="btn btn-xs"
                      aria-label={`Forget ${attempt.ip} for ${scopeLabel(attempt.scope)}`}
                      onClick={async () => {
                        if ((await resetRateLimitState(resetOne(attempt))) !== null) {
                          state.refresh();
                        }
                      }}
                    >
                      Forget
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </Panel>
  );
}
