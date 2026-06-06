import { useEffect, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { convertFileSrc } from "@tauri-apps/api/core";
import { AlertCircle, Check, Clock, Download, FileJson, FileVideo, Languages, Link, Loader2, Pause, Pencil, Play, RotateCcw, Save, Search, Settings, Upload } from "lucide-react";
import { api } from "./tauri";
import type { AlignmentLine, CaptionLine, LanguageKey, LyricLine, LyricSegment, MemberProfile, SongPackage, VideoFormat, VideoMetadata } from "./types";

const AUTO_QUALITY = "auto";

const initialLyrics = "Nayeon: Tell me what you want\nMomo: Tell me what you need\nSana: A to Z da malhaebwa";
const initialCaptions = "WEBVTT\n\n00:00:01.000 --> 00:00:02.400\nTell me what you want\n\n00:00:02.500 --> 00:00:03.900\nTell me what you need\n\n00:00:04.000 --> 00:00:05.600\nA to Z da malhaebwa";

export function App() {
  const [url, setUrl] = useState("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
  const [query, setQuery] = useState("");
  const [metadata, setMetadata] = useState<VideoMetadata | null>(null);
  const [songPackage, setSongPackage] = useState<SongPackage | null>(null);
  const [captions, setCaptions] = useState<CaptionLine[]>([]);
  const [alignment, setAlignment] = useState<AlignmentLine[]>([]);
  const [languages, setLanguages] = useState<Record<LanguageKey, boolean>>({ original: true, romanization: false, english: true });
  const [editorOpen, setEditorOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [manualLyrics, setManualLyrics] = useState(initialLyrics);
  const [manualCaptions, setManualCaptions] = useState(initialCaptions);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [playerLoaded, setPlayerLoaded] = useState(false);
  const [availableFormats, setAvailableFormats] = useState<VideoFormat[]>([]);
  const [selectedFormatId, setSelectedFormatId] = useState(AUTO_QUALITY);
  const [buffering, setBuffering] = useState(false);
  const [syncRunning, setSyncRunning] = useState(false);
  const [currentMs, setCurrentMs] = useState(0);

  useEffect(() => {
    let unlistenPosition: (() => void) | undefined;
    let unlistenError: (() => void) | undefined;

    void api.onVideoPosition((position) => {
      setCurrentMs(position.ms);
      setBuffering(position.buffering);
      setSyncRunning(position.playing);
    }).then((dispose) => {
      unlistenPosition = dispose;
    });

    void api.onVideoPlayerError((message) => {
      setError(message);
    }).then((dispose) => {
      unlistenError = dispose;
    });

    return () => {
      unlistenPosition?.();
      unlistenError?.();
    };
  }, []);

  const activeIndex = useMemo(() => {
    const found = alignment.find((line) => currentMs >= line.startMs && currentMs <= line.endMs);
    if (found) {
      return found.lyricIndex;
    }
    const previous = alignment
      .filter((line) => currentMs >= line.startMs)
      .sort((left, right) => right.startMs - left.startMs)[0];
    return previous?.lyricIndex ?? 0;
  }, [alignment, currentMs]);

  const activeMembers = useMemo(() => {
    const line = songPackage?.lines.find((lyric) => lyric.index === activeIndex);
    return new Set(line?.member ? [line.member] : []);
  }, [songPackage?.lines, activeIndex]);

  const activeQualityLabel = useMemo(() => {
    if (selectedFormatId === AUTO_QUALITY) {
      return "Auto";
    }
    return availableFormats.find((format) => format.formatId === selectedFormatId)?.label ?? selectedFormatId;
  }, [availableFormats, selectedFormatId]);

  async function loadPlayer(formatId = selectedFormatId): Promise<boolean> {
    setError(null);
    setBuffering(true);
    try {
      await api.playerLoad(url, formatId === AUTO_QUALITY ? undefined : formatId);
      setPlayerLoaded(true);
      return true;
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return false;
    } finally {
      setBuffering(false);
    }
  }

  async function run<T>(label: string, action: () => Promise<T>): Promise<T | null> {
    setBusy(label);
    setError(null);
    setMessage(null);
    try {
      const result = await action();
      setMessage(`${label} complete`);
      return result;
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return null;
    } finally {
      setBusy(null);
    }
  }

  async function resolveVideo() {
    setBusy("Video");
    setError(null);
    setMessage(null);
    try {
      const result = await api.resolveVideoMetadata(url);
      setMetadata(result);
      const lyricQuery = queryFromMetadata(result);
      setQuery(lyricQuery);
      setCaptions([]);
      setAlignment([]);
      setSyncRunning(false);
      setCurrentMs(0);
      setPlayerLoaded(false);
      setAvailableFormats([]);
      setSelectedFormatId(AUTO_QUALITY);

      setBusy("Video formats");
      const formats = await api.listVideoFormats(url).catch(() => []);
      setAvailableFormats(formats);

      if (lyricQuery) {
        await loadPlayer();

        setBusy("Lyrics");
        const lyrics = await api.fetchLyrics(lyricQuery);
        await applySongPackage(lyrics);
        setMessage("Lyrics complete");

        setBusy("Captions");
        const fetchedCaptions = await api.fetchCaptions(result.videoId);
        setCaptions(fetchedCaptions);
        setMessage("Captions complete");

        if (lyrics.song.id && fetchedCaptions.length) {
          setBusy("Alignment");
          const aligned = await api.alignLyrics(lyrics.song.id, result.videoId);
          setAlignment(aligned);
          setMessage("Video, lyrics, captions, and alignment complete");
        }
      } else {
        setMessage("Video complete");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  }

  async function fetchLyrics() {
    const result = await run("Lyrics", () => api.fetchLyrics(query));
    if (result) {
      await applySongPackage(result);
    }
  }

  async function applySongPackage(result: SongPackage) {
    setSongPackage(result);
    if (result.song.groupName) {
      const profiles = await api.searchMemberProfiles(result.song.groupName).catch(() => []);
      if (profiles.length) {
        setSongPackage({ ...result, members: mergeMembers(result.members, profiles) });
      }
    }
  }

  async function importLyrics() {
    const result = await run("Lyric import", () => api.importLyrics(manualLyrics, query || "Imported Song", query.split(" ")[0] || "Imported Group"));
    if (result) {
      setSongPackage(result);
    }
  }

  async function fetchCaptions() {
    const videoId = metadata?.videoId;
    if (!videoId) {
      setError("Resolve a YouTube URL first");
      return;
    }
    const result = await run("Captions", () => api.fetchCaptions(videoId));
    if (result) {
      setCaptions(result);
    }
  }

  async function importCaptions() {
    const videoId = metadata?.videoId;
    if (!videoId) {
      setError("Resolve a YouTube URL first");
      return;
    }
    const result = await run("Caption import", () => api.importCaptions(videoId, manualCaptions));
    if (result) {
      setCaptions(result);
    }
  }

  async function align() {
    const songId = songPackage?.song.id;
    const videoId = metadata?.videoId;
    if (!songId || !videoId) {
      setError("Load lyrics and resolve a video first");
      return;
    }
    const result = await run("Alignment", () => api.alignLyrics(songId, videoId));
    if (result) {
      setAlignment(result);
    }
  }

  function startAlignedSync() {
    void api.playerPlay();
  }

  function resetAlignedSync() {
    void api.playerPause();
    void api.playerSeek(0);
    setSyncRunning(false);
    setCurrentMs(0);
  }

  async function changeVideoQuality(formatId: string) {
    setSelectedFormatId(formatId);
    if (!playerLoaded) {
      return;
    }
    setBuffering(true);
    try {
      await api.playerSetQuality(formatId);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBuffering(false);
    }
  }

  async function saveEdits() {
    const songId = songPackage?.song.id;
    const videoId = metadata?.videoId;
    if (!songId || !videoId) {
      return;
    }
    await run("Save", () => api.saveAlignmentEdits(songId, videoId, alignment));
  }

  function exportJson() {
    if (!metadata || !songPackage) {
      setError("Load lyrics and resolve a video first");
      return;
    }
    const payload = buildJsonExport(metadata, songPackage, alignment);
    const blob = new Blob([`${JSON.stringify(payload, null, 2)}\n`], { type: "application/json" });
    const href = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = href;
    link.download = `${safeFilename(songPackage.song.artist)}-${safeFilename(songPackage.song.title)}-${metadata.videoId}.json`;
    document.body.append(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(href);
    setMessage("JSON export complete");
    setError(null);
  }

  async function pickMemberImage(member: MemberProfile) {
    const selected = await open({
      multiple: false,
      filters: [{ name: "Images", extensions: ["jpg", "jpeg", "gif", "png", "webp", "avif"] }],
    });
    if (!selected || Array.isArray(selected) || !songPackage) {
      return;
    }
    const updated = { ...member, localImagePath: selected };
    const groupName = songPackage.song.groupName ?? songPackage.song.artist;
    await api.saveMemberOverride(groupName, updated);
    setSongPackage({
      ...songPackage,
      members: songPackage.members.map((item) => (item.stageName === member.stageName ? updated : item)),
    });
  }

  return (
    <main className="app-shell">
      <section className="stage-band">
        <div className="stage-head">
          <MemberStrip members={songPackage?.members ?? []} activeMembers={activeMembers} onPickImage={pickMemberImage} />
          <LanguageToggles value={languages} onChange={setLanguages} />
        </div>
        <LyricStage lines={songPackage?.lines ?? []} alignment={alignment} currentMs={currentMs} activeIndex={activeIndex} languages={languages} members={songPackage?.members ?? []} />
      </section>

      <section className="player-band">
        <div className="address-row">
          <div className="field wide">
            <Link size={16} />
            <input value={url} onChange={(event) => setUrl(event.target.value)} placeholder="Paste a YouTube MV URL" />
          </div>
          <select
            className="quality-select"
            value={selectedFormatId}
            onChange={(event) => void changeVideoQuality(event.target.value)}
            disabled={busy !== null || !metadata}
            aria-label="Video quality"
          >
            <option value={AUTO_QUALITY}>Auto</option>
            {availableFormats.map((format) => (
              <option key={format.formatId} value={format.formatId}>
                {format.label}
              </option>
            ))}
          </select>
          <button onClick={resolveVideo} disabled={busy !== null}>
            <Search size={17} /> Open
          </button>
          <button onClick={() => void loadPlayer()} disabled={busy !== null}>
            <FileVideo size={17} /> Stream
          </button>
          <button onClick={() => setEditorOpen((value) => !value)} aria-pressed={editorOpen}>
            <Pencil size={17} /> Editor
          </button>
          <button className="icon-button" onClick={() => setSettingsOpen((value) => !value)} aria-pressed={settingsOpen} aria-label="Settings">
            <Settings size={18} />
          </button>
        </div>
        <div className={`player-grid ${settingsOpen ? "settings-open" : ""}`}>
          {settingsOpen && <div className="load-panel">
            <div className="field">
              <Search size={16} />
              <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Artist and song title" />
            </div>
            <div className="button-grid">
              <button onClick={fetchLyrics} disabled={busy !== null}>
                <Download size={17} /> Fetch Lyrics
              </button>
              <button onClick={fetchCaptions} disabled={busy !== null}>
                <Clock size={17} /> Fetch Captions
              </button>
              <button onClick={align} disabled={busy !== null || !songPackage || !captions.length}>
                <Check size={17} /> Align
              </button>
              <button onClick={saveEdits} disabled={busy !== null || !alignment.length}>
                <Save size={17} /> Save
              </button>
              <button onClick={() => syncRunning ? void api.playerPause() : startAlignedSync()} disabled={!alignment.length || !playerLoaded}>
                {syncRunning ? <Pause size={17} /> : <Play size={17} />} {syncRunning ? "Pause Sync" : "Start Sync"}
              </button>
              <button onClick={() => resetAlignedSync()} disabled={!alignment.length || !playerLoaded}>
                <RotateCcw size={17} /> Reset Sync
              </button>
            </div>
            <div className="status-line">
              {busy && <><Loader2 className="spin" size={16} /> {busy} running</>}
              {!busy && buffering && <><Loader2 className="spin" size={16} /> Buffering video</>}
              {!busy && !buffering && message && <><Check size={16} /> {message}</>}
              {error && <><AlertCircle size={16} /> {error}</>}
            </div>
            <div className="meta-grid">
              <span>{metadata ? metadata.videoId : "No video"}</span>
              <span>{songPackage ? `${songPackage.song.artist} - ${songPackage.song.title}` : "No song"}</span>
              <span>{captions.length} captions</span>
            <span>{playerLoaded ? `${activeQualityLabel} native player ready` : "No stream"}</span>
            <span>{alignment.filter((line) => line.needsReview).length} review</span>
            <span>{syncRunning ? "sync running" : "sync paused"}</span>
          </div>
          </div>
          }
        </div>
      </section>

      {editorOpen && (
        <section className="editor-band">
          <div className="import-grid">
            <label>
              Manual lyrics
              <textarea value={manualLyrics} onChange={(event) => setManualLyrics(event.target.value)} />
            </label>
            <label>
              Manual captions
              <textarea value={manualCaptions} onChange={(event) => setManualCaptions(event.target.value)} />
            </label>
            <div className="editor-actions">
              <button onClick={importLyrics}>
                <Upload size={17} /> Import Lyrics
              </button>
              <button onClick={importCaptions}>
                <Upload size={17} /> Import Captions
              </button>
              <button onClick={() => setAlignment(shiftAlignment(alignment, -500))}>-0.5s</button>
              <button onClick={() => setAlignment(shiftAlignment(alignment, 500))}>+0.5s</button>
              <button onClick={exportJson}>
                <FileJson size={17} /> Export JSON
              </button>
            </div>
          </div>
          <AlignmentTable
            lines={songPackage?.lines ?? []}
            alignment={alignment}
            onChange={setAlignment}
            members={songPackage?.members ?? []}
            onMemberChange={(lineIndex, member) => {
              if (!songPackage) return;
              setSongPackage({
                ...songPackage,
                lines: songPackage.lines.map((line) => (line.index === lineIndex ? { ...line, member } : line)),
              });
            }}
          />
        </section>
      )}

    </main>
  );
}

function MemberStrip({ members, activeMembers, onPickImage }: { members: MemberProfile[]; activeMembers: Set<string>; onPickImage: (member: MemberProfile) => void }) {
  const [brokenImages, setBrokenImages] = useState<Set<string>>(() => new Set());

  return (
    <div className="member-strip">
      {members.length === 0 && <span className="empty">Members appear after lyrics are loaded</span>}
      {members.map((member) => {
        const active = activeMembers.has(member.stageName);
        const image = member.localImagePath ? convertFileSrc(member.localImagePath) : member.imageUrl;
        const visibleImage = image && !brokenImages.has(image) ? image : null;
        return (
          <button key={member.stageName} className={`member ${active ? "active" : ""}`} style={{ "--member-color": member.color } as React.CSSProperties} onClick={() => onPickImage(member)} title="Choose member image">
            {visibleImage ? <img src={visibleImage} alt="" onError={() => setBrokenImages((current) => new Set(current).add(visibleImage))} /> : <span className="member-initials">{initials(member.stageName)}</span>}
            <span>{member.stageName}</span>
          </button>
        );
      })}
    </div>
  );
}

function initials(name: string) {
  return name
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((part) => part[0]?.toUpperCase())
    .join("");
}

function LanguageToggles({ value, onChange }: { value: Record<LanguageKey, boolean>; onChange: (next: Record<LanguageKey, boolean>) => void }) {
  return (
    <div className="language-toggles" aria-label="Lyric variants">
      <Languages size={18} />
      {(["original", "romanization", "english"] as LanguageKey[]).map((key) => (
        <button key={key} aria-pressed={value[key]} onClick={() => onChange({ ...value, [key]: !value[key] })}>
          {key === "romanization" ? "Roman" : key[0].toUpperCase() + key.slice(1)}
        </button>
      ))}
    </div>
  );
}

function LyricStage({ lines, alignment, currentMs, activeIndex, languages, members }: { lines: LyricLine[]; alignment: AlignmentLine[]; currentMs: number; activeIndex: number; languages: Record<LanguageKey, boolean>; members: MemberProfile[] }) {
  const visible = lines.filter((line) => Math.abs(line.index - activeIndex) <= 2);
  const usedColors = useMemo(() => new Set(members.map((member) => normalizeHexColor(member.color)).filter(Boolean) as string[]), [members]);
  return (
    <div className="lyric-stage">
      <div className="clock">{formatMs(currentMs)}</div>
      {visible.length === 0 && <div className="stage-empty">Load or import lyrics, then align captions to start synced playback.</div>}
      {visible.map((line) => {
        const timing = alignment.find((item) => item.lyricIndex === line.index);
        return (
          <article key={line.index} className={`lyric-line ${line.index === activeIndex ? "active" : ""}`}>
            <span className="member-name">{line.member ?? "All"}</span>
            <div>
              {languages.original && <p>{renderLyricText(line, "original", line.original, usedColors)}</p>}
              {languages.romanization && line.romanization && <p className="variant">{renderLyricText(line, "romanization", line.romanization, usedColors)}</p>}
              {languages.english && line.english && <p className="variant">{renderLyricText(line, "english", line.english, usedColors)}</p>}
            </div>
            <span className={timing?.needsReview ? "review-pill" : "time-pill"}>{timing ? formatMs(timing.startMs) : "Unaligned"}</span>
          </article>
        );
      })}
    </div>
  );
}

function renderLyricText(line: LyricLine, language: LanguageKey, fallback: string, usedColors: Set<string>) {
  const segments = line.segments?.filter((segment) => segment.language === language) ?? [];
  if (!segments.length) {
    return fallback;
  }
  return segments.map((segment, index) => (
    <span key={`${language}-${index}`} style={segmentStyle(segment, usedColors)}>
      {index > 0 ? " " : ""}
      {segment.text}
    </span>
  ));
}

function segmentStyle(segment: LyricSegment, usedColors: Set<string>): React.CSSProperties | undefined {
  const color = readableTextColor(segment.color, usedColors);
  return color ? { color } : undefined;
}

function readableTextColor(color: string | null | undefined, usedColors: Set<string>) {
  const rgb = parseHexColor(color);
  if (!rgb) {
    return null;
  }
  if (contrastRatio(rgb, [255, 255, 255]) >= 4.5) {
    return rgbToHex(rgb);
  }
  const hsl = rgbToHsl(rgb);
  for (const lightness of [0.34, 0.3, 0.26, 0.22, 0.18]) {
    const candidate = hslToRgb([hsl[0], Math.max(hsl[1], 0.45), lightness]);
    const hex = rgbToHex(candidate);
    if (contrastRatio(candidate, [255, 255, 255]) >= 4.5 && !usedColors.has(hex)) {
      return hex;
    }
  }
  return "#26323d";
}

function normalizeHexColor(color: string | null | undefined) {
  const rgb = parseHexColor(color);
  return rgb ? rgbToHex(rgb) : null;
}

function parseHexColor(color: string | null | undefined): [number, number, number] | null {
  if (!color) {
    return null;
  }
  const value = color.trim();
  const short = value.match(/^#([0-9a-f]{3})$/i);
  if (short) {
    return short[1].split("").map((part) => parseInt(part + part, 16)) as [number, number, number];
  }
  const full = value.match(/^#([0-9a-f]{6})$/i);
  if (!full) {
    return null;
  }
  return [0, 2, 4].map((offset) => parseInt(full[1].slice(offset, offset + 2), 16)) as [number, number, number];
}

function rgbToHex(rgb: [number, number, number]) {
  return `#${rgb.map((value) => clampByte(value).toString(16).padStart(2, "0")).join("")}`;
}

function clampByte(value: number) {
  return Math.max(0, Math.min(255, Math.round(value)));
}

function contrastRatio(a: [number, number, number], b: [number, number, number]) {
  const lighter = Math.max(relativeLuminance(a), relativeLuminance(b));
  const darker = Math.min(relativeLuminance(a), relativeLuminance(b));
  return (lighter + 0.05) / (darker + 0.05);
}

function relativeLuminance(rgb: [number, number, number]) {
  const [r, g, b] = rgb.map((value) => {
    const channel = value / 255;
    return channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function rgbToHsl(rgb: [number, number, number]): [number, number, number] {
  const [r, g, b] = rgb.map((value) => value / 255);
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const lightness = (max + min) / 2;
  if (max === min) {
    return [0, 0, lightness];
  }
  const delta = max - min;
  const saturation = lightness > 0.5 ? delta / (2 - max - min) : delta / (max + min);
  const hue = max === r
    ? ((g - b) / delta + (g < b ? 6 : 0)) / 6
    : max === g
      ? ((b - r) / delta + 2) / 6
      : ((r - g) / delta + 4) / 6;
  return [hue, saturation, lightness];
}

function hslToRgb(hsl: [number, number, number]): [number, number, number] {
  const [h, s, l] = hsl;
  if (s === 0) {
    const value = l * 255;
    return [value, value, value];
  }
  const hueToRgb = (p: number, q: number, t: number) => {
    let value = t;
    if (value < 0) value += 1;
    if (value > 1) value -= 1;
    if (value < 1 / 6) return p + (q - p) * 6 * value;
    if (value < 1 / 2) return q;
    if (value < 2 / 3) return p + (q - p) * (2 / 3 - value) * 6;
    return p;
  };
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  return [
    hueToRgb(p, q, h + 1 / 3) * 255,
    hueToRgb(p, q, h) * 255,
    hueToRgb(p, q, h - 1 / 3) * 255,
  ];
}

function AlignmentTable({ lines, alignment, onChange, members, onMemberChange }: { lines: LyricLine[]; alignment: AlignmentLine[]; onChange: (lines: AlignmentLine[]) => void; members: MemberProfile[]; onMemberChange: (lineIndex: number, member: string) => void }) {
  function update(line: AlignmentLine, patch: Partial<AlignmentLine>) {
    onChange(alignment.map((item) => (item.lyricIndex === line.lyricIndex ? { ...item, ...patch, needsReview: patch.needsReview ?? true } : item)));
  }
  return (
    <div className="alignment-table">
      <div className="table-row header">
        <span>Line</span>
        <span>Member</span>
        <span>Start</span>
        <span>End</span>
        <span>Confidence</span>
      </div>
      {lines.map((line) => {
        const timing = alignment.find((item) => item.lyricIndex === line.index) ?? { lyricIndex: line.index, startMs: 0, endMs: 1200, confidence: 0, needsReview: true };
        return (
          <div className="table-row" key={line.index}>
            <span>{line.original}</span>
            <select value={line.member ?? ""} onChange={(event) => onMemberChange(line.index, event.target.value)}>
              <option value="">All</option>
              {members.map((member) => <option key={member.stageName}>{member.stageName}</option>)}
            </select>
            <input type="number" value={timing.startMs} step={100} onChange={(event) => update(timing, { startMs: Number(event.target.value) })} />
            <input type="number" value={timing.endMs} step={100} onChange={(event) => update(timing, { endMs: Number(event.target.value) })} />
            <span>{Math.round(timing.confidence * 100)}% {timing.needsReview ? "review" : ""}</span>
          </div>
        );
      })}
    </div>
  );
}

function mergeMembers(primary: MemberProfile[], secondary: MemberProfile[]) {
  if (primary.length === 0) {
    return secondary;
  }
  const byName = new Map(primary.map((member) => [member.stageName.toLowerCase(), member]));
  for (const member of secondary) {
    const key = member.stageName.toLowerCase();
    const existing = byName.get(key) ?? [...byName.values()].find((item) => namesMatch(item.stageName, member.stageName));
    if (!existing) {
      continue;
    }
    const merged = { ...member, ...existing, imageUrl: existing?.imageUrl ?? member.imageUrl };
    byName.set(existing.stageName.toLowerCase(), merged);
  }
  return [...byName.values()];
}

export function buildJsonExport(metadata: VideoMetadata, songPackage: SongPackage, alignment: AlignmentLine[]) {
  const timings = new Map(alignment.map((line) => [line.lyricIndex, line]));
  return {
    version: 1,
    video: {
      platform: platformFromUrl(metadata.originalUrl),
      videoId: metadata.videoId,
      url: metadata.originalUrl,
    },
    members: songPackage.members.map((member) => ({
      name: member.stageName,
      color: member.color,
      imageUrl: member.imageUrl ?? null,
      localImagePath: member.localImagePath ?? null,
    })),
    lyrics: songPackage.lines.map((line) => {
      const timing = timings.get(line.index);
      return {
        index: line.index,
        startMs: timing?.startMs ?? null,
        endMs: timing?.endMs ?? null,
        layer: line.layer ?? "lead",
        member: line.member ?? null,
        original: line.original,
        ...(line.romanization ? { romanization: line.romanization } : {}),
        ...(line.english ? { english: line.english } : {}),
      };
    }),
  };
}

function platformFromUrl(rawUrl: string) {
  try {
    const host = new URL(rawUrl).hostname.replace(/^www\./, "");
    if (host === "youtube.com" || host === "youtu.be" || host.endsWith(".youtube.com")) {
      return "youtube";
    }
    return host;
  } catch {
    return "unknown";
  }
}

function safeFilename(value: string) {
  return value
    .trim()
    .replace(/[^a-z0-9._-]+/gi, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 60) || "export";
}

function namesMatch(left: string, right: string) {
  const normalize = (value: string) => value.toLowerCase().replace(/kim |huh |hong |miyawaki |nakamura |[^a-z]/g, "");
  const a = normalize(left);
  const b = normalize(right);
  return Boolean(a && b && (a === b || a.includes(b) || b.includes(a)));
}

function queryFromMetadata(metadata: VideoMetadata) {
  return cleanVideoTitle(metadata.title ?? metadata.originalUrl);
}

function cleanVideoTitle(title: string) {
  const cleaned = title
    .replace(/\s+-\s+YouTube$/i, "")
    .replace(/\s*\[[^\]]*(official|mv|m\/v|music video)[^\]]*\]\s*/gi, " ")
    .replace(/\s*\((official\s*)?(mv|m\/v|music video|official video)\)\s*/gi, " ")
    .replace(/\s+(official\s*)?(mv|m\/v|music video|official video)$/i, "")
    .replace(/\s+/g, " ")
    .trim();
  const quotedTitle = cleaned.match(/^(.*?)\s+["“'‘]([^"”'’]+)["”'’]/);
  if (quotedTitle) {
    return `${quotedTitle[1]} ${quotedTitle[2]}`.replace(/\s+/g, " ").trim();
  }
  return cleaned;
}

function shiftAlignment(lines: AlignmentLine[], delta: number) {
  return lines.map((line) => ({ ...line, startMs: Math.max(0, line.startMs + delta), endMs: Math.max(0, line.endMs + delta), needsReview: true }));
}

function formatMs(ms: number) {
  const safe = Math.max(0, ms);
  const minutes = Math.floor(safe / 60000);
  const seconds = Math.floor((safe % 60000) / 1000);
  const millis = String(safe % 1000).padStart(3, "0");
  return `${minutes}:${String(seconds).padStart(2, "0")}.${millis}`;
}
