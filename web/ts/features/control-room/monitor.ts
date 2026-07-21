import { escapeHtml } from "../../core/utils.js";
import { getYoutubeMonitoringStatus } from "../../core/api.js";
import type { YoutubeMonitoringStatus } from "../../core/api-types.js";
import {
  clearManagedHlsPlayer,
  getManagedHlsController,
  renderManagedHlsPlayer,
} from "../hls-player.js";
import type {
  ControlRoomMediaController,
  MonitoringEmbedKind,
  YouTubeApiNamespace,
  YouTubePlayerApi,
} from "./types.js";

declare global {
  interface Window {
    YT?: YouTubeApiNamespace;
    onYouTubeIframeAPIReady?: (() => void) | undefined;
  }
}

const CONTROL_ROOM_PLAYER_HEIGHT_CLASS = "h-[11rem]";
const CONTROL_ROOM_MONITOR_FRAME_CLASS =
  "relative isolate w-full overflow-hidden rounded-[0.9rem] bg-neutral-950";
const CONTROL_ROOM_MONITOR_BUTTON_CLASS =
  "btn btn-xs border border-white/15 bg-black/55 text-white shadow-sm backdrop-blur hover:border-white/25 hover:bg-black/75";
const YOUTUBE_MONITORING_STATUS_TTL_MS = 60_000;

import {
  controlRoomMuteIntent,
  controlRoomPlaybackIntent,
} from "./index.js";

const controlRoomCardWarnings = new Map<string, string>();
const controlRoomMediaControllers = new WeakMap<
  HTMLElement,
  ControlRoomMediaController
>();
const controlRoomLoadedEmbedCards = new Set<string>();
let youtubeIframeApiPromise: Promise<YouTubeApiNamespace> | null = null;
const youtubeMonitoringStatusCache = new Map<
  string,
  {
    expiresAt: number;
    data: YoutubeMonitoringStatus | null;
    pending?: Promise<YoutubeMonitoringStatus | null>;
  }
>();

function isYouTubeMonitoringUrl(url: string): boolean {
  try {
    const parsed = new URL(url);
    const host = parsed.hostname.replace(/^www\./i, "").toLowerCase();
    return host === "youtu.be" || host.endsWith("youtube.com");
  } catch {
    return false;
  }
}

function isHlsMonitoringUrl(url: string): boolean {
  return /\.m3u8(?:$|[?#])/i.test(url);
}

function isDirectVideoMonitoringUrl(url: string): boolean {
  return /\.(mp4|m4v|webm|ogg|mov)(?:$|[?#])/i.test(url);
}

function applyYouTubeMonitoringParams(embed: URL): string {
  embed.searchParams.set("autoplay", "1");
  embed.searchParams.set("mute", "1");
  embed.searchParams.set("playsinline", "1");
  embed.searchParams.set("controls", "0");
  embed.searchParams.set("enablejsapi", "1");
  embed.searchParams.set("modestbranding", "1");
  embed.searchParams.set("disablekb", "1");
  embed.searchParams.set("fs", "0");
  embed.searchParams.set("iv_load_policy", "3");
  embed.searchParams.set("rel", "0");
  embed.searchParams.set("origin", window.location.origin);
  return embed.toString();
}

function toEmbeddableMonitoringUrl(url: string): string {
  try {
    const parsed = new URL(url);
    const host = parsed.hostname.replace(/^www\./i, "").toLowerCase();
    const pathParts = parsed.pathname.split("/").filter(Boolean);
    if (host === "youtu.be" && pathParts[0]) {
      const embed = new URL(
        `https://www.youtube-nocookie.com/embed/${encodeURIComponent(pathParts[0])}`,
      );
      return applyYouTubeMonitoringParams(embed);
    }
    if (host.endsWith("youtube.com")) {
      const videoId =
        parsed.searchParams.get("v") || pathParts[1] || pathParts[0] || "";
      if (parsed.pathname === "/watch" && videoId) {
        const embed = new URL(
          `https://www.youtube-nocookie.com/embed/${encodeURIComponent(videoId)}`,
        );
        return applyYouTubeMonitoringParams(embed);
      }
      if (
        (pathParts[0] === "live" ||
          pathParts[0] === "shorts" ||
          pathParts[0] === "embed") &&
        pathParts[1]
      ) {
        const embed = new URL(
          `https://www.youtube-nocookie.com/embed/${encodeURIComponent(pathParts[1])}`,
        );
        return applyYouTubeMonitoringParams(embed);
      }
    }
    return url;
  } catch {
    return url;
  }
}

function toOpenableMonitoringUrl(url: string | null): string | null {
  if (!url) return null;
  try {
    const parsed = new URL(url);
    const host = parsed.hostname.replace(/^www\./i, "").toLowerCase();
    const pathParts = parsed.pathname.split("/").filter(Boolean);
    if (host === "youtu.be" && pathParts[0]) {
      return `https://www.youtube.com/live/${encodeURIComponent(pathParts[0])}?feature=share`;
    }
    if (host.endsWith("youtube.com")) {
      const videoId =
        parsed.searchParams.get("v") || pathParts[1] || pathParts[0] || "";
      if (videoId) {
        return `https://www.youtube.com/live/${encodeURIComponent(videoId)}?feature=share`;
      }
    }
    return url;
  } catch {
    return url;
  }
}

function controlRoomCardTitle(element: Element): string {
  return (
    element.closest<HTMLElement>("article")?.dataset.cardTitle?.trim() || ""
  );
}

function monitorPreviewActionLabel(element: Element, label: string): string {
  const title = controlRoomCardTitle(element);
  if (!title) return label;
  return label.toLowerCase().includes("preview")
    ? `${label} for ${title}`
    : `${label} preview for ${title}`;
}

function buildMonitorPopupFeatures(): string {
  const width = Math.min(
    1600,
    Math.max(960, Math.floor(window.screen.availWidth * 0.86)),
  );
  const height = Math.min(
    1100,
    Math.max(720, Math.floor(window.screen.availHeight * 0.9)),
  );
  const left = Math.max(0, Math.floor((window.screen.availWidth - width) / 2));
  const top = Math.max(0, Math.floor((window.screen.availHeight - height) / 2));
  return `noopener,width=${width},height=${height},left=${left},top=${top}`;
}

function openSizedPopup(url: string): Window | null {
  return window.open(url, "_blank", buildMonitorPopupFeatures());
}

function openMonitorUrl(url: string, _title: string): void {
  openSizedPopup(url);
}

function getYouTubeMonitoringWarning(
  status: YoutubeMonitoringStatus | null,
): string | null {
  if (!status) return null;
  if (status.live_now) return null;
  return status.live_content || status.upcoming
    ? "This YouTube monitor is not live right now. Update the monitoring URL if the stream moved or has ended."
    : "This YouTube monitor resolves to a regular video, not a live stream. Update the monitoring URL to the active live share URL.";
}

async function fetchYouTubeMonitoringStatus(
  monitoringUrl: string,
): Promise<YoutubeMonitoringStatus | null> {
  const now = Date.now();
  const cached = youtubeMonitoringStatusCache.get(monitoringUrl);
  if (cached && cached.expiresAt > now) return cached.data;
  if (cached?.pending) return cached.pending;

  const pending = getYoutubeMonitoringStatus(monitoringUrl).then((data) => {
    youtubeMonitoringStatusCache.set(monitoringUrl, {
      expiresAt: Date.now() + YOUTUBE_MONITORING_STATUS_TTL_MS,
      data,
    });
    return data;
  });

  youtubeMonitoringStatusCache.set(monitoringUrl, {
    expiresAt: 0,
    data: cached?.data || null,
    pending,
  });
  return pending;
}

function listMountedMediaControllers(
  scope: ParentNode = document,
): Array<{ shell: HTMLElement; controller: ControlRoomMediaController }> {
  const result: Array<{
    shell: HTMLElement;
    controller: ControlRoomMediaController;
  }> = [];
  const shells = scope.querySelectorAll<HTMLElement>(
    '[data-role="control-room-player-shell"]',
  );
  shells.forEach((shell) => {
    const controller = controlRoomMediaControllers.get(shell);
    if (controller) result.push({ shell, controller });
  });
  return result;
}

function syncGlobalMuteButton(scope: ParentNode = document): void {
  const mounted = listMountedMediaControllers(scope);
  let canMute = false;
  for (const { controller } of mounted) {
    if (!controller.setMuted || !controller.isMuted) continue;
    canMute = true;
  }
  const muteToggleButton = scope.querySelector<HTMLButtonElement>(
    '[data-action="control-room-toggle-mute-all"]',
  );
  if (muteToggleButton) {
    muteToggleButton.disabled = !canMute;
    muteToggleButton.classList.toggle(
      "btn-disabled",
      muteToggleButton.disabled,
    );
    const label = controlRoomMuteIntent === "mute" ? "Unmute All" : "Mute All";
    const actionLabel =
      controlRoomMuteIntent === "mute" ? "Unmute all" : "Mute all";
    muteToggleButton.textContent = label;
    muteToggleButton.setAttribute(
      "aria-label",
      `${actionLabel} monitor previews`,
    );
  }
}

function syncGlobalPlaybackButton(scope: ParentNode = document): void {
  const mounted = listMountedMediaControllers(scope);
  const canTogglePlayback = mounted.some(
    ({ controller }) => !!controller.play || !!controller.pause,
  );
  const anyPlaying = mounted.some(
    ({ controller }) => controller.isPlaying?.() === true,
  );
  const playbackToggleButton = scope.querySelector<HTMLButtonElement>(
    '[data-action="control-room-toggle-playback-all"]',
  );
  if (playbackToggleButton) {
    playbackToggleButton.disabled = !canTogglePlayback;
    playbackToggleButton.classList.toggle(
      "btn-disabled",
      playbackToggleButton.disabled,
    );
    const label =
      controlRoomPlaybackIntent === "play" || anyPlaying
        ? "Pause All"
        : "Play All";
    const actionLabel =
      controlRoomPlaybackIntent === "play" || anyPlaying
        ? "Pause all"
        : "Play all";
    playbackToggleButton.textContent = label;
    playbackToggleButton.setAttribute(
      "aria-label",
      `${actionLabel} monitor previews`,
    );
  }
}

function syncGlobalMediaButtons(scope: ParentNode = document): void {
  syncGlobalPlaybackButton(scope);
  syncGlobalMuteButton(scope);
  syncCardPlaybackButtons(scope);
}

function clearCardPlayerShell(
  shell: HTMLElement | null,
  options: { resetMediaKey?: boolean } = {},
): void {
  if (!shell) return;
  controlRoomMediaControllers.get(shell)?.destroy();
  controlRoomMediaControllers.delete(shell);
  clearManagedHlsPlayer(
    shell.querySelector<HTMLElement>('[data-role="control-room-media-frame"]'),
  );
  shell.replaceChildren();
  if (options.resetMediaKey !== false) {
    delete shell.dataset.mediaKey;
  }
}

function setTileMessage(shell: HTMLElement, message: string): void {
  shell.innerHTML = `<div class="text-base-content/70 flex ${CONTROL_ROOM_PLAYER_HEIGHT_CLASS} items-center justify-center px-4 py-5 text-center text-sm leading-6">${escapeHtml(message)}</div>`;
}

function setLazyEmbedMessage(shell: HTMLElement, message: string): void {
  const actionLabel = monitorPreviewActionLabel(shell, "Load preview");
  shell.innerHTML = `<div class="text-base-content/70 flex ${CONTROL_ROOM_PLAYER_HEIGHT_CLASS} flex-col items-center justify-center gap-3 px-4 py-5 text-center text-sm leading-6">
        <span>${escapeHtml(message)}</span>
        <button type="button" class="btn btn-xs btn-accent btn-outline" data-action="control-room-load-preview" aria-label="${escapeHtml(actionLabel)}">Load preview</button>
    </div>`;
}

function setCardWarning(shell: HTMLElement, message: string | null): void {
  const article = shell.closest("article");
  const cardId = article?.dataset.cardId || "";
  if (cardId) {
    if (message) {
      controlRoomCardWarnings.set(cardId, message);
    } else {
      controlRoomCardWarnings.delete(cardId);
    }
  }
  const warning = article?.querySelector<HTMLElement>(
    '[data-role="control-room-card-warning"]',
  );
  if (!message) {
    warning?.remove();
    return;
  }
  const status = article?.querySelector<HTMLElement>(
    '[data-role="control-room-card-status"]',
  );
  const statusCluster = article?.querySelector<HTMLElement>(
    '[data-role="control-room-card-status-cluster"]',
  );
  const target =
    warning ||
    (() => {
      const badge = document.createElement("div");
      badge.className =
        "inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full border border-amber-500/35 bg-amber-500/12 text-xs font-bold text-amber-700 dark:text-amber-300";
      badge.dataset.role = "control-room-card-warning";
      badge.textContent = "!";
      if (status) {
        status.before(badge);
      } else {
        statusCluster?.appendChild(badge);
      }
      return badge;
    })();
  target.setAttribute("title", message);
  target.setAttribute("aria-label", message);
}

function refreshYouTubeCardWarning(
  shell: HTMLElement,
  monitoringUrl: string,
): void {
  void fetchYouTubeMonitoringStatus(monitoringUrl).then((status) => {
    if (!document.body.contains(shell)) return;
    // The shell is a reused DOM node: by the time this fetch resolves it may
    // have been reassigned to a different output/URL. Only apply the result
    // if the shell is still showing the media this fetch was started for,
    // otherwise a slow stale response can overwrite a newer card's status.
    if (shell.dataset.mediaKey !== monitoringUrl) return;
    setCardWarning(shell, getYouTubeMonitoringWarning(status));
  });
}

function detectMonitoringEmbedKind(url: string): MonitoringEmbedKind {
  if (/^srt:\/\//i.test(url)) return "unsupported";
  if (isHlsMonitoringUrl(url)) return "hls";
  if (isDirectVideoMonitoringUrl(url)) return "video";
  if (isYouTubeMonitoringUrl(url)) return "youtube";
  if (/^https?:\/\//i.test(url)) return "iframe";
  return "unsupported";
}

function createMonitorFrame(shell: HTMLElement): {
  frame: HTMLElement;
  controls: HTMLElement;
} {
  shell.innerHTML = "";

  const surface = document.createElement("div");
  surface.className = `${CONTROL_ROOM_MONITOR_FRAME_CLASS} ${CONTROL_ROOM_PLAYER_HEIGHT_CLASS}`;

  const frame = document.createElement("div");
  frame.dataset.role = "control-room-media-frame";
  frame.className = "h-full w-full";

  const topShade = document.createElement("div");
  topShade.className =
    "pointer-events-none absolute inset-x-0 top-0 h-14 bg-gradient-to-b from-black/45 to-transparent";

  const bottomShade = document.createElement("div");
  bottomShade.className =
    "pointer-events-none absolute inset-x-0 bottom-0 h-16 bg-gradient-to-t from-black/65 to-transparent";

  const controls = document.createElement("div");
  controls.dataset.role = "control-room-media-controls";
  controls.className =
    "absolute right-2 top-2 z-10 flex gap-1.5 opacity-0 transition-opacity duration-150 group-hover:opacity-100 group-focus-within:opacity-100";

  surface.appendChild(frame);
  surface.appendChild(topShade);
  surface.appendChild(bottomShade);
  surface.appendChild(controls);
  shell.appendChild(surface);
  return { frame, controls };
}

function addMonitorButton(
  controls: HTMLElement,
  action: string,
  label: string,
): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = CONTROL_ROOM_MONITOR_BUTTON_CLASS;
  button.dataset.action = action;
  button.textContent = label;
  controls.appendChild(button);
  button.setAttribute("aria-label", monitorPreviewActionLabel(button, label));
  return button;
}

async function requestMonitorFullscreen(shell: HTMLElement): Promise<void> {
  const fullscreenTarget =
    shell.querySelector<HTMLElement>('[data-role="control-room-media-frame"]')
      ?.parentElement || shell;
  if (!document.fullscreenElement) {
    await fullscreenTarget.requestFullscreen?.();
    return;
  }
  if (document.fullscreenElement === fullscreenTarget) {
    await document.exitFullscreen?.();
  } else {
    await fullscreenTarget.requestFullscreen?.();
  }
}

function setMuteButtonLabel(button: HTMLButtonElement, muted: boolean): void {
  const label = muted ? "Unmute" : "Mute";
  button.textContent = label;
  button.setAttribute("aria-label", monitorPreviewActionLabel(button, label));
}

function setPlaybackButtonLabel(
  button: HTMLButtonElement,
  playing: boolean,
): void {
  const label = playing ? "Pause" : "Play";
  button.textContent = label;
  button.setAttribute("aria-label", monitorPreviewActionLabel(button, label));
}

function syncCardPlaybackButtons(scope: ParentNode = document): void {
  listMountedMediaControllers(scope).forEach(({ shell, controller }) => {
    const button = shell.querySelector<HTMLButtonElement>(
      '[data-action="control-room-toggle-playback"]',
    );
    if (
      !button ||
      !controller.play ||
      !controller.pause ||
      !controller.isPlaying
    ) {
      return;
    }
    setPlaybackButtonLabel(button, controller.isPlaying());
  });
}

function registerMediaController(
  shell: HTMLElement,
  controller: ControlRoomMediaController,
): void {
  controlRoomMediaControllers.set(shell, controller);
  if (controller.setMuted) {
    controller.setMuted(controlRoomMuteIntent === "mute");
  }
  if (controlRoomPlaybackIntent === "play") {
    controller.play?.();
  } else {
    controller.pause?.();
  }
}

function getMediaControllerForAction(
  target: Element | null,
): { shell: HTMLElement; controller: ControlRoomMediaController } | null {
  const shell = target?.closest?.(
    '[data-role="control-room-player-shell"]',
  ) as HTMLElement | null;
  if (!shell) return null;
  const controller = controlRoomMediaControllers.get(shell);
  if (!controller) return null;
  return { shell, controller };
}

function loadYouTubeIframeApi(): Promise<YouTubeApiNamespace> {
  if (window.YT?.Player) {
    return Promise.resolve(window.YT);
  }
  if (youtubeIframeApiPromise) return youtubeIframeApiPromise;

  youtubeIframeApiPromise = new Promise((resolve, reject) => {
    const existingScript = document.querySelector<HTMLScriptElement>(
      'script[data-role="youtube-iframe-api"]',
    );
    const cleanup = () => {
      if (window.onYouTubeIframeAPIReady === handleReady) {
        window.onYouTubeIframeAPIReady = undefined;
      }
    };
    const handleReady = () => {
      cleanup();
      if (window.YT?.Player) {
        resolve(window.YT);
        return;
      }
      reject(new Error("YouTube iframe API loaded without Player"));
    };

    window.onYouTubeIframeAPIReady = handleReady;

    if (!existingScript) {
      const script = document.createElement("script");
      script.src = "https://www.youtube.com/iframe_api";
      script.async = true;
      script.dataset.role = "youtube-iframe-api";
      script.addEventListener("error", () => {
        cleanup();
        reject(new Error("Failed to load YouTube iframe API"));
      });
      document.head.appendChild(script);
      return;
    }

    existingScript.addEventListener("error", () => {
      cleanup();
      reject(new Error("Failed to load YouTube iframe API"));
    });
  });

  return youtubeIframeApiPromise;
}

function syncCardMedia(
  cardId: string,
  shell: HTMLElement,
  mediaUrl: string | null,
  loadOnDemand: boolean,
  emptyMessage: string,
): void {
  if (!mediaUrl) {
    const desiredKey = `message:${emptyMessage}`;
    if (shell.dataset.mediaKey === desiredKey) return;
    clearCardPlayerShell(shell, { resetMediaKey: false });
    shell.dataset.mediaKey = desiredKey;
    setTileMessage(shell, emptyMessage);
    return;
  }

  const embedKind = detectMonitoringEmbedKind(mediaUrl);
  const isWaitingForPreview =
    loadOnDemand && !controlRoomLoadedEmbedCards.has(cardId);
  const desiredKey = isWaitingForPreview ? `lazy:${mediaUrl}` : mediaUrl;
  if (shell.dataset.mediaKey === desiredKey) return;
  clearCardPlayerShell(shell, { resetMediaKey: false });
  shell.dataset.mediaKey = desiredKey;

  if (embedKind === "unsupported") {
    setTileMessage(
      shell,
      "This URL is saved, but this card can only preview browser-playable sources today.",
    );
    return;
  }
  if (isWaitingForPreview) {
    setLazyEmbedMessage(shell, "Preview is not loaded yet.");
    return;
  }

  if (embedKind === "hls" || embedKind === "video") {
    const { frame, controls } = createMonitorFrame(shell);
    const playbackButton = addMonitorButton(
      controls,
      "control-room-toggle-playback",
      "Play",
    );
    const muteButton = addMonitorButton(
      controls,
      "control-room-toggle-mute",
      "Unmute",
    );
    addMonitorButton(controls, "control-room-toggle-fullscreen", "Fullscreen");

    if (embedKind === "hls") {
      renderManagedHlsPlayer(frame, mediaUrl, {
        className: `${CONTROL_ROOM_PLAYER_HEIGHT_CLASS} w-full bg-black object-contain`,
        loadingLabel: "Loading...",
        idleLabel: "Paused",
        autoStart: false,
        showOverlayButton: false,
        controls: false,
      });
      const managedController = getManagedHlsController(frame);
      const video = frame.querySelector<HTMLVideoElement>(
        '[data-role="managed-hls-video"]',
      );
      if (!managedController || !video) return;
      registerMediaController(shell, {
        destroy: () => clearManagedHlsPlayer(frame),
        play: () => managedController.play(),
        pause: () => managedController.pause(),
        isPlaying: () => managedController.isPlaying(),
        isMuted: () => managedController.isMuted(),
        setMuted: (muted: boolean) => managedController.setMuted(muted),
      });
      setMuteButtonLabel(muteButton, video.muted);
      setPlaybackButtonLabel(playbackButton, managedController.isPlaying());
      video.addEventListener("play", () => {
        setPlaybackButtonLabel(playbackButton, true);
        syncGlobalPlaybackButton(document);
      });
      video.addEventListener("pause", () => {
        setPlaybackButtonLabel(playbackButton, false);
        syncGlobalPlaybackButton(document);
      });
    } else {
      const video = document.createElement("video");
      video.className = `${CONTROL_ROOM_PLAYER_HEIGHT_CLASS} w-full bg-black object-contain`;
      video.controls = false;
      video.setAttribute("controlslist", "nodownload");
      video.autoplay = true;
      video.muted = true;
      video.playsInline = true;
      frame.appendChild(video);
      video.src = mediaUrl;
      void video.play().catch(() => {
        // Autoplay can be blocked; controls remain available.
      });
      registerMediaController(shell, {
        destroy: () => {
          video.pause();
          video.removeAttribute("src");
          video.load();
        },
        play: () => {
          void video.play().catch(() => {
            // Autoplay can still be denied until the browser is ready.
          });
        },
        pause: () => {
          video.pause();
        },
        isPlaying: () => !video.paused && !video.ended,
        isMuted: () => video.muted,
        setMuted: (muted: boolean) => {
          video.muted = muted;
        },
      });
      setMuteButtonLabel(muteButton, video.muted);
      setPlaybackButtonLabel(playbackButton, !video.paused && !video.ended);
      video.addEventListener("play", () => {
        setPlaybackButtonLabel(playbackButton, true);
        syncGlobalPlaybackButton(document);
      });
      video.addEventListener("pause", () => {
        setPlaybackButtonLabel(playbackButton, false);
        syncGlobalPlaybackButton(document);
      });
    }
    return;
  }

  if (embedKind === "youtube") {
    const { frame, controls } = createMonitorFrame(shell);
    const playbackButton = addMonitorButton(
      controls,
      "control-room-toggle-playback",
      "Play",
    );
    playbackButton.disabled = true;
    playbackButton.classList.add("btn-disabled");
    const cardId = shell.closest("article")?.dataset.cardId || "";
    setCardWarning(shell, controlRoomCardWarnings.get(cardId) || null);
    const muteButton = addMonitorButton(
      controls,
      "control-room-toggle-mute",
      "Unmute",
    );
    muteButton.disabled = true;
    muteButton.classList.add("btn-disabled");
    addMonitorButton(controls, "control-room-toggle-fullscreen", "Fullscreen");

    const iframeWrap = document.createElement("div");
    iframeWrap.className = "pointer-events-none absolute inset-[-7%]";

    const iframe = document.createElement("iframe");
    const iframeId = `control-room-youtube-${Math.random().toString(36).slice(2, 10)}`;
    iframe.id = iframeId;
    iframe.src = toEmbeddableMonitoringUrl(mediaUrl);
    iframe.className = "h-full w-full border-0 bg-black";
    iframe.allow =
      "autoplay; clipboard-write; encrypted-media; picture-in-picture; web-share";
    iframe.referrerPolicy = "strict-origin-when-cross-origin";
    iframe.loading = "lazy";
    iframe.title = "Monitoring player";
    iframe.setAttribute("allowfullscreen", "true");
    iframeWrap.appendChild(iframe);
    frame.className = `${frame.className} relative`;
    frame.appendChild(iframeWrap);

    let player: YouTubePlayerApi | null = null;
    let disposed = false;
    registerMediaController(shell, {
      destroy: () => {
        disposed = true;
        player?.destroy();
      },
      play: () => {
        player?.playVideo();
      },
      pause: () => {
        player?.pauseVideo();
      },
      isPlaying: () => player?.getPlayerState?.() === 1,
      isMuted: () => player?.isMuted() ?? true,
      setMuted: (muted: boolean) => {
        if (!player) return;
        if (muted) {
          player.mute();
        } else {
          player.unMute();
        }
      },
    });

    void loadYouTubeIframeApi()
      .then((YT) => {
        if (disposed) return;
        player = new YT.Player(iframeId, {
          events: {
            onReady: () => {
              if (!player) return;
              player.mute();
              setPlaybackButtonLabel(
                playbackButton,
                player.getPlayerState?.() === 1,
              );
              playbackButton.disabled = false;
              playbackButton.classList.remove("btn-disabled");
              setMuteButtonLabel(muteButton, true);
              muteButton.disabled = false;
              muteButton.classList.remove("btn-disabled");
              refreshYouTubeCardWarning(shell, mediaUrl);
            },
            onStateChange: () => {
              if (!player) return;
              setPlaybackButtonLabel(
                playbackButton,
                player.getPlayerState?.() === 1,
              );
              syncGlobalPlaybackButton(document);
            },
          },
        });
      })
      .catch(() => {
        playbackButton.disabled = true;
        playbackButton.classList.add("btn-disabled");
        playbackButton.textContent = "Unavailable";
        muteButton.disabled = true;
        muteButton.classList.add("btn-disabled");
        muteButton.textContent = "Unavailable";
      });
    return;
  }

  if (embedKind === "iframe") {
    const { frame, controls } = createMonitorFrame(shell);
    addMonitorButton(controls, "control-room-toggle-fullscreen", "Fullscreen");
    const iframe = document.createElement("iframe");
    iframe.src = toEmbeddableMonitoringUrl(mediaUrl);
    iframe.className = `${CONTROL_ROOM_PLAYER_HEIGHT_CLASS} w-full border-0 bg-black`;
    iframe.allow =
      "autoplay; clipboard-write; encrypted-media; picture-in-picture; web-share";
    iframe.referrerPolicy = "strict-origin-when-cross-origin";
    iframe.loading = "lazy";
    iframe.title = "Monitoring player";
    iframe.setAttribute("allowfullscreen", "true");
    frame.appendChild(iframe);
    registerMediaController(shell, {
      destroy: () => {
        iframe.src = "about:blank";
      },
    });
    return;
  }

  setTileMessage(shell, emptyMessage);
}

export function openOutputMonitoringUrl(
  url: string | null | undefined,
): void {
  const openUrl = toOpenableMonitoringUrl(url || null);
  if (!openUrl) return;
  openMonitorUrl(openUrl, "Monitor");
}

export {
  clearCardPlayerShell,
  controlRoomCardWarnings,
  controlRoomLoadedEmbedCards,
  isHlsMonitoringUrl,
  isDirectVideoMonitoringUrl,
  isYouTubeMonitoringUrl,
  listMountedMediaControllers,
  refreshYouTubeCardWarning,
  setCardWarning,
  setMuteButtonLabel,
  setPlaybackButtonLabel,
  syncCardMedia,
  syncCardPlaybackButtons,
  syncGlobalMediaButtons,
  syncGlobalMuteButton,
  syncGlobalPlaybackButton,
  toEmbeddableMonitoringUrl,
  toOpenableMonitoringUrl,
  getMediaControllerForAction,
  openMonitorUrl,
  requestMonitorFullscreen,
};
