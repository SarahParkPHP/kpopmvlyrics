#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_ID="com.kpopmvlyrics.desktop"
VERSION="$(node -p "require('$ROOT/package.json').version")"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) APPIMAGE_ARCH="x86_64" ;;
  aarch64 | arm64) APPIMAGE_ARCH="aarch64" ;;
  *) APPIMAGE_ARCH="$ARCH" ;;
esac
OUT_DIR="$ROOT/src-tauri/target/release/bundle/appimage"
APPDIR="$OUT_DIR/K-Pop MV Lyrics.AppDir"
APPIMAGE_TOOL="${APPIMAGE_TOOL:-$HOME/.cache/tauri/linuxdeploy-plugin-appimage.AppImage}"
APPIMAGE_TOOL_BASENAME="$(basename "$APPIMAGE_TOOL")"
RAW_APPIMAGE_TOOL="${RAW_APPIMAGE_TOOL:-$OUT_DIR/appimagetool-prefix/usr/bin/appimagetool}"
RAW_APPIMAGE_TOOL_PREFIX="$(dirname "$(dirname "$(dirname "$RAW_APPIMAGE_TOOL")")")"
APPIMAGE_RUNTIME_FILE="${APPIMAGE_RUNTIME_FILE:-$OUT_DIR/runtime-$APPIMAGE_ARCH}"
APPIMAGE_RUNTIME_URL="${APPIMAGE_RUNTIME_URL:-https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-$APPIMAGE_ARCH}"

if [[ ! -x "$APPIMAGE_TOOL" ]]; then
  echo "appimagetool is required. Expected executable at $APPIMAGE_TOOL" >&2
  echo "Run a Tauri AppImage build once, or set APPIMAGE_TOOL=/path/to/appimagetool.AppImage." >&2
  exit 1
fi

cd "$ROOT/src-tauri"
CARGO_INCREMENTAL=0 cargo build --release

rm -rf "$APPDIR"
mkdir -p \
  "$APPDIR/usr/bin" \
  "$APPDIR/usr/lib/kpopmvlyrics" \
  "$APPDIR/usr/share/applications" \
  "$APPDIR/usr/share/doc/kpopmvlyrics" \
  "$APPDIR/usr/share/icons/hicolor/128x128/apps" \
  "$APPDIR/usr/share/metainfo" \
  "$OUT_DIR"

install -m 0755 "$ROOT/src-tauri/target/release/kpopmvlyrics" "$APPDIR/usr/bin/kpopmvlyrics"
install -m 0755 "$ROOT/scripts/run_qwen_asr.py" "$APPDIR/usr/lib/kpopmvlyrics/run_qwen_asr.py"
install -m 0644 "$ROOT/src-tauri/icons/128x128.png" "$APPDIR/usr/share/icons/hicolor/128x128/apps/$APP_ID.png"
install -m 0644 "$ROOT/src-tauri/icons/128x128.png" "$APPDIR/$APP_ID.png"
install -m 0644 "$ROOT/packaging/linux/$APP_ID.metainfo.xml" "$APPDIR/usr/share/metainfo/$APP_ID.metainfo.xml"
install -m 0644 "$ROOT/packaging/linux/asr-dependencies.md" "$APPDIR/usr/share/doc/kpopmvlyrics/ASR.md"
install -m 0644 "$ROOT/requirements-asr.txt" "$APPDIR/usr/share/doc/kpopmvlyrics/requirements-asr.txt"
install -m 0644 "$ROOT/packaging/linux/$APP_ID.desktop" "$APPDIR/$APP_ID.desktop"
install -m 0644 "$ROOT/packaging/linux/$APP_ID.desktop" "$APPDIR/usr/share/applications/$APP_ID.desktop"

cat > "$APPDIR/AppRun" <<'APPRUN'
#!/usr/bin/env bash
set -euo pipefail

HERE="$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")"
export PATH="$HERE/usr/bin:$PATH"
export LD_LIBRARY_PATH="$HERE/usr/lib:$HERE/usr/lib64:${LD_LIBRARY_PATH:-}"
export XDG_DATA_DIRS="$HERE/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
export KPOPMVLYRICS_ASR_SCRIPT="$HERE/usr/lib/kpopmvlyrics/run_qwen_asr.py"
exec "$HERE/usr/bin/kpopmvlyrics" "$@"
APPRUN
chmod +x "$APPDIR/AppRun"

desktop-file-validate "$APPDIR/$APP_ID.desktop"

if [[ "$APPIMAGE_TOOL_BASENAME" != *appimagetool* && ! -x "$RAW_APPIMAGE_TOOL" ]]; then
  for prefix in /tmp/appimage_extracted_*/appimagetool-prefix /tmp/squashfs-root/appimagetool-prefix; do
    if [[ -x "$prefix/usr/bin/appimagetool" ]]; then
      rm -rf "$RAW_APPIMAGE_TOOL_PREFIX"
      mkdir -p "$(dirname "$RAW_APPIMAGE_TOOL_PREFIX")"
      cp -a "$prefix" "$RAW_APPIMAGE_TOOL_PREFIX"
      break
    fi
  done
fi

if [[ "$APPIMAGE_TOOL_BASENAME" == *appimagetool* || -x "$RAW_APPIMAGE_TOOL" ]]; then
  if [[ -x "$RAW_APPIMAGE_TOOL" ]]; then
    APPIMAGE_TOOL="$RAW_APPIMAGE_TOOL"
  fi
  APPIMAGE_TOOL_ARGS=("$APPDIR")
  if [[ ! -f "$APPIMAGE_RUNTIME_FILE" ]]; then
    echo "Downloading AppImage runtime for $APPIMAGE_ARCH"
    if command -v curl >/dev/null 2>&1; then
      curl -L --fail "$APPIMAGE_RUNTIME_URL" -o "$APPIMAGE_RUNTIME_FILE"
    elif command -v wget >/dev/null 2>&1; then
      wget -O "$APPIMAGE_RUNTIME_FILE" "$APPIMAGE_RUNTIME_URL"
    else
      echo "curl or wget is required to download $APPIMAGE_RUNTIME_URL" >&2
      exit 1
    fi
  fi
  APPIMAGE_TOOL_ARGS+=(--runtime-file "$APPIMAGE_RUNTIME_FILE")
else
  APPIMAGE_TOOL_ARGS=(--appdir "$APPDIR")
fi

rm -f "$OUT_DIR/K-Pop_MV_Lyrics-$APPIMAGE_ARCH.AppImage" "$OUT_DIR/K-Pop MV Lyrics_${VERSION}_${APPIMAGE_ARCH}.AppImage"
(
  cd "$OUT_DIR"
  APPIMAGELAUNCHER_DISABLE=1 APPIMAGE_EXTRACT_AND_RUN=1 "$APPIMAGE_TOOL" "${APPIMAGE_TOOL_ARGS[@]}"
)

if [[ -f "$OUT_DIR/K-Pop_MV_Lyrics-$APPIMAGE_ARCH.AppImage" ]]; then
  mv "$OUT_DIR/K-Pop_MV_Lyrics-$APPIMAGE_ARCH.AppImage" "$OUT_DIR/K-Pop MV Lyrics_${VERSION}_${APPIMAGE_ARCH}.AppImage"
fi

echo "Created $OUT_DIR/K-Pop MV Lyrics_${VERSION}_${APPIMAGE_ARCH}.AppImage"
