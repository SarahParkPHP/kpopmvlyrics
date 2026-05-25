import { useState } from "react";

export function useYouTubePlayer(videoId?: string) {
  const [currentMs, setCurrentMs] = useState(0);

  void videoId;
  void setCurrentMs;
  return { currentMs, ready: false, player: null };
}
