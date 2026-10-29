#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/dogfood-build.sh [--link] [--dry-run]

Build the c9 engine dogfood binary (plan C1a-17, D28 Q8=A): dev channel,
release profile, no bundle, `engine` feature. The main binary has the GUI
(with the dev-only shadow diff), the CLI and the engine; c9watch-agent sits
next to it, where the engine copies it from (C1a-21).

Steps:
  1. Main binary:  npm run tauri build -- --no-bundle --features engine
                   -> <target>/release/c9watch
  2. Agent binary: cargo build --release --features engine --bin c9watch-agent
                   -> <target>/release/c9watch-agent
  3. Symlink:      ~/.local/bin/c9watch -> <target>/release/c9watch,
                   only with --link. Without it, the script prints what it
                   would change.

Options:
  --link       Point ~/.local/bin/c9watch at the new binary.
  --dry-run    Print the commands and the symlink change; build nothing.
  -h, --help   Show this help.

<target> is $CARGO_TARGET_DIR when set, else src-tauri/target.

Run the binary directly (`<target>/release/c9watch`); do not install it in
/Applications. The script never launches it. A dev channel GUI does not run
the update check on launch, so a release does not replace the dogfood build.

Why no bundle: `tauri build` with a bundle also makes updater artifacts
(`createUpdaterArtifacts` in tauri.conf.json), which need the signing key.

Verified 2026-10-30 on macOS with tauri-cli 2: see VERIFIED below.
EOF
}

# The commands as verified on M3 day one. Keep in sync with the plan.
# VERIFIED 2026-10-30: `npm run tauri build -- --no-bundle --features engine`
# passes `--no-bundle` and `--features` through tauri v2, needs no signing
# key, and builds <target>/release/c9watch with the engine and the dev-only
# shadow diff (`c9watch engine --help` lists `shadow-report`).
# C1a-21 (M4) added the second [[bin]], c9watch-agent (required-features =
# ["engine"], default-run = "c9watch"). `cargo check --features engine` of
# the GUI passes with it; the release `tauri build` with both binaries is
# not re-run yet. Step 2 builds the agent explicitly, so the script does not
# depend on whether `tauri build` also builds it: when it did, step 2 is a
# no-op. The engine copies `c9watch-agent` from the directory of
# realpath(~/.local/bin/c9watch), so both must land in <target>/release.
MAIN_BUILD=(npm run tauri build -- --no-bundle --features engine)
AGENT_BUILD=(cargo build --release --manifest-path src-tauri/Cargo.toml
  --features engine --bin c9watch-agent)

link=false
dry_run=false
while (($# > 0)); do
  case "$1" in
    --link) link=true ;;
    --dry-run) dry_run=true ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo"

# Dogfood is the dev channel: build.rs turns on cfg(c9_channel_dev) unless
# C9WATCH_CHANNEL=prod.
if [[ -n "${C9WATCH_CHANNEL:-}" ]]; then
  echo "C9WATCH_CHANNEL is set (${C9WATCH_CHANNEL}); unset it for a dev channel build." >&2
  exit 1
fi

target_dir="${CARGO_TARGET_DIR:-$repo/src-tauri/target}"
main_bin="$target_dir/release/c9watch"
agent_bin="$target_dir/release/c9watch-agent"
link_path="$HOME/.local/bin/c9watch"

run() {
  echo "+ $*"
  if ! $dry_run; then
    "$@"
  fi
}

# A release build adds several GB to the target dir.
if ! $dry_run && command -v disk-guard >/dev/null 2>&1; then
  disk-guard 7
fi

if [[ ! -d node_modules ]]; then
  run npm ci
fi

echo "== 1/3 main binary (dev channel, release profile, engine)"
run "${MAIN_BUILD[@]}"
if ! $dry_run; then
  test -x "$main_bin" || {
    echo "Expected $main_bin after the build." >&2
    exit 1
  }
fi

echo "== 2/3 c9watch-agent"
run "${AGENT_BUILD[@]}"
if ! $dry_run; then
  test -x "$agent_bin" || {
    echo "Expected $agent_bin after the build." >&2
    exit 1
  }
fi

echo "== 3/3 ~/.local/bin/c9watch"
current="(none)"
if [[ -L "$link_path" ]]; then
  current="$(readlink "$link_path")"
elif [[ -e "$link_path" ]]; then
  current="(a regular file, not a symlink)"
fi
echo "now:  $link_path -> $current"
echo "new:  $link_path -> $main_bin"
if ! $link; then
  echo "not changed: pass --link to update the symlink."
elif [[ -e "$link_path" && ! -L "$link_path" ]]; then
  echo "refused: $link_path is not a symlink; move it away first." >&2
  exit 1
else
  run mkdir -p "$(dirname "$link_path")"
  run ln -sfn "$main_bin" "$link_path"
fi

echo
echo "Done. Run the binary directly: $main_bin"
echo "Agent binary: $agent_bin (the engine copies it at start)"
