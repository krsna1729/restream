import { useEffect, useRef, useState } from "react";

import {
  deleteMediaFile,
  getMediaFileAnalysis,
  listMediaFiles,
  mediaFileUrl,
  renameMediaFile,
  uploadMediaFile,
} from "../core/api.js";
import type { MediaFile, MediaFileAnalysis } from "../core/api-types.js";
import { formatBytes, formatPercent } from "./format.js";
import {
  formatDuration,
  ingestReadiness,
  searchMedia,
  splitLibrary,
  storedForms,
} from "./media-model.js";
import { Freshness, usePolled } from "./polled.js";
import type { LiveSnapshot } from "./store.js";

const READINESS_BADGE = {
  ready: "badge-success",
  needsReencode: "badge-warning",
  noVideo: "badge-error",
} as const;

const READINESS_LABEL = {
  ready: "safe to use live",
  needsReencode: "needs Live Optimized",
  noVideo: "no video",
} as const;

/** The media directory: recordings and source files, what each one is, and
 * whether it can feed a live pipeline as it is. */
export function Media({ live }: { live: LiveSnapshot }) {
  const library = usePolled(listMediaFiles);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const files = library.value?.files ?? null;
  const chosen = files?.find((file) => file.name === selected) ?? null;
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3">
        <h2 className="text-xl font-semibold">Media</h2>
        <Freshness atMs={library.atMs} />
        <input
          type="search"
          className="input input-sm input-bordered ml-auto w-64"
          placeholder="Search by file name"
          aria-label="Search media"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
        <Upload onUploaded={library.refresh} />
      </div>
      {files === null ? (
        <p className="opacity-70">Loading the media directory…</p>
      ) : (
        <div className="grid gap-4 lg:grid-cols-[1fr_22rem]">
          <Library
            files={searchMedia(files, query)}
            total={files.length}
            selected={selected}
            onSelect={setSelected}
          />
          <div className="space-y-4">
            {chosen ? (
              <Asset
                key={chosen.name}
                file={chosen}
                onChanged={(name) => {
                  setSelected(name);
                  library.refresh();
                }}
              />
            ) : (
              <p className="rounded bg-base-200 p-4 text-sm opacity-70">
                Select a file to see its streams and whether it can feed a live
                pipeline.
              </p>
            )}
            <Storage live={live} files={files} />
          </div>
        </div>
      )}
    </div>
  );
}

function Upload({ onUploaded }: { onUploaded: () => void }) {
  const input = useRef<HTMLInputElement>(null);
  return (
    <>
      <button
        type="button"
        className="btn btn-sm btn-primary"
        onClick={() => input.current?.click()}
      >
        Upload media
      </button>
      <input
        ref={input}
        type="file"
        className="hidden"
        aria-label="Media file to upload"
        onChange={async (event) => {
          const file = event.target.files?.[0];
          event.target.value = "";
          if (file && (await uploadMediaFile(file)) !== null) onUploaded();
        }}
      />
    </>
  );
}

function Library(props: {
  files: readonly MediaFile[];
  total: number;
  selected: string | null;
  onSelect: (name: string) => void;
}) {
  const { recordings, sources } = splitLibrary(props.files);
  return (
    <section className="card bg-base-200" aria-labelledby="media-library">
      <div className="card-body gap-3 p-4">
        <h3 id="media-library" className="card-title text-base">
          Library
          <span className="text-sm font-normal opacity-70">
            {props.files.length === props.total
              ? `${props.total} files`
              : `${props.files.length} of ${props.total} files`}
          </span>
        </h3>
        {props.total === 0 ? (
          <p className="text-sm opacity-70">
            The media directory is empty. Upload a file, or record a pipeline.
          </p>
        ) : (
          <>
            <FileList title="Recordings" {...props} files={recordings} />
            <FileList title="Source files" {...props} files={sources} />
          </>
        )}
      </div>
    </section>
  );
}

function FileList(props: {
  title: string;
  files: readonly MediaFile[];
  selected: string | null;
  onSelect: (name: string) => void;
}) {
  return (
    <div>
      <h4 className="mb-1 text-xs uppercase opacity-70">
        {props.title} · {props.files.length}
      </h4>
      {props.files.length === 0 ? (
        <p className="text-sm opacity-70">None.</p>
      ) : (
        <ul className="space-y-1" aria-label={props.title}>
          {props.files.map((file) => (
            <li key={file.name}>
              <button
                type="button"
                className={`flex w-full flex-wrap items-center gap-2 rounded px-2 py-1 text-left text-sm hover:bg-base-100 ${
                  file.name === props.selected ? "bg-base-100 font-semibold" : ""
                }`}
                aria-pressed={file.name === props.selected}
                onClick={() => props.onSelect(file.name)}
              >
                <span className="min-w-0 flex-1 truncate">{file.name}</span>
                <ConversionBadge file={file} />
                <span className="tabular-nums opacity-70">
                  {formatBytes(file.sourceSize ?? file.size)}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ConversionBadge({ file }: { file: MediaFile }) {
  if (file.conversionStatus === "converting") {
    return <span className="badge badge-sm badge-warning">converting</span>;
  }
  if (file.conversionStatus === "ready") {
    return <span className="badge badge-sm badge-success">MP4 ready</span>;
  }
  if (file.conversionStatus === "failed") {
    return (
      <span className="badge badge-sm badge-error" title={file.conversionError ?? undefined}>
        conversion failed
      </span>
    );
  }
  return null;
}

function Asset(props: { file: MediaFile; onChanged: (name: string | null) => void }) {
  const { file } = props;
  const forms = storedForms(file);
  const ingests = file.ingestCount ?? 0;
  return (
    <section className="card bg-base-200" aria-labelledby="media-asset">
      <div className="card-body gap-3 p-4">
        <h3 id="media-asset" className="card-title break-all text-base">
          {file.name}
        </h3>
        <dl className="grid grid-cols-[8rem_1fr] gap-1 text-sm">
          <dt className="opacity-70">Original</dt>
          <dd className="break-all">
            {forms.original} · {formatBytes(file.sourceSize ?? file.size)}
          </dd>
          {forms.converted && (
            <>
              <dt className="opacity-70">Converted</dt>
              <dd className="break-all">
                {forms.converted} · {formatBytes(file.convertedSize)}
              </dd>
            </>
          )}
          <dt className="opacity-70">Modified</dt>
          <dd>{new Date(file.modifiedAt).toLocaleString()}</dd>
          <dt className="opacity-70">Used by</dt>
          <dd>{ingests === 0 ? "no pipeline" : `${ingests} file ingest${ingests === 1 ? "" : "s"}`}</dd>
        </dl>
        {file.conversionStatus === "failed" && file.conversionError && (
          <p className="text-sm text-error">{file.conversionError}</p>
        )}
        <Analysis filename={file.name} />
        <div className="flex flex-wrap gap-2">
          {file.playName && (
            <a
              className="btn btn-sm"
              href={mediaFileUrl(file.playName)}
              target="_blank"
              rel="noopener noreferrer"
            >
              Play
            </a>
          )}
          {forms.converted && (
            <a className="btn btn-sm" href={mediaFileUrl(forms.converted)} download={forms.converted}>
              Download MP4
            </a>
          )}
          <a className="btn btn-sm" href={mediaFileUrl(forms.original)} download={forms.original}>
            Download original
          </a>
        </div>
        <Rename file={file} onRenamed={props.onChanged} />
        <Delete file={file} onDeleted={() => props.onChanged(null)} />
      </div>
    </section>
  );
}

/** The file's streams and keyframe spacing, probed once when it is selected. */
function Analysis({ filename }: { filename: string }) {
  const [analysis, setAnalysis] = useState<MediaFileAnalysis | null | "failed">(null);
  useEffect(() => {
    let alive = true;
    void getMediaFileAnalysis(filename).then((result) => {
      if (alive) setAnalysis(result ?? "failed");
    });
    return () => {
      alive = false;
    };
  }, [filename]);
  if (analysis === null) return <p className="text-sm opacity-70">Probing the file…</p>;
  if (analysis === "failed") {
    return <p className="text-sm opacity-70">The engine could not probe this file.</p>;
  }
  const readiness = ingestReadiness(analysis);
  return (
    <div className="rounded bg-base-100 p-3 text-sm" aria-label="Ingest readiness">
      <div className="flex items-center gap-2">
        <b>Ingest readiness</b>
        <span className={`badge badge-sm ${READINESS_BADGE[readiness.kind]}`}>
          {READINESS_LABEL[readiness.kind]}
        </span>
      </div>
      <dl className="mt-2 grid grid-cols-[8rem_1fr] gap-1">
        <dt className="opacity-70">Video</dt>
        <dd>
          {analysis.videoCodec ?? "--"}
          {analysis.fps ? ` · ${analysis.fps.toFixed(2)} fps` : ""}
        </dd>
        <dt className="opacity-70">Duration</dt>
        <dd>{formatDuration(analysis.durationSec)}</dd>
        <dt className="opacity-70">Keyframes</dt>
        <dd>
          {analysis.keyframeCount}
          {analysis.averageKeyframeIntervalSec
            ? ` · every ${analysis.averageKeyframeIntervalSec.toFixed(1)} s on average`
            : ""}
        </dd>
      </dl>
      <p className="mt-2">{readiness.detail}</p>
    </div>
  );
}

function Rename(props: { file: MediaFile; onRenamed: (name: string) => void }) {
  const [name, setName] = useState(props.file.name);
  const changed = name.trim() !== "" && name.trim() !== props.file.name;
  return (
    <form
      className="flex gap-2"
      onSubmit={async (event) => {
        event.preventDefault();
        if (!changed) return;
        const result = await renameMediaFile(props.file.name, name.trim());
        if (result !== null) props.onRenamed(result.name);
      }}
    >
      <input
        className="input input-sm input-bordered min-w-0 flex-1"
        aria-label="New file name"
        title="The file extension must stay the same"
        value={name}
        onChange={(event) => setName(event.target.value)}
      />
      <button type="submit" className="btn btn-sm" disabled={!changed}>
        Rename
      </button>
    </form>
  );
}

/** Delete asks once more before it acts; a file a pipeline ingests cannot be
 * deleted until that ingest is removed. */
function Delete(props: { file: MediaFile; onDeleted: () => void }) {
  const [armed, setArmed] = useState(false);
  const inUse = (props.file.ingestCount ?? 0) > 0;
  if (inUse) {
    return (
      <p className="text-xs opacity-70">
        A pipeline ingests this file: remove that ingest before deleting it.
      </p>
    );
  }
  return armed ? (
    <div className="flex items-center gap-2 text-sm">
      <span>Delete it permanently?</span>
      <button
        type="button"
        className="btn btn-sm btn-error"
        onClick={async () => {
          if ((await deleteMediaFile(props.file.name)) !== null) props.onDeleted();
          else setArmed(false);
        }}
      >
        Delete
      </button>
      <button type="button" className="btn btn-sm btn-ghost" onClick={() => setArmed(false)}>
        Keep
      </button>
    </div>
  ) : (
    <button
      type="button"
      className="btn btn-sm btn-outline btn-error self-start"
      onClick={() => setArmed(true)}
    >
      Delete…
    </button>
  );
}

function Storage({ live, files }: { live: LiveSnapshot; files: readonly MediaFile[] }) {
  const disk = live.metrics?.mediaDisk;
  const converting = files.filter((file) => file.conversionStatus === "converting").length;
  return (
    <section className="card bg-base-200" aria-labelledby="media-storage">
      <div className="card-body gap-2 p-4">
        <h3 id="media-storage" className="card-title text-base">
          Storage
        </h3>
        {disk ? (
          <>
            <div className="grid grid-cols-[1fr_3rem] items-center gap-2">
              <progress
                className="progress w-full"
                value={disk.usedPercent ?? 0}
                max={100}
                aria-label="Media disk used"
              />
              <span className="text-right text-sm tabular-nums">
                {formatPercent(disk.usedPercent)}
              </span>
            </div>
            <dl className="grid grid-cols-[8rem_1fr] gap-1 text-sm">
              <dt className="opacity-70">Used · free</dt>
              <dd>
                {formatBytes(disk.usedBytes)} · {formatBytes(disk.freeBytes)}
              </dd>
              <dt className="opacity-70">Directory</dt>
              <dd className="break-all">{disk.mediaDir ?? "--"}</dd>
              <dt className="opacity-70">Mount</dt>
              <dd className="break-all">{disk.mountPoint ?? "--"}</dd>
            </dl>
          </>
        ) : (
          <p className="text-sm opacity-70">Waiting for the host sample…</p>
        )}
        <p className="text-sm">
          {files.length} files · {formatBytes(files.reduce((sum, file) => sum + file.size, 0))}
          {converting > 0 && ` · ${converting} converting`}
        </p>
      </div>
    </section>
  );
}
