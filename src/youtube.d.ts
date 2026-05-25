interface Window {
  onYouTubeIframeAPIReady?: () => void;
  YT?: typeof YT;
}

declare namespace YT {
  class Player {
    constructor(elementId: string, options: PlayerOptions);
    loadVideoById(videoId: string): void;
    getCurrentTime(): number;
    playVideo(): void;
    pauseVideo(): void;
  }

  interface PlayerOptions {
    videoId?: string;
    playerVars?: Record<string, unknown>;
    events?: {
      onReady?: () => void;
    };
  }
}
