# K-Pop MV Lyrics

Cross-platform Tauri v2 desktop app for playing a YouTube MV while showing synced, member-colored lyrics.

## Run

```bash
npm install
npm run tauri dev
```

For frontend-only iteration:

```bash
npm run dev
```

## Test

```bash
npm test
cd src-tauri && cargo test
```

## Package

```bash
npm run package:linux
```

This builds:

- `.deb` in `src-tauri/target/release/bundle/deb/`
- `.rpm` in `src-tauri/target/release/bundle/rpm/`
- portable `.tar.gz` in `src-tauri/target/release/bundle/tar/`
- `.flatpak` in `src-tauri/target/release/bundle/flatpak/`

The tarball is intended for Arch/Cachy-style installs and includes an `install.sh` that respects `PREFIX`.

## Current Capabilities

- YouTube URL resolution and embedded IFrame Player API playback.
- Rust commands for lyric fetching/import, caption fetching/import, alignment, member profile search, and override persistence.
- SQLite cache for songs, lyric lines, captions, alignments, members, provider metadata, and user overrides.
- ColorCodedLyrics-first provider with Genius fallback, plus manual lyric import.
- Best-effort public YouTube caption discovery, plus VTT/SRT/YouTube JSON manual import.
- Fuzzy caption-to-lyric alignment with interpolation and review flags for low confidence lines.
- React lyric stage with original/romanization/English toggles, active member highlighting, timing editor, global shift controls, member assignment, and local image picker.

Live scrapers are intentionally best-effort because provider markup and access rules can change. Manual import and local edits are part of the normal workflow.
