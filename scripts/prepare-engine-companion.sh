#!/bin/sh
# Run from the repo root before the release Tauri build. Tauri externalBin
# puts the target-suffixed source next to c9watch, under its unsuffixed name,
# and signs it with the rest of the native bundle.
set -eu
target=${1:?usage: prepare-engine-companion.sh target-triple}
cd src-tauri
cargo build --release --no-default-features --features engine --bin c9watch-agent --target "$target"
mkdir -p binaries
build_target=${CARGO_TARGET_DIR:-target}
cp "$build_target/$target/release/c9watch-agent" "binaries/c9watch-agent-$target"
