export interface VideoMetadata {
  videoId: string;
  title?: string | null;
  artistHint?: string | null;
  originalUrl: string;
}

export interface VideoFormat {
  formatId: string;
  label: string;
  height?: number | null;
  ext?: string | null;
}

export interface VideoDownloadProgress {
  percent: number;
  status: string;
  active: boolean;
}

export interface SongPackage {
  song: Song;
  lines: LyricLine[];
  members: MemberProfile[];
  provider: string;
}

export interface Song {
  id?: number | null;
  title: string;
  artist: string;
  groupName?: string | null;
  sourceUrl?: string | null;
}

export interface LyricLine {
  id?: number | null;
  songId?: number | null;
  index: number;
  member?: string | null;
  original: string;
  romanization?: string | null;
  english?: string | null;
  segments?: LyricSegment[];
}

export interface LyricSegment {
  language: "original" | "romanization" | "english" | string;
  text: string;
  member?: string | null;
  color?: string | null;
}

export interface CaptionLine {
  id?: number | null;
  videoId: string;
  index: number;
  startMs: number;
  endMs: number;
  text: string;
}

export interface AlignmentLine {
  lyricIndex: number;
  captionIndex?: number | null;
  startMs: number;
  endMs: number;
  confidence: number;
  needsReview: boolean;
}

export interface MemberProfile {
  id?: number | null;
  stageName: string;
  realName?: string | null;
  color: string;
  imageUrl?: string | null;
  localImagePath?: string | null;
  provider?: string | null;
}

export type LanguageKey = "original" | "romanization" | "english";
