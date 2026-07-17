export interface ControlRoomState {
  pipelineId: string | null;
  page: number;
  searchQuery: string;
}

export interface ControlRoomWorkspaceDependencies {
  selectedPipelineId: () => string | null;
  selectPipeline: (pipelineId: string | null) => void;
  openMonitorView: (pipelineId: string | null) => void;
}

export interface ControlRoomOutputOption {
  outputId: string;
  pipelineId: string;
  pipelineName: string;
  outputName: string;
  monitoringUrl: string | null;
  status: string;
  flapping: boolean;
}

export interface ControlRoomCardDescriptor {
  id: string;
  title: string;
  mediaUrl: string | null;
  loadOnDemand: boolean;
  emptyMessage: string;
  openUrl: string | null;
  copyUrl: string | null;
  editable: boolean;
  outputId: string | null;
  pipelineId: string | null;
  monitoringUrl: string | null;
  statusLabel?: string | null;
}

export type MonitoringEmbedKind =
  "hls" | "video" | "youtube" | "iframe" | "unsupported";

export interface YouTubePlayerApi {
  mute(): void;
  unMute(): void;
  isMuted(): boolean;
  playVideo(): void;
  pauseVideo(): void;
  getPlayerState?(): number;
  getVideoData?(): {
    title?: string;
    isLive?: boolean;
    isPlayable?: boolean;
    errorCode?: string | null;
  };
  getDuration?(): number;
  destroy(): void;
}

export interface YouTubeApiNamespace {
  Player: new (
    elementId: string,
    options: Record<string, unknown>,
  ) => YouTubePlayerApi;
}

export interface ControlRoomMediaController {
  destroy(): void;
  play?(): void;
  pause?(): void;
  isPlaying?(): boolean;
  isMuted?(): boolean;
  setMuted?(muted: boolean): void;
}
