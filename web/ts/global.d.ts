declare global {
  interface HlsConfig {
    [key: string]: unknown;
  }

  interface HlsEvents {
    MEDIA_ATTACHED: string;
    MANIFEST_PARSED: string;
    ERROR: string;
    AUDIO_TRACKS_UPDATED: string;
    AUDIO_TRACK_SWITCHED: string;
    LEVEL_SWITCHED: string;
    LEVEL_LOADED: string;
    FRAG_LOADED: string;
  }

  interface Hls {
    readonly config: HlsConfig;
    loadSource(url: string): void;
    attachMedia(video: HTMLVideoElement): void;
    detachMedia(): void;
    destroy(): void;
    on(event: string, handler: (...args: unknown[]) => void): void;
    off(event: string, handler: (...args: unknown[]) => void): void;
    readonly audioTracks: Array<{
      id: number;
      name: string;
      lang?: string;
      groupId?: string;
    }>;
    audioTrack: number;
    loadSource(url: string): void;
    attachMedia(video: HTMLVideoElement): void;
    recoverMediaError(): void;
    swapAudioCodec(): void;
    nextLevel: number;
    readonly levels: Array<{
      height: number;
      width: number;
      bitrate: number;
      name: string;
    }>;
    currentLevel: number;
    loadLevel: number;
    nextLoadLevel: number;
    readonly firstLevel: number;
    readonly autoLevelEnabled: boolean;
    readonly manualLevel: number;
    readonly latency: number;
    readonly targetLatency: number;
    readonly liveSyncPosition: number;
    startLoad(startPosition?: number): void;
    stopLoad(): void;
  }

  interface HlsConstructor {
    new (config?: Partial<HlsConfig>): Hls;
    isSupported(): boolean;
    Events: HlsEvents;
  }

  interface Window {
    Hls?: HlsConstructor;
    __RESTREAM_BASE_PATH__?: string;
  }
}

export {};
