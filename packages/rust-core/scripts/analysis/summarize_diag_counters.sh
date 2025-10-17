#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/analysis/summarize_diag_counters.sh

# Summarize diagnostic counters from runs/repro/*.log
# Prints counts for tags emitted under feature=diagnostics

DIR=${1:-runs/repro}

shopt -s nullglob
files=($DIR/*.log)
if [[ ${#files[@]} -eq 0 ]]; then
  echo "no log files under $DIR" >&2
  exit 0
fi

tags=(
  pc_qs_gate
  pc_verif_tried
  pc_cut_hit
  razor_triggered
  lmr_fullwin_re
  hp_skip_d1
  hp_skip_d2
  hp_skip_d3
)

printf "%-40s %8s\n" "tag" "count"
printf "%-40s %8s\n" "----------------------------------------" "--------"
for t in "${tags[@]}"; do
  c=$(rg -N "\\[diag\\]|\\[trace\\]|${t}" -n $DIR/*.log | rg -c "${t}" || true)
  # rg -c across multiple files prints one total per file when used with -N pattern; accumulate with awk
  if [[ -z "$c" ]]; then
    total=0
  else
    total=$(rg -N "${t}" -n $DIR/*.log | wc -l | awk '{print $1}')
  fi
  printf "%-40s %8d\n" "$t" "$total"
done
