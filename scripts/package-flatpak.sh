#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_ID="com.kpopmvlyrics.desktop"
VERSION="$(node -p "require('$ROOT/package.json').version")"
FLATPAK_DIR="$ROOT/src-tauri/target/release/bundle/flatpak"
STAGE="$FLATPAK_DIR/stage"
BUILD_DIR="$FLATPAK_DIR/build"
REPO_DIR="$FLATPAK_DIR/repo"
MANIFEST="$FLATPAK_DIR/$APP_ID.yml"

if ! command -v flatpak-builder >/dev/null 2>&1; then
  echo "flatpak-builder is required to build the Flatpak bundle" >&2
  exit 1
fi

if ! flatpak remotes --user --columns=name | grep -qx "flathub"; then
  flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
fi

cd "$ROOT/src-tauri"
CARGO_INCREMENTAL=0 cargo build --release

rm -rf "$STAGE" "$BUILD_DIR" "$REPO_DIR"
mkdir -p \
  "$STAGE/bin" \
  "$STAGE/lib/kpopmvlyrics" \
  "$STAGE/share/applications" \
  "$STAGE/share/doc/kpopmvlyrics" \
  "$STAGE/share/icons/hicolor/128x128/apps" \
  "$STAGE/share/metainfo" \
  "$FLATPAK_DIR"

install -m 0755 "$ROOT/src-tauri/target/release/kpopmvlyrics" "$STAGE/bin/kpopmvlyrics"
install -m 0755 "$ROOT/scripts/run_qwen_asr.py" "$STAGE/lib/kpopmvlyrics/run_qwen_asr.py"
install -m 0644 "$ROOT/src-tauri/icons/128x128.png" "$STAGE/share/icons/hicolor/128x128/apps/$APP_ID.png"
install -m 0644 "$ROOT/packaging/linux/$APP_ID.desktop" "$STAGE/share/applications/$APP_ID.desktop"
install -m 0644 "$ROOT/packaging/linux/$APP_ID.metainfo.xml" "$STAGE/share/metainfo/$APP_ID.metainfo.xml"
install -m 0644 "$ROOT/packaging/linux/asr-dependencies.md" "$STAGE/share/doc/kpopmvlyrics/ASR.md"
install -m 0644 "$ROOT/requirements-asr.txt" "$STAGE/share/doc/kpopmvlyrics/requirements-asr.txt"

cat > "$MANIFEST" <<EOF_MANIFEST
app-id: $APP_ID
runtime: org.gnome.Platform
runtime-version: '50'
sdk: org.gnome.Sdk
command: kpopmvlyrics
finish-args:
  - --share=network
  - --share=ipc
  - --socket=wayland
  - --socket=fallback-x11
  - --device=dri
  - --filesystem=xdg-download
  - --talk-name=org.freedesktop.portal.Desktop
  - --env=GST_PLUGIN_SYSTEM_PATH=/usr/lib/gstreamer-1.0
modules:
  - name: kpopmvlyrics
    buildsystem: simple
    build-commands:
      - install -Dm0755 bin/kpopmvlyrics /app/bin/kpopmvlyrics
      - install -Dm0755 lib/kpopmvlyrics/run_qwen_asr.py /app/lib/kpopmvlyrics/run_qwen_asr.py
      - install -Dm0644 share/applications/$APP_ID.desktop /app/share/applications/$APP_ID.desktop
      - install -Dm0644 share/doc/kpopmvlyrics/ASR.md /app/share/doc/kpopmvlyrics/ASR.md
      - install -Dm0644 share/doc/kpopmvlyrics/requirements-asr.txt /app/share/doc/kpopmvlyrics/requirements-asr.txt
      - install -Dm0644 share/icons/hicolor/128x128/apps/$APP_ID.png /app/share/icons/hicolor/128x128/apps/$APP_ID.png
      - install -Dm0644 share/metainfo/$APP_ID.metainfo.xml /app/share/metainfo/$APP_ID.metainfo.xml
    sources:
      - type: dir
        path: stage
EOF_MANIFEST

flatpak-builder --user --install-deps-from=flathub --disable-rofiles-fuse --force-clean --repo="$REPO_DIR" "$BUILD_DIR" "$MANIFEST"
flatpak build-bundle "$REPO_DIR" "$FLATPAK_DIR/$APP_ID-$VERSION.flatpak" "$APP_ID"
echo "Created $FLATPAK_DIR/$APP_ID-$VERSION.flatpak"
