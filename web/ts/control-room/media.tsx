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
import { ConfirmDelete } from "./confirm.js";
import { formatBytes, formatPercent } from "./format.js";
import { When } from "./when-view.js";
import {
  formatDuration,
  ingestReadiness,
  mediaKey,
  recordingTimeline,
  searchMedia,
  sourceDraft,
  splitLibrary,
  storedForms,
} from "./media-model.js";
import type { TimelineEvent } from "./media-model.js";
import { PipelineForm } from "./pipeline-form.js";
import { Freshness, usePolled } from "./polled.js";
import { PlaceLink, useGo } from "./router.js";
import type { LiveSnapshot } from "./store.js";
import { KeyValue, Meter } from "../ui/kpi.js";
import { PageHeader } from "../ui/page-header.js";
import { Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";

const READINESS_TONE: Record<"ready" | "needsReencode" | "unknown" | "noVideo", Tone> = {
  ready: "ok",
  needsReencode: "warn",
  unknown: "neutral",
  noVideo: "bad",
};

const READINESS_LABEL = {
  ready: "safe to use live",
  needsReencode: "needs Live Optimized",
  unknown: "readiness unknown",
  noVideo: "no video",
} as const;

/** The media directory: recordings and source files, what each one is, and
 * whether it can feed a live pipeline as it is. */
export function Media({ live }: { live: LiveSnapshot }) {
  const library = usePolled(["media-files"], listMediaFiles);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const files = library.value?.files ?? null;
  // A rename selects the new name: the session join is by stored path, so a
  // renamed recording loses its session (and its timeline).
  const chosen =
    files?.find((file) => mediaKey(file) === selected || file.name === selected) ?? null;
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="Media library"
        title="Recordings and source files"
        subtitle="Upload, probe and reuse media as a looping live source."
      />
      <div className="flex flex-wrap items-center gap-3">
        <Freshness atMs={library.atMs} />
        <input
          type="search"
          className="input input-sm ml-auto w-64"
          placeholder="Search by file name"
          aria-label="Search media"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
        <Upload onUploaded={library.refresh} />
      </div>
      {files === null ? (
        <p className="text-sm text-base-content/70">Loading the media directory…</p>
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
                key={mediaKey(chosen)}
                file={chosen}
                files={files}
                onChanged={(name) => {
                  setSelected(name);
                  library.refresh();
                }}
              />
            ) : (
              <p className="rounded bg-base-200 p-4 text-sm text-base-content/70">
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
    <Panel
      id="media-library"
      title="Library"
      subtitle={props.files.length === props.total ? `${props.total} files` : `${props.files.length} of ${props.total} files`}
    >
      <div className="flex flex-col gap-3">
        {props.total === 0 ? (
          <p className="text-sm text-base-content/70">
            The media directory is empty. Upload a file, or record a pipeline.
          </p>
        ) : (
          <>
            <FileList title="Recordings" {...props} files={recordings} />
            <FileList title="Source files" {...props} files={sources} />
          </>
        )}
      </div>
    </Panel>
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
      <h4 className="mb-1 text-xs uppercase text-base-content/70">
        {props.title} · {props.files.length}
      </h4>
      {props.files.length === 0 ? (
        <p className="text-sm text-base-content/70">None.</p>
      ) : (
        <ul className="space-y-1" aria-label={props.title}>
          {props.files.map((file) => (
            <li key={mediaKey(file)}>
              <button
                type="button"
                className={`flex min-h-11 w-full flex-wrap items-center gap-2 rounded-field px-2 py-1 text-left text-sm hover:bg-base-100 md:min-h-8 ${
                  mediaKey(file) === props.selected ? "bg-base-100 font-semibold" : ""
                }`}
                aria-pressed={mediaKey(file) === props.selected}
                onClick={() => props.onSelect(mediaKey(file))}
              >
                <span className="min-w-0 flex-1 truncate">{file.name}</span>
                <ConversionBadge file={file} />
                <span className="tabular-nums text-base-content/70">
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
    return <StatusBadge tone="warn">converting</StatusBadge>;
  }
  if (file.conversionStatus === "ready") {
    return <StatusBadge tone="ok">MP4 ready</StatusBadge>;
  }
  if (file.conversionStatus === "failed") {
    return (
      <StatusBadge tone="bad" title={file.conversionError ?? undefined}>
        conversion failed
      </StatusBadge>
    );
  }
  return null;
}

function Asset(props: {
  file: MediaFile;
  files: readonly MediaFile[];
  onChanged: (name: string | null) => void;
}) {
  const { file } = props;
  const forms = storedForms(file);
  const ingests = file.ingestCount ?? 0;
  const analysis = useAnalysis(file.name);
  return (
    <Panel id="media-asset" title={file.name}>
      <div className="flex flex-col gap-3">
        <KeyValue
          rows={[
            ["Original", `${forms.original} · ${formatBytes(file.sourceSize ?? file.size)}`],
            !!forms.converted && ["Converted", `${forms.converted} · ${formatBytes(file.convertedSize)}`],
            ["Modified", <When atMs={new Date(file.modifiedAt).getTime()} show="both" />],
            ["Used by", ingests === 0 ? "no pipeline" : `${ingests} file ingest${ingests === 1 ? "" : "s"}`],
          ]}
        />
        {file.conversionStatus === "failed" && file.conversionError && (
          <p className="text-sm text-error">{file.conversionError}</p>
        )}
        <RecordingTimeline file={file} />
        <Analysis analysis={analysis} />
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
        {/* A remux can replace the .ts while the form is open: a new name
            starts a new form, so it never saves a deleted file. */}
        <UseAsSource key={file.name} file={file} files={props.files} analysis={analysis} />
        <Rename file={file} onRenamed={props.onChanged} />
        <Delete file={file} onDeleted={() => props.onChanged(null)} />
      </div>
    </Panel>
  );
}

/** The file's streams and keyframe spacing, probed once when it is selected. */
function useAnalysis(filename: string): MediaFileAnalysis | null | "failed" {
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
  return analysis;
}

function Analysis({ analysis }: { analysis: MediaFileAnalysis | null | "failed" }) {
  if (analysis === null) return <p className="text-sm text-base-content/70">Probing the file…</p>;
  if (analysis === "failed") {
    return <p className="text-sm text-base-content/70">The engine could not probe this file.</p>;
  }
  const readiness = ingestReadiness(analysis);
  return (
    <div className="rounded-box border border-base-content/10 bg-base-100 p-3 text-sm" aria-label="Ingest readiness">
      <div className="flex items-center gap-2">
        <b>Ingest readiness</b>
        <StatusBadge tone={READINESS_TONE[readiness.kind]} dot>
          {READINESS_LABEL[readiness.kind]}
        </StatusBadge>
      </div>
      <dl className="mt-2 grid grid-cols-[8rem_1fr] gap-1">
        <dt className="text-base-content/70">Video</dt>
        <dd>
          {analysis.videoCodec ?? "--"}
          {analysis.fps ? ` · ${analysis.fps.toFixed(2)} fps` : ""}
        </dd>
        <dt className="text-base-content/70">Duration</dt>
        <dd>{formatDuration(analysis.durationSec)}</dd>
        <dt className="text-base-content/70">Keyframes</dt>
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

const TIMELINE_DOT: Record<TimelineEvent["tone"], string> = {
  ok: "bg-success",
  info: "bg-info",
  error: "bg-error",
};

/** What happened to a recording, from its persisted session and remux. */
function RecordingTimeline({ file }: { file: MediaFile }) {
  const events = recordingTimeline(file);
  if (events === null) return null;
  return (
    <div className="rounded bg-base-100 p-3 text-sm">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <b>Recording timeline</b>
        {file.pipelineId && (
          <PlaceLink
            className="link text-xs"
            place={{ workspace: "operate", pipelineId: file.pipelineId }}
          >
            Open its pipeline
          </PlaceLink>
        )}
      </div>
      <ol className="mt-2 space-y-1" aria-label="Recording timeline">
        {events.map((event) => (
          <li key={event.label} className="grid grid-cols-[0.5rem_6rem_1fr] items-center gap-2">
            <span className={`h-2 w-2 rounded-full ${TIMELINE_DOT[event.tone]}`} />
            <When atMs={event.atMs} show="at" seconds className="text-base-content/70" />
            <span className={event.tone === "error" ? "text-error" : undefined}>
              {event.label}
            </span>
          </li>
        ))}
      </ol>
    </div>
  );
}

/** A new pipeline that plays this file, opened in Operate once it exists.
 * Offered once the probe is in: whether it needs Live Optimized is set when
 * the form opens. */
function UseAsSource(props: {
  file: MediaFile;
  files: readonly MediaFile[];
  analysis: MediaFileAnalysis | null | "failed";
}) {
  const [open, setOpen] = useState(false);
  const go = useGo();
  if (props.analysis === null) return null;
  const draft = sourceDraft(
    props.file,
    props.analysis === "failed" ? null : ingestReadiness(props.analysis),
  );
  if (draft === null) {
    return <p className="text-sm text-base-content/70">A file with no video cannot feed a pipeline.</p>;
  }
  if (!open) {
    return (
      <button type="button" className="btn btn-sm btn-primary" onClick={() => setOpen(true)}>
        Use as source
      </button>
    );
  }
  return (
    <PipelineForm
      stored={null}
      files={props.files}
      keyLocked={false}
      initial={draft}
      onSaved={(pipeline) => go({ workspace: "operate", pipelineId: pipeline.id })}
    />
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
        className="input input-sm min-w-0 flex-1"
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
  if ((props.file.ingestCount ?? 0) > 0) {
    return (
      <p className="text-xs text-base-content/70">
        A pipeline ingests this file: remove that ingest before deleting it.
      </p>
    );
  }
  return (
    <ConfirmDelete
      label={`Delete ${props.file.name}`}
      question="Delete it permanently?"
      onDelete={async () => {
        const deleted = (await deleteMediaFile(props.file.name)) !== null;
        if (deleted) props.onDeleted();
        return deleted;
      }}
    />
  );
}

function Storage({ live, files }: { live: LiveSnapshot; files: readonly MediaFile[] }) {
  const disk = live.metrics?.mediaDisk;
  const converting = files.filter((file) => file.conversionStatus === "converting").length;
  return (
    <Panel id="media-storage" title="Storage">
      <div className="flex flex-col gap-2">
        {disk ? (
          <>
            <div className="grid grid-cols-[1fr_3rem] items-center gap-2">
              <Meter value={disk.usedPercent === null || disk.usedPercent === undefined ? null : disk.usedPercent / 100} label="Media disk used" />
              <span className="text-right text-sm tabular-nums">
                {formatPercent(disk.usedPercent)}
              </span>
            </div>
            <KeyValue
              rows={[
                ["Used · free", `${formatBytes(disk.usedBytes)} · ${formatBytes(disk.freeBytes)}`],
                ["Directory", disk.mediaDir ?? "--"],
                ["Mount", disk.mountPoint ?? "--"],
              ]}
            />
          </>
        ) : (
          <p className="text-sm text-base-content/70">Waiting for the host sample…</p>
        )}
        <p className="text-sm">
          {files.length} files · {formatBytes(files.reduce((sum, file) => sum + file.size, 0))}
          {converting > 0 && ` · ${converting} converting`}
        </p>
      </div>
    </Panel>
  );
}
