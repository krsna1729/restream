import { lazy, Suspense, useEffect, useRef, useState } from "react";

import type { DashboardV2PipelineInputStatusActions } from "../dashboard-v2-loader.js";
import type { PipelineOperateInputStatusModel } from "../../features/pipeline-operate-view-model.js";

import { StatusBadge, INPUT_AUDIO_TRACK_PREVIEW_LIMIT } from "./common.js";

const PipelineInputsPanel = lazy(async () => {
  const module = await import("../../features/pipeline-inputs-panel.js");
  return { default: module.PipelineInputsPanel };
});

export function DashboardV2PipelineInputStatus({
  actions,
  model,
}: {
  actions: DashboardV2PipelineInputStatusActions;
  model: PipelineOperateInputStatusModel;
}): React.JSX.Element {
  const previewContainerRef = useRef<HTMLDivElement>(null);
  const [audioExpanded, setAudioExpanded] = useState(false);
  const [audioQuery, setAudioQuery] = useState("");
  const normalizedAudioQuery = audioQuery.trim().toLowerCase();
  const audioTrackOverflow =
    model.audioTracks.length > INPUT_AUDIO_TRACK_PREVIEW_LIMIT;
  const filteredAudioTracks = normalizedAudioQuery
    ? model.audioTracks.filter((track) =>
        [
          `track ${track.index + 1}`,
          track.label,
          track.identity,
          track.codec,
          track.sampleRate,
          track.channels,
          track.profile,
        ]
          .join(" ")
          .toLowerCase()
          .includes(normalizedAudioQuery),
      )
    : model.audioTracks;
  const visibleAudioTracks = normalizedAudioQuery
    ? filteredAudioTracks
    : audioExpanded
      ? model.audioTracks
      : model.audioTracks.slice(0, INPUT_AUDIO_TRACK_PREVIEW_LIMIT);
  const showAudioSearch = audioTrackOverflow || normalizedAudioQuery !== "";
  const audioSummaryText = normalizedAudioQuery
    ? `${filteredAudioTracks.length}/${model.audioTracks.length} audio tracks match "${audioQuery.trim()}"`
    : `Showing ${visibleAudioTracks.length} of ${model.audioTracks.length} audio tracks`;

  useEffect(() => {
    const container = previewContainerRef.current;
    if (!container || !model.previewEnabled) return;
    actions.mountPreview(model.id, container);
    return () => actions.clearPreview(container);
  }, [
    actions,
    model.id,
    model.previewEnabled,
    model.previewKeyAssigned,
  ]);

  useEffect(() => {
    setAudioExpanded(false);
    setAudioQuery("");
  }, [model.id]);

  return (
    <section
      aria-labelledby="dashboard-v2-input-status-title"
      className="mb-3 text-left"
    >
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <h2
            className="text-base-content/70 text-xs font-semibold uppercase tracking-wide"
            id="dashboard-v2-input-status-title"
          >
            Input and preview
          </h2>
          <p className="text-base-content/55 mt-1 text-xs tabular-nums">
            {model.uptimeLabel}
          </p>
        </div>
        <StatusBadge status={model.status} />
      </div>
      <div className="border-base-content/10 divide-base-content/10 mt-3 grid border-y sm:grid-cols-3 sm:divide-x">
        <div className="border-base-content/10 px-1 py-2.5 sm:px-3">
          <div className="text-base-content/55 text-[0.7rem] font-semibold uppercase">
            Publisher
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-2">
            <span className="text-sm font-medium">{model.publisherLabel}</span>
            {model.publisherHealth ? (
              <StatusBadge status={model.publisherHealth} />
            ) : null}
          </div>
          <p
            className="text-base-content/55 mt-1 truncate text-xs"
            title={model.publisherDetail}
          >
            {model.publisherDetail}
          </p>
        </div>
        <div className="border-base-content/10 border-t px-1 py-2.5 sm:border-t-0 sm:px-3">
          <div className="text-base-content/55 text-[0.7rem] font-semibold uppercase">
            Browser preview
          </div>
          <div className="mt-1">
            <StatusBadge status={model.preview} />
          </div>
          <p className="text-base-content/55 mt-1 text-xs tabular-nums">
            {model.previewDetail}
          </p>
        </div>
        <div className="border-base-content/10 border-t px-1 py-2.5 sm:border-t-0 sm:px-3">
          <div className="text-base-content/55 text-[0.7rem] font-semibold uppercase">
            Media
          </div>
          <p className="mt-1 text-sm font-medium">{model.videoLabel}</p>
          <p className="text-base-content/55 mt-1 text-xs">
            {model.audioLabel}
          </p>
        </div>
      </div>
      {model.previewEnabled ? (
        <div className="mt-3">
          <h3 className="text-base-content/60 mb-1 text-[0.7rem] font-semibold uppercase tracking-wide">
            Preview player
          </h3>
          <div
            data-role="dashboard-v2-input-preview"
            ref={previewContainerRef}
          />
        </div>
      ) : null}
      {model.unexpectedReadersLabel ? (
        <p className="text-error mt-2 text-xs font-medium">
          {model.unexpectedReadersLabel}
        </p>
      ) : null}
      {model.metricGroups.map((group) => (
        <div className="mt-3" key={group.key}>
          <h3 className="text-base-content/60 text-[0.7rem] font-semibold uppercase tracking-wide">
            {group.label}
          </h3>
          <dl className="border-base-content/10 mt-1 grid grid-cols-2 overflow-hidden rounded-md border sm:grid-cols-4">
            {group.metrics.map((metric, index) => (
              <div
                className={`${index % 2 === 1 ? "border-base-content/10 border-l" : ""} ${index > 1 ? "border-base-content/10 border-t sm:border-t-0" : ""} ${index > 0 ? "sm:border-base-content/10 sm:border-l" : ""} px-3 py-2`}
                key={metric.key}
              >
                <dt className="text-base-content/55 text-[0.7rem]">
                  {metric.label}
                </dt>
                <dd className="mt-1 text-sm font-medium tabular-nums">
                  {metric.value}
                </dd>
              </div>
            ))}
          </dl>
        </div>
      ))}
      <Suspense
        fallback={
          <div className="dashboard-empty mt-3" role="status">
            Loading inputs...
          </div>
        }
      >
        <PipelineInputsPanel actions={actions} pipelineId={model.id} />
      </Suspense>
      <div className="mt-3">
        <div className="flex flex-wrap items-end justify-between gap-2">
          <h3 className="text-base-content/60 text-[0.7rem] font-semibold uppercase tracking-wide">
            Audio
          </h3>
          {showAudioSearch ? (
            <div className="flex min-w-0 flex-wrap items-center gap-2">
              <label className="input input-bordered input-xs flex min-h-8 min-w-48 items-center gap-2">
                <span className="text-base-content/55 text-[0.65rem] font-semibold uppercase">
                  Find
                </span>
                <input
                  aria-label="Search audio tracks"
                  className="min-w-0 grow"
                  onChange={(event) => setAudioQuery(event.currentTarget.value)}
                  placeholder="track, codec, language"
                  type="search"
                  value={audioQuery}
                />
              </label>
              {normalizedAudioQuery ? (
                <button
                  aria-label="Clear audio track search"
                  className="btn btn-xs btn-ghost"
                  onClick={() => setAudioQuery("")}
                  type="button"
                >
                  Clear search
                </button>
              ) : null}
            </div>
          ) : null}
        </div>
        {model.audioTracks.length ? (
          <div className="border-base-content/10 divide-base-content/10 mt-1 divide-y border-y">
            {visibleAudioTracks.length ? (
              visibleAudioTracks.map((track) => (
              <div
                className="border-base-content/10 grid gap-2 px-1 py-2.5 sm:grid-cols-[minmax(0,1.2fr)_repeat(4,minmax(0,.7fr))] sm:px-3"
                key={track.key}
              >
                <div className="min-w-0">
                  <div className="text-base-content/55 text-[0.7rem]">
                    Track {track.index + 1}
                  </div>
                  {track.editing ? (
                    <div className="mt-1 flex flex-wrap gap-1">
                      <input
                        aria-label="Audio track name"
                        autoFocus
                        className="input input-bordered input-xs min-w-32 flex-1"
                        defaultValue={track.draft}
                        onChange={(event) =>
                          actions.updateAudioTrackDraft(
                            model.id,
                            track.key,
                            event.currentTarget.value,
                          )
                        }
                        onKeyDown={(event) => {
                          if (event.key === "Enter")
                            actions.saveAudioTrack(model.id, track.key);
                          if (event.key === "Escape")
                            actions.cancelAudioTrackEdit(model.id, track.key);
                        }}
                      />
                      <button
                        aria-label={`Save audio track ${track.label} for ${model.name}`}
                        className="btn btn-xs btn-accent"
                        onClick={() =>
                          actions.saveAudioTrack(model.id, track.key)
                        }
                        type="button"
                      >
                        Save
                      </button>
                      <button
                        aria-label={`Cancel audio track edit for ${track.label}`}
                        className="btn btn-xs btn-ghost"
                        onClick={() =>
                          actions.cancelAudioTrackEdit(model.id, track.key)
                        }
                        type="button"
                      >
                        Cancel
                      </button>
                    </div>
                  ) : (
                    <div className="mt-1 flex items-center gap-1">
                      <div className="min-w-0 flex-1">
                        <div className="truncate text-sm font-medium">
                          {track.label}
                        </div>
                        <div className="text-base-content/55 truncate text-xs">
                          {track.identity}
                        </div>
                      </div>
                      <button
                        aria-label={`Rename ${track.label}`}
                        className="btn btn-xs btn-ghost"
                        onClick={() =>
                          actions.editAudioTrack(model.id, track.key)
                        }
                        type="button"
                      >
                        Rename
                      </button>
                    </div>
                  )}
                </div>
                {[
                  ["Codec", track.codec],
                  ["Freq", track.sampleRate],
                  ["Channels", track.channels],
                  ["Profile", track.profile],
                ].map(([label, value]) => (
                  <div className="min-w-0" key={label}>
                    <div className="text-base-content/55 text-[0.7rem]">
                      {label}
                    </div>
                    <div className="mt-1 truncate text-sm">{value}</div>
                  </div>
                ))}
              </div>
              ))
            ) : (
              <div className="px-1 py-3 text-sm text-base-content/60 sm:px-3">
                No audio tracks match "{audioQuery.trim()}". Clear search to
                show all.
              </div>
            )}
            {audioTrackOverflow || normalizedAudioQuery ? (
              <div className="flex items-center justify-between gap-2 px-1 py-2.5 sm:px-3">
                <p
                  aria-live="polite"
                  className="text-base-content/55 text-xs"
                  role="status"
                >
                  {audioSummaryText}
                </p>
                {normalizedAudioQuery ? null : (
                  <button
                    aria-label={
                      audioExpanded
                        ? "Show fewer audio tracks"
                        : `Show all ${model.audioTracks.length} audio tracks`
                    }
                    className="btn btn-xs btn-outline"
                    onClick={() => setAudioExpanded((expanded) => !expanded)}
                    type="button"
                  >
                    {audioExpanded
                      ? "Show fewer"
                      : `Show all ${model.audioTracks.length}`}
                  </button>
                )}
              </div>
            ) : null}
          </div>
        ) : (
          <p className="text-base-content/55 mt-1 text-sm">No tracks</p>
        )}
      </div>
      {model.fileSource ? (
        <div className="border-base-content/10 mt-4 border-t pt-3">
          <div className="text-base-content/55 text-[0.7rem] font-semibold uppercase">
            Source file
          </div>
          <p
            className="mt-1 truncate text-sm font-medium"
            title={model.fileSource.filename}
          >
            {model.fileSource.filename}
          </p>
          {model.fileSource.warning ? (
            <div className="alert alert-warning mt-3 py-2 text-sm">
              {model.fileSource.warning}
            </div>
          ) : null}
          <dl className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-3">
            {model.fileSource.details.map((detail) => (
              <div
                className="bg-base-200/45 rounded-md px-3 py-2"
                key={detail.key}
              >
                <dt className="text-base-content/55 text-[0.7rem]">
                  {detail.label}
                </dt>
                <dd className="mt-1 text-sm font-medium tabular-nums">
                  {detail.value}
                </dd>
              </div>
            ))}
          </dl>
        </div>
      ) : null}
    </section>
  );
}
