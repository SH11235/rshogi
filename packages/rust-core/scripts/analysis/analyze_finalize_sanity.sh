#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/analysis/analyze_finalize_sanity.sh

# Analyze finalize sanity related logs.
# Usage:
#   scripts/analyze_finalize_sanity.sh taikyoku_log_*.md
#   scripts/analyze_finalize_sanity.sh runs/repro

shopt -s nullglob
files=()
if [[ $# -eq 0 ]]; then
  set -- runs/repro
fi
for arg in "$@"; do
  if [[ -d "$arg" ]]; then
    files+=("$arg"/*.log "$arg"/*.md)
  else
    files+=("$arg")
  fi
done

if [[ ${#files[@]} -eq 0 ]]; then
  echo "no log files found for inputs: $*" >&2
  exit 1
fi

printf "file,sanity_switch,no_publish,king_alt_blocked,king_alt_reselect,pv1_is_king,postverify_reject,mate_switch,finalize_joined,finalize_fast\n"
for f in "${files[@]}"; do
  # counts (robust to no-match)
  sw=$(rg -c "sanity_switch=1" "$f" || true)
  np=$(rg -c "no_publish=1" "$f" || true)
  kab=$(rg -c "sanity_king_alt_blocked=1" "$f" || true)
  kar=$(rg -c "sanity_alt_reselect_nonking=1|sanity_pv1_king_alt_reselect_nonking=1" "$f" || true)
  pvk=$(rg -c "sanity_pv1_is_king=1" "$f" || true)
  pvr=$(rg -c "mate_postverify_reject=1" "$f" || true)
  msw=$(rg -c "mate_switch=1" "$f" || true)
  fj=$(rg -c "finalize_event label=finalize mode=joined" "$f" || true)
  ff=$(rg -c "finalize_event label=finalize mode=cached|_fast_" "$f" || true)
  echo "$(basename "$f"),${sw:-0},${np:-0},${kab:-0},${kar:-0},${pvk:-0},${pvr:-0},${msw:-0},${fj:-0},${ff:-0}"
done
