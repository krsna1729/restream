export function controlRoomShellHtml(): string {
  return `
        <div class="space-y-5">
            <section class="border-base-content/10 from-base-200 via-base-200 to-base-100 rounded-2xl border bg-gradient-to-br p-4 shadow-sm">
                <div class="flex flex-wrap items-center justify-between gap-3">
                    <div>
                        <h1 class="text-lg font-semibold">Control Room</h1>
                        <p id="control-room-route-summary" class="text-base-content/60 mt-1 text-sm" role="status" aria-live="polite"></p>
                    </div>
                    <div class="flex flex-wrap items-center gap-2">
                        <button type="button" class="btn btn-sm btn-outline" data-action="control-room-toggle-playback-all">Play All</button>
                        <button type="button" class="btn btn-sm btn-outline" data-action="control-room-toggle-mute-all">Mute All</button>
                        <button type="button" id="control-room-reset-btn" class="btn btn-sm btn-outline" aria-label="Reset monitor wall">Reset</button>
                    </div>
                </div>
                <div class="mt-4 border-t border-base-content/10 pt-3">
                    <h2 id="control-room-controls-title" class="text-sm font-semibold tracking-[0.01em]">Monitor controls</h2>
                    <p class="text-base-content/60 mt-1 text-xs">Choose a pipeline, narrow the wall, and control all visible previews.</p>
                </div>
                <div class="mt-3 flex flex-wrap items-end gap-3" aria-labelledby="control-room-controls-title">
                    <label class="min-w-[18rem] flex-1 text-sm">
                        <span class="text-base-content/70 mb-1 block text-xs font-semibold uppercase">Pipeline</span>
                        <select id="control-room-pipeline-select" class="select select-sm w-full" aria-label="Filter monitor by pipeline"></select>
                    </label>
                    <label class="min-w-[12rem] flex-1 text-sm">
                        <span class="text-base-content/70 mb-1 block text-xs font-semibold uppercase">Search Outputs</span>
                        <input type="text" id="control-room-search-input" aria-label="Search monitor outputs" placeholder="Search outputs..." class="input input-sm input-bordered w-full" />
                    </label>
                    <div class="flex items-center gap-2">
                        <button type="button" class="btn btn-sm btn-outline" data-action="control-room-prev-page" aria-label="Previous monitor page">Prev</button>
                        <span id="control-room-page-label" class="text-base-content/70 min-w-[6rem] text-center text-sm">Page 1 / 1</span>
                        <button type="button" class="btn btn-sm btn-outline" data-action="control-room-next-page" aria-label="Next monitor page">Next</button>
                    </div>
                </div>
                <div class="text-base-content/60 mt-2 text-xs" id="control-room-summary" role="status" aria-live="polite"></div>
            </section>
            <section aria-labelledby="control-room-previews-title" class="space-y-3">
                <div class="flex flex-wrap items-end justify-between gap-3">
                    <div>
                        <h2 id="control-room-previews-title" class="text-sm font-semibold tracking-[0.01em]">Monitor previews</h2>
                        <p class="text-base-content/60 mt-1 text-xs">Local HLS first, followed by configured output monitors.</p>
                    </div>
                </div>
                <div id="control-room-grid" class="grid gap-4 sm:grid-cols-2 xl:grid-cols-4"></div>
            </section>
        </div>`;
}
