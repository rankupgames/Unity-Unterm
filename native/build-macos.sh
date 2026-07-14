#!/usr/bin/env bash
# Build the Unterm native terminal and install it as a Unity macOS plugin.
#
# Unity accepts a `.dylib` as a native plugin (and, unlike a flat Mach-O renamed
# `.bundle`, recognizes it as a real plugin so it runs UnityPluginLoad). We lipo
# the per-arch cdylibs into a universal `.dylib` under the package's Plugins dir.
set -euo pipefail

cd "$(dirname "$0")"

# Keep host paths out of debug information and derive timestamps from the source
# commit so repeated builds of the same revision have stable inputs.
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C .. log -1 --pretty=%ct)}"
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$(pwd)=."

PROFILE="${1:-release}"
case "$PROFILE" in
  release) CARGO_FLAGS=(--release); TARGET_DIR="release" ;;
  debug)   CARGO_FLAGS=();          TARGET_DIR="debug"   ;;
  *) echo "usage: $0 [release|debug]" >&2; exit 1 ;;
esac

# Universal binary so the plugin runs on Apple Silicon and Intel editors.
ARCHS=(aarch64-apple-darwin x86_64-apple-darwin)
for arch in "${ARCHS[@]}"; do
  rustup target add "$arch"
done

echo "==> building unterm ($PROFILE)"
for arch in "${ARCHS[@]}"; do
  # Bash 3.2 (macOS) treats "${CARGO_FLAGS[@]}" as unbound under `set -u` when the
  # array is empty (the debug profile), so expand it guardedly.
  cargo build -p unterm --locked --lib --bin unterm-debugger \
    ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} --target "$arch"
done

PLUGIN_DIR="../Packages/dev.tnayuki.unterm/Editor/Plugins/macOS"
LIB_DEST="$PLUGIN_DIR/unterm.dylib"
DEBUGGER_DEST="$PLUGIN_DIR/unterm-debugger"
mkdir -p "$PLUGIN_DIR"

LIBS=()
DEBUGGERS=()
for arch in "${ARCHS[@]}"; do
  LIBS+=("target/$arch/${TARGET_DIR}/libunterm.dylib")
  DEBUGGERS+=("target/$arch/${TARGET_DIR}/unterm-debugger")
done

echo "==> lipo library -> $LIB_DEST"
lipo -create "${LIBS[@]}" -output "$LIB_DEST"

echo "==> lipo debugger -> $DEBUGGER_DEST"
lipo -create "${DEBUGGERS[@]}" -output "$DEBUGGER_DEST"
chmod 0755 "$DEBUGGER_DEST"
# Lipo invalidates per-architecture Mach-O signatures. Re-sign the universal
# executable deterministically (ad-hoc, no timestamp) before publishing it.
codesign --force --sign - --timestamp=none \
  --identifier dev.tnayuki.unterm.debugger "$DEBUGGER_DEST"
codesign --verify --strict --verbose=2 "$DEBUGGER_DEST"

echo "==> done: $LIB_DEST"
lipo -info "$LIB_DEST"
echo "==> done: $DEBUGGER_DEST"
lipo -info "$DEBUGGER_DEST"
test -x "$DEBUGGER_DEST"
