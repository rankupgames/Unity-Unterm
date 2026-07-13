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
  cargo build -p unterm --locked ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} --target "$arch"
done

DEST="../Packages/dev.tnayuki.unterm/Editor/Plugins/macOS/unterm.dylib"
mkdir -p "$(dirname "$DEST")"

LIBS=()
for arch in "${ARCHS[@]}"; do
  LIBS+=("target/$arch/${TARGET_DIR}/libunterm.dylib")
done

echo "==> lipo -> $DEST"
lipo -create "${LIBS[@]}" -output "$DEST"

echo "==> done: $DEST"
lipo -info "$DEST"
