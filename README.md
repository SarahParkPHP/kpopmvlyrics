# K-Pop MV Lyrics

Desktop app for playing a YouTube MV while showing synced, member-colored lyrics.

## Run

### Linux (native GTK + GStreamer)

Linux builds use a **native GTK UI** and GStreamer video pane — no WebKit, no npm frontend, native Wayland support.

Install dependencies:

```bash
# Arch / CachyOS
sudo pacman -S gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-libav gst-plugin-gtk gtk3

# Debian / Ubuntu
sudo apt install gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad gstreamer1.0-libav gstreamer1.0-gl libgtk-3-dev
```

YouTube stream resolution requires **yt-dlp** on your PATH.

Build and run:

```bash
cd src-tauri
cargo build --release
./target/release/kpopmvlyrics
```

### Other native frontends

Experimental native frontends live under `apps/` for macOS, Windows WinUI, and KDE/Qt.
Install [GStreamer runtime](https://gstreamer.freedesktop.org/download/) for native video playback on those platforms.

## Test

```bash
npm test
cd src-tauri && cargo test
```

## Package

Debian/Ubuntu `.deb`, Fedora/openSUSE-style `.rpm`, and AppImage bundles:

```bash
npm run package:deb-rpm
npm run package:appimage
```

Arch/CachyOS tarball and Flatpak:

```bash
npm run package:tar
npm run package:flatpak
```

All Linux packages:

```bash
npm run package:linux
```

Native Linux runtime packages are GTK 4, GStreamer playback plugins,
GDK Pixbuf, `yt-dlp`, and CA certificates. ASR/STT is optional: install
Python, FFmpeg, and the packages in `requirements-asr.txt` via
`scripts/setup-asr.sh` for local Qwen ASR and optional Demucs vocal stem
separation. External STT providers use the bundled Python bridge plus the API
key entered in app settings.

## Current Capabilities

- YouTube URL resolution via yt-dlp with native GStreamer playback (progressive 360p or adaptive HD streams).
- Split-pane layout: lyrics, member strip, and controls (top), native video surface (bottom).
- Rust backend for lyric fetching/import, caption fetching/import, alignment, member profile search, and SQLite persistence.
- ColorCodedLyrics-first provider with Genius fallback.
- Best-effort YouTube caption discovery.
- Fuzzy caption-to-lyric alignment with review flags.
- Member-colored lyric lines with original / romanization / English toggles and sync playback.

Live scrapers are intentionally best-effort because provider markup and access rules can change.
