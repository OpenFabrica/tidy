#!/usr/bin/env bash
# Build mole-helper in release mode and stage it as the Tauri sidecar
# (src-tauri/binaries/mole-helper-<host-triple>). Tauri copies it next to the
# app binary in dev (target/debug/) and into Contents/MacOS in the bundle,
# where the GUI launches it as `mole-helper fan-daemon`.
set -euo pipefail
cd "$(dirname "$0")/.."   # desktop/

TRIPLE="${1:-$(rustc -vV | sed -n 's/host: //p')}"
mkdir -p src-tauri/binaries

if [ "$TRIPLE" = "universal-apple-darwin" ]; then
  # Universal release: the tauri CLI compiles the app once per architecture
  # (each build.rs wants its own per-arch sidecar) and then bundles with the
  # universal triple (wanting a lipo'd sidecar), so stage all three.
  for arch in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release -p mole-helper --target "$arch"
    cp "target/$arch/release/mole-helper" "src-tauri/binaries/mole-helper-$arch"
    echo "▶ sidecar: src-tauri/binaries/mole-helper-$arch"
  done
  lipo -create -output "src-tauri/binaries/mole-helper-$TRIPLE" \
    target/aarch64-apple-darwin/release/mole-helper \
    target/x86_64-apple-darwin/release/mole-helper
  echo "▶ sidecar: src-tauri/binaries/mole-helper-$TRIPLE"
else
  cargo build --release -p mole-helper
  cp target/release/mole-helper "src-tauri/binaries/mole-helper-$TRIPLE"
  echo "▶ sidecar: src-tauri/binaries/mole-helper-$TRIPLE"
fi
