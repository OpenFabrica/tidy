#!/usr/bin/env bash
# Build mole-helper in release mode and stage it as the Tauri sidecar
# (src-tauri/binaries/mole-helper-<host-triple>). Tauri copies it next to the
# app binary in dev (target/debug/) and into Contents/MacOS in the bundle,
# where the GUI launches it as `mole-helper fan-daemon`.
set -euo pipefail
cd "$(dirname "$0")/.."   # desktop/

TRIPLE="${1:-$(rustc -vV | sed -n 's/host: //p')}"
mkdir -p src-tauri/binaries
OUT="src-tauri/binaries/mole-helper-$TRIPLE"

if [ "$TRIPLE" = "universal-apple-darwin" ]; then
  # Universal release: build both slices and lipo them, mirroring what the
  # tauri CLI does for the main binary (needs both rustup targets installed).
  for arch in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release -p mole-helper --target "$arch"
  done
  lipo -create -output "$OUT" \
    target/aarch64-apple-darwin/release/mole-helper \
    target/x86_64-apple-darwin/release/mole-helper
else
  cargo build --release -p mole-helper
  cp target/release/mole-helper "$OUT"
fi
echo "▶ sidecar: $OUT"
