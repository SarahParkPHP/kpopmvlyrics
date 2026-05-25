#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_ID="com.kpopmvlyrics.desktop"
APP_NAME="K-Pop MV Lyrics"
VERSION="$(node -p "require('$ROOT/package.json').version")"
ARCH="$(uname -m)"
OUT_DIR="$ROOT/src-tauri/target/release/bundle/tar"
STAGE="$OUT_DIR/$APP_ID-$VERSION-$ARCH"

cd "$ROOT"
CARGO_INCREMENTAL=0 npm run tauri -- build --no-bundle

rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/share/applications" "$STAGE/share/icons/hicolor/128x128/apps" "$STAGE/share/metainfo"

install -m 0755 "$ROOT/src-tauri/target/release/kpopmvlyrics" "$STAGE/bin/kpopmvlyrics"
install -m 0644 "$ROOT/src-tauri/icons/128x128.png" "$STAGE/share/icons/hicolor/128x128/apps/$APP_ID.png"
install -m 0644 "$ROOT/packaging/linux/$APP_ID.desktop" "$STAGE/share/applications/$APP_ID.desktop"
install -m 0644 "$ROOT/packaging/linux/$APP_ID.metainfo.xml" "$STAGE/share/metainfo/$APP_ID.metainfo.xml"
install -m 0644 "$ROOT/README.md" "$STAGE/README.md"

cat > "$STAGE/install.sh" <<'INSTALL'
#!/usr/bin/env bash
set -euo pipefail

PREFIX="${PREFIX:-/usr/local}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

install -Dm0755 "$ROOT/bin/kpopmvlyrics" "$PREFIX/bin/kpopmvlyrics"
install -Dm0644 "$ROOT/share/applications/com.kpopmvlyrics.desktop.desktop" "$PREFIX/share/applications/com.kpopmvlyrics.desktop.desktop"
install -Dm0644 "$ROOT/share/icons/hicolor/128x128/apps/com.kpopmvlyrics.desktop.png" "$PREFIX/share/icons/hicolor/128x128/apps/com.kpopmvlyrics.desktop.png"
install -Dm0644 "$ROOT/share/metainfo/com.kpopmvlyrics.desktop.metainfo.xml" "$PREFIX/share/metainfo/com.kpopmvlyrics.desktop.metainfo.xml"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$PREFIX/share/applications" || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache "$PREFIX/share/icons/hicolor" || true
fi
INSTALL
chmod +x "$STAGE/install.sh"

tar -C "$OUT_DIR" -czf "$OUT_DIR/$APP_ID-$VERSION-$ARCH.tar.gz" "$APP_ID-$VERSION-$ARCH"
echo "Created $OUT_DIR/$APP_ID-$VERSION-$ARCH.tar.gz"
