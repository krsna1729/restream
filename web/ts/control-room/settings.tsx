import { useEffect, useState } from "react";

import { getConfig, patchConfig } from "../core/api.js";
import type { TranscodeProfile } from "../core/api-types.js";
import type { ConfigData, ConfigOutput, SrtGlobalIngestConfig } from "../types.js";
import { SettingsForm } from "./settings-form.js";
import type { Saver } from "./settings-form.js";
import { Security } from "./security.js";
import {
  BUILT_IN_PROFILES,
  NEW_PROFILE,
  X264_PRESETS,
  X264_TUNES,
  nextProfileName,
  passphraseProblem,
  profileRows,
  profilesToSave,
} from "./settings-model.js";
import type { ProfileRow } from "./settings-model.js";

/** Engine settings. `/settings` is read once when the page opens (it also
 * lists every pipeline and output); each form saves only its own part and
 * shows what the engine stored. */
export function Settings() {
  const [config, setConfig] = useState<ConfigData | null>(null);
  useEffect(() => {
    void getConfig().then(setConfig);
  }, []);
  if (config === null) return <p className="opacity-70">Loading settings…</p>;
  const save: Saver = async (patch) => {
    const stored = await patchConfig(patch);
    if (stored === null) return false;
    setConfig((current) => (current ? { ...current, ...stored } : current));
    return true;
  };
  return (
    <div className="space-y-4">
      <h2 className="text-xl font-semibold">Settings</h2>
      <div className="grid gap-4 lg:grid-cols-2">
        <General config={config} save={save} />
        <SrtIngest current={config.srtIngest} save={save} />
        <Profiles
          current={config.transcodeProfiles ?? {}}
          outputs={config.outputs}
          save={save}
        />
        <Security config={config} save={save} />
      </div>
    </div>
  );
}

function General({ config, save }: { config: ConfigData; save: Saver }) {
  const [serverName, setServerName] = useState(config.serverName ?? "");
  const [ingestHost, setIngestHost] = useState(config.ingestHost ?? "");
  const [retainTs, setRetainTs] = useState(config.recordingSettings?.retainSourceTs ?? false);
  return (
    <SettingsForm
      id="settings-general"
      title="General"
      problem={serverName.trim() ? null : "The server name cannot be empty."}
      onSave={() =>
        save({
          serverName: serverName.trim(),
          ingestHost: ingestHost.trim(),
          recordingSettings: { retainSourceTs: retainTs },
        })
      }
    >
      <label className="form-control">
        <span className="label-text">Server name</span>
        <input
          className="input input-sm input-bordered"
          value={serverName}
          onChange={(event) => setServerName(event.target.value)}
        />
      </label>
      <label className="form-control">
        <span className="label-text">Ingest host</span>
        <input
          className="input input-sm input-bordered"
          placeholder="the address publishers use; empty uses this host"
          value={ingestHost}
          onChange={(event) => setIngestHost(event.target.value)}
        />
      </label>
      <label className="label cursor-pointer justify-start gap-2">
        <input
          type="checkbox"
          className="checkbox checkbox-sm"
          checked={retainTs}
          onChange={(event) => setRetainTs(event.target.checked)}
        />
        <span className="label-text">Keep the recorded TS after converting it to MP4</span>
      </label>
    </SettingsForm>
  );
}

function SrtIngest(props: { current: SrtGlobalIngestConfig | undefined; save: Saver }) {
  const [mode, setMode] = useState(props.current?.mode ?? "plaintext");
  const [passphrase, setPassphrase] = useState(props.current?.passphrase ?? "");
  const [pbkeylen, setPbkeylen] = useState(props.current?.pbkeylen ?? 16);
  const [latencyMs, setLatencyMs] = useState(props.current?.latencyMs ?? 250);
  const encrypted = mode === "encrypted";
  return (
    <SettingsForm
      id="settings-srt"
      title="SRT ingest"
      problem={encrypted ? passphraseProblem(passphrase) : null}
      onSave={() =>
        props.save({
          srtIngest: {
            mode,
            passphrase: encrypted ? passphrase.trim() : null,
            pbkeylen,
            latencyMs,
          },
        })
      }
    >
      <p className="text-xs opacity-70">
        Every SRT publisher without a pipeline override uses these.
      </p>
      <label className="form-control">
        <span className="label-text">Encryption</span>
        <select
          className="select select-sm select-bordered"
          value={mode}
          onChange={(event) => setMode(event.target.value === "encrypted" ? "encrypted" : "plaintext")}
        >
          <option value="plaintext">plaintext</option>
          <option value="encrypted">encrypted</option>
        </select>
      </label>
      {encrypted && (
        <>
          <label className="form-control">
            <span className="label-text">Passphrase (10 to 79 bytes)</span>
            <input
              type="password"
              className="input input-sm input-bordered"
              autoComplete="off"
              value={passphrase}
              onChange={(event) => setPassphrase(event.target.value)}
            />
          </label>
          <label className="form-control">
            <span className="label-text">Key length</span>
            <select
              className="select select-sm select-bordered"
              value={pbkeylen}
              onChange={(event) => setPbkeylen(Number(event.target.value) as 16 | 24 | 32)}
            >
              <option value={16}>AES-128</option>
              <option value={24}>AES-192</option>
              <option value={32}>AES-256</option>
            </select>
          </label>
        </>
      )}
      <label className="form-control">
        <span className="label-text">Receive latency (ms, 20 to 8000)</span>
        <input
          type="number"
          min={20}
          max={8000}
          required
          className="input input-sm input-bordered"
          value={latencyMs}
          onChange={(event) => setLatencyMs(Number(event.target.value))}
        />
      </label>
    </SettingsForm>
  );
}

const PROFILE_NUMBERS: ReadonlyArray<[keyof TranscodeProfile, string, number, number?]> = [
  ["crf", "CRF (0 lossless, 23 default)", 0, 51],
  ["gop", "GOP (frames)", 0],
  ["bframes", "B-frames", 0, 16],
  ["bitrate", "Bitrate (kb/s, 0 auto)", 0],
  ["maxBitrate", "Max bitrate (kb/s)", 0],
  ["width", "Width (0 source)", 0],
  ["height", "Height (0 source)", 0],
];

function Profiles(props: {
  current: Record<string, TranscodeProfile>;
  outputs: readonly ConfigOutput[];
  save: Saver;
}) {
  const [rows, setRows] = useState<readonly ProfileRow[]>(() => profileRows(props.current));
  const result = profilesToSave(rows, props.outputs, props.current);
  const edit = (index: number, next: Partial<ProfileRow>) =>
    setRows(rows.map((row, at) => (at === index ? { ...row, ...next } : row)));
  return (
    <SettingsForm
      id="settings-profiles"
      title="Transcode profiles"
      problem={"problem" in result ? result.problem : null}
      onSave={() => ("profiles" in result ? props.save({ transcodeProfiles: result.profiles }) : Promise.resolve(false))}
    >
      <ul className="space-y-3" aria-label="Profiles">
        {rows.map((row, index) => {
          const builtIn = BUILT_IN_PROFILES.includes(row.name);
          return (
            <li key={index} className="space-y-2 rounded bg-base-100 p-3">
              <div className="flex items-center gap-2">
                <input
                  className="input input-sm input-bordered font-mono"
                  aria-label="Profile name"
                  value={row.name}
                  readOnly={builtIn}
                  onChange={(event) => edit(index, { name: event.target.value })}
                />
                {builtIn ? (
                  <span className="badge badge-ghost badge-sm">built-in</span>
                ) : (
                  <button
                    type="button"
                    className="btn btn-ghost btn-xs text-error"
                    onClick={() => setRows(rows.filter((_, at) => at !== index))}
                  >
                    Remove
                  </button>
                )}
              </div>
              <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
                <label className="form-control">
                  <span className="label-text text-xs">Preset</span>
                  <select
                    className="select select-sm select-bordered"
                    value={row.profile.preset}
                    onChange={(event) =>
                      edit(index, { profile: { ...row.profile, preset: event.target.value } })
                    }
                  >
                    {X264_PRESETS.map((preset) => (
                      <option key={preset}>{preset}</option>
                    ))}
                  </select>
                </label>
                <label className="form-control">
                  <span className="label-text text-xs">Tune</span>
                  <select
                    className="select select-sm select-bordered"
                    value={row.profile.tune}
                    onChange={(event) =>
                      edit(index, { profile: { ...row.profile, tune: event.target.value } })
                    }
                  >
                    {X264_TUNES.map((tune) => (
                      <option key={tune} value={tune}>
                        {tune || "none"}
                      </option>
                    ))}
                  </select>
                </label>
                {PROFILE_NUMBERS.map(([field, label, min, max]) => (
                  <label key={field} className="form-control">
                    <span className="label-text text-xs">{label}</span>
                    <input
                      type="number"
                      min={min}
                      max={max}
                      required
                      className="input input-sm input-bordered"
                      value={row.profile[field]}
                      onChange={(event) =>
                        edit(index, {
                          profile: { ...row.profile, [field]: Number(event.target.value) },
                        })
                      }
                    />
                  </label>
                ))}
              </div>
            </li>
          );
        })}
      </ul>
      <button
        type="button"
        className="btn btn-sm self-start"
        onClick={() => setRows([...rows, { name: nextProfileName(rows), profile: NEW_PROFILE }])}
      >
        Add profile
      </button>
    </SettingsForm>
  );
}
