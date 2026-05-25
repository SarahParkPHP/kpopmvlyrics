import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AlignmentLine, CaptionLine, MemberProfile, SongPackage, VideoDownloadProgress, VideoFormat, VideoMetadata } from "./types";

const inTauri = "__TAURI_INTERNALS__" in window;

export async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  if (!inTauri) {
    return mockCommand<T>(name, args);
  }
  return invoke<T>(name, args);
}

async function mockCommand<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  if (name === "resolve_video_metadata") {
    const url = String(args?.url ?? "");
    const videoId = url.match(/[?&]v=([A-Za-z0-9_-]{11})/)?.[1] ?? url.match(/youtu\.be\/([A-Za-z0-9_-]{11})/)?.[1] ?? "dQw4w9WgXcQ";
    return { videoId, originalUrl: url, title: "TWICE Talk That Talk Official MV", artistHint: "TWICE" } as T;
  }
  if (name === "list_video_formats") {
    return [
      { formatId: "137+140", label: "1080p MP4", height: 1080, ext: "mp4" },
      { formatId: "136+140", label: "720p MP4", height: 720, ext: "mp4" },
      { formatId: "135+140", label: "480p MP4", height: 480, ext: "mp4" },
      { formatId: "18", label: "360p MP4", height: 360, ext: "mp4" },
    ] as T;
  }
  if (name === "resolve_video_stream") {
    return "" as T;
  }
  if (name === "import_lyrics" || name === "fetch_lyrics") {
    const raw = String(args?.rawText ?? "Nayeon: Tell me what you want\nMomo: Tell me what you need\nSana: A to Z da malhaebwa");
    const lines = raw
      .split(/\n+/)
      .filter(Boolean)
      .map((line, index) => {
        const match = line.match(/^([^:]+):\s*(.+)$/);
        return {
          id: index + 1,
          songId: 1,
          index,
          member: match?.[1] ?? null,
          original: match?.[2] ?? line,
          romanization: null,
          english: null,
        };
      });
    const members = [...new Set(lines.map((line) => line.member).filter(Boolean))].map((stageName, index) => ({
      id: index + 1,
      stageName: String(stageName),
      color: ["#e84855", "#2f80ed", "#27ae60", "#f2994a"][index % 4],
    }));
    return { song: { id: 1, title: String(args?.title ?? "Fixture Song"), artist: String(args?.artist ?? "Fixture Group"), groupName: String(args?.artist ?? "Fixture Group") }, lines, members, provider: "browser-fixture" } as T;
  }
  if (name === "import_captions" || name === "fetch_captions") {
    return [
      { id: 1, videoId: String(args?.videoId ?? "fixture"), index: 0, startMs: 1000, endMs: 2400, text: "Tell me what you want" },
      { id: 2, videoId: String(args?.videoId ?? "fixture"), index: 1, startMs: 2500, endMs: 3900, text: "Tell me what you need" },
      { id: 3, videoId: String(args?.videoId ?? "fixture"), index: 2, startMs: 4000, endMs: 5600, text: "A to Z da malhaebwa" },
    ] as T;
  }
  if (name === "align_lyrics") {
    return [
      { lyricIndex: 0, captionIndex: 0, startMs: 1000, endMs: 2400, confidence: 1, needsReview: false },
      { lyricIndex: 1, captionIndex: 1, startMs: 2500, endMs: 3900, confidence: 1, needsReview: false },
      { lyricIndex: 2, captionIndex: 2, startMs: 4000, endMs: 5600, confidence: 0.86, needsReview: false },
    ] as T;
  }
  if (name === "save_alignment_edits" || name === "save_member_override" || name === "show_video_browser" || name === "position_video_browser") {
    return undefined as T;
  }
  if (name === "search_member_profiles") {
    return [] as T;
  }
  throw new Error(`Mock command not implemented: ${name}`);
}

export const api = {
  resolveVideoMetadata: (url: string) => command<VideoMetadata>("resolve_video_metadata", { url }),
  listVideoFormats: (url: string) => command<VideoFormat[]>("list_video_formats", { url }),
  resolveVideoStream: (url: string, formatId?: string) => command<string>("resolve_video_stream", { url, formatId: formatId ?? null }),
  onVideoDownloadProgress: (handler: (progress: VideoDownloadProgress) => void) => {
    if (!inTauri) {
      return Promise.resolve(() => undefined);
    }
    return listen<VideoDownloadProgress>("video-download-progress", (event) => handler(event.payload));
  },
  fetchLyrics: (query: string) => command<SongPackage>("fetch_lyrics", { query }),
  importLyrics: (rawText: string, title: string, artist: string) => command<SongPackage>("import_lyrics", { rawText, title, artist }),
  fetchCaptions: (videoId: string) => command<CaptionLine[]>("fetch_captions", { videoId }),
  importCaptions: (videoId: string, rawText: string) => command<CaptionLine[]>("import_captions", { videoId, rawText }),
  alignLyrics: (songId: number, videoId: string) => command<AlignmentLine[]>("align_lyrics", { songId, videoId }),
  saveAlignmentEdits: (songId: number, videoId: string, lines: AlignmentLine[]) => command<void>("save_alignment_edits", { songId, videoId, lines }),
  searchMemberProfiles: (groupName: string) => command<MemberProfile[]>("search_member_profiles", { groupName }),
  saveMemberOverride: (groupName: string, member: MemberProfile) => command<MemberProfile>("save_member_override", { groupName, member }),
};
