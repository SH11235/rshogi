#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: assert_output_root.sh REPO OUT_DIR" >&2
  exit 2
fi

repo="$(realpath -e "$1")"
if [[ "$2" != /* ]]; then
  echo "selfplay output gate: --out-dir must be an absolute path; got $2" >&2
  exit 2
fi
out_dir="$2"
mapfile -t worktrees < <(
  git -c "safe.directory=$repo" -C "$repo" worktree list --porcelain |
    sed -n 's/^worktree //p'
)

if [ "${#worktrees[@]}" -lt 1 ]; then
  echo "selfplay output gate: git reported no worktrees" >&2
  exit 2
fi
canonical_raw="${worktrees[0]}"
if [ "$(basename "$canonical_raw")" != "rshogi" ]; then
  echo "selfplay output gate: primary worktree must be named 'rshogi'; got $canonical_raw" >&2
  exit 2
fi
if [ ! -d "$canonical_raw" ] || [ -L "$canonical_raw" ]; then
  echo "selfplay output gate: primary worktree must be an existing real directory: $canonical_raw" >&2
  exit 2
fi
canonical="$(realpath -e "$canonical_raw")"

for persistent_path in "$canonical/runs" "$canonical/runs/selfplay"; do
  if [ ! -d "$persistent_path" ] || [ -L "$persistent_path" ]; then
    echo "selfplay output gate: persistent path must be an existing real directory: $persistent_path" >&2
    exit 2
  fi
done
output_root="$(realpath -e "$canonical/runs/selfplay")"
out_parent="$(realpath -e "$(dirname "$out_dir")")"
if [ "$out_parent" != "$output_root" ]; then
  echo "selfplay output gate: --out-dir must be a direct child of $output_root; got $out_dir" >&2
  exit 2
fi
if [ -e "$out_dir" ] || [ -L "$out_dir" ]; then
  echo "selfplay output gate: run directory must not exist before kick: $out_dir" >&2
  exit 2
fi

run_name="$(basename "$out_dir")"
if [[ ! "$run_name" =~ ^[0-9]{8}-[0-9]{6}-[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
  echo "selfplay output gate: run directory must match YYYYMMDD-HHMMSS-PURPOSE; got '$run_name'" >&2
  exit 2
fi
timestamp="${run_name:0:15}"
date_input="${timestamp:0:4}-${timestamp:4:2}-${timestamp:6:2} ${timestamp:9:2}:${timestamp:11:2}:${timestamp:13:2}"
normalized_timestamp="$(date -d "$date_input" +%Y%m%d-%H%M%S || true)"
if [ "$normalized_timestamp" != "$timestamp" ]; then
  echo "selfplay output gate: invalid timestamp '$timestamp'" >&2
  exit 2
fi

echo "canonical_rshogi=$canonical"
echo "selfplay_out=$out_dir"
