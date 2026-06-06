#!/usr/bin/env bash
#
# Build the Rust core as a universal static library and (re)generate the Swift
# UniFFI bindings the Xcode project links against.
#
# Usage:  Scripts/build-rust.sh [debug|release]
#
# Run from anywhere; paths are resolved relative to the repo.
set -euo pipefail

PROFILE="${1:-debug}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MACOS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$MACOS_DIR/.." && pwd)"
CRATE_DIR="$REPO_ROOT/src-tauri"
GEN_DIR="$MACOS_DIR/Generated"
LIB_DIR="$MACOS_DIR/Libs"
LIB_NAME="libkpopmvlyrics_lib"

ARCHS=("aarch64-apple-darwin" "x86_64-apple-darwin")
CARGO_FLAGS=(--features macos_ffi)
if [[ "$PROFILE" == "release" ]]; then
    CARGO_FLAGS+=(--release)
    TARGET_SUBDIR="release"
else
    TARGET_SUBDIR="debug"
fi

mkdir -p "$GEN_DIR" "$LIB_DIR"

echo "==> Building Rust core ($PROFILE) for: ${ARCHS[*]}"
STATIC_LIBS=()
for arch in "${ARCHS[@]}"; do
    if ! rustup target list --installed | grep -q "^$arch$"; then
        echo "    installing rust target $arch"
        rustup target add "$arch"
    fi
    ( cd "$CRATE_DIR" && cargo build "${CARGO_FLAGS[@]}" --target "$arch" )
    STATIC_LIBS+=("$CRATE_DIR/target/$arch/$TARGET_SUBDIR/$LIB_NAME.a")
done

echo "==> Creating universal static library"
lipo -create "${STATIC_LIBS[@]}" -output "$LIB_DIR/$LIB_NAME.a"

# uniffi-bindgen reads metadata from a built library; the host-arch dylib works.
HOST_ARCH="$(uname -m)"
case "$HOST_ARCH" in
    arm64) HOST_TARGET="aarch64-apple-darwin" ;;
    x86_64) HOST_TARGET="x86_64-apple-darwin" ;;
    *) HOST_TARGET="aarch64-apple-darwin" ;;
esac
HOST_DYLIB="$CRATE_DIR/target/$HOST_TARGET/$TARGET_SUBDIR/$LIB_NAME.dylib"

echo "==> Generating Swift bindings"
( cd "$CRATE_DIR" && cargo run "${CARGO_FLAGS[@]}" --example uniffi-bindgen -- \
    generate --library "$HOST_DYLIB" --language swift --out-dir "$GEN_DIR" )

# Swift's clang importer discovers a module named `module.modulemap` on the
# include path; rename UniFFI's `<ns>FFI.modulemap` so `import ...FFI` resolves.
if [[ -f "$GEN_DIR/${LIB_NAME}FFI.modulemap" ]]; then
    mv -f "$GEN_DIR/${LIB_NAME}FFI.modulemap" "$GEN_DIR/module.modulemap"
fi

echo "==> Done."
echo "    static lib : $LIB_DIR/$LIB_NAME.a"
echo "    bindings   : $GEN_DIR"
