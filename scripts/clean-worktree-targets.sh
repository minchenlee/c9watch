#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/clean-worktree-targets.sh [--apply] [--older-than-hours N]

Find Cargo build targets belonging to registered c9watch worktrees other than
the current worktree. The default mode is a dry run.

Options:
  --apply                 Run cargo clean for eligible targets.
  --older-than-hours N   Only include targets at least N hours old (default: 24).
  -h, --help             Show this help.

The script never removes a worktree or source file. It only targets the exact
src-tauri/target directory and skips symlinks. Stop active Cargo/Tauri builds
before using --apply.
EOF
}

apply=false
older_than_hours=24

while (($# > 0)); do
  case "$1" in
    --apply)
      apply=true
      ;;
    --older-than-hours)
      if (($# < 2)); then
        echo "--older-than-hours requires a non-negative integer" >&2
        exit 2
      fi
      older_than_hours="$2"
      shift
      if [[ ! "$older_than_hours" =~ ^[0-9]+$ ]]; then
        echo "--older-than-hours requires a non-negative integer" >&2
        exit 2
      fi
      ;;
    -h|--help)
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

repo_root="$(git rev-parse --show-toplevel)"
repo_root="$(cd "$repo_root" && pwd -P)"
now="$(date +%s)"
found=0

mtime_for() {
  case "$(uname -s)" in
    Darwin)
      stat -f %m "$1"
      ;;
    *)
      stat -c %Y "$1"
      ;;
  esac
}

echo "Scanning registered c9watch worktrees (excluding $repo_root)"

while IFS= read -r worktree; do
  [[ -n "$worktree" ]] || continue
  [[ -d "$worktree" ]] || continue

  worktree="$(cd "$worktree" && pwd -P)"
  [[ "$worktree" == "$repo_root" ]] && continue

  manifest="$worktree/src-tauri/Cargo.toml"
  target="$worktree/src-tauri/target"
  [[ -f "$manifest" ]] || continue

  if [[ -L "$target" ]]; then
    echo "skip symlink: $target"
    continue
  fi
  [[ -d "$target" ]] || continue

  modified_at="$(mtime_for "$target")"
  age_seconds=$((now - modified_at))
  ((age_seconds < 0)) && age_seconds=0
  age_hours=$((age_seconds / 3600))
  if ((age_hours < older_than_hours)); then
    echo "skip recent target (${age_hours}h < ${older_than_hours}h): $target"
    continue
  fi

  found=1
  size="$(du -sh "$target" | awk '{print $1}')"
  if [[ "$apply" == true ]]; then
    echo "cleaning $size: $target"
    CARGO_TARGET_DIR="$target" cargo clean --manifest-path "$manifest"
  else
    echo "would clean $size: $target"
  fi
done < <(git worktree list --porcelain | awk '/^worktree / { sub(/^worktree /, ""); print }')

if ((found == 0)); then
  echo "No eligible Cargo targets found."
elif [[ "$apply" == false ]]; then
  echo "Dry run only; re-run with --apply to clean the listed targets."
fi
