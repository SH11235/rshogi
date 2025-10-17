#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/analysis/analyze_usi_logs.sh

# Analyze USI logs and extract aspiration failures, max depth/seldepth,
# PV head switches, deadline hits, and bestmove.
#
# Usage:
#   scripts/analyze_usi_logs.sh runs/repro/*.log
#   scripts/analyze_usi_logs.sh runs/repro

shopt -s nullglob
files=()
if [[ $# -eq 0 ]]; then
  set -- runs/repro
fi
for arg in "$@"; do
  if [[ -d "$arg" ]]; then
    files+=("$arg"/*.log)
  else
    files+=("$arg")
  fi
done

if [[ ${#files[@]} -eq 0 ]]; then
  echo "no log files found for inputs: $*" >&2
  exit 1
fi

printf "file,threads,run,asp_fail,max_depth,max_seldepth,pv_switches,deadline_soft,deadline_hard,bestmove\n"
for f in "${files[@]}"; do
  base=$(basename "$f")
  threads=""; run=""
  if [[ "$base" =~ baseline_threads([0-9]+)_run([0-9]+)\.log ]]; then
    threads="${BASH_REMATCH[1]}"; run="${BASH_REMATCH[2]}"
  fi

  # robust count: print 0 when no matches
  asp=$(rg -N "aspiration fail-" -n "$f" | wc -l | awk '{print $1}')
  # max depth
  maxd=$(rg -oP "(?<=info depth )\\d+" "$f" | sort -n | tail -1 || true)
  if [[ -z "$maxd" ]]; then maxd=0; fi
  # max seldepth
  maxsd=$(rg -oP "(?<=seldepth )\\d+" "$f" | sort -n | tail -1 || true)
  if [[ -z "$maxsd" ]]; then maxsd=0; fi
  # pv head switches
  pvseq=$(rg -oP "info depth .* pv \K[^ ]+" "$f" || true)
  if [[ -z "$pvseq" ]]; then
    pvsw=0
  else
    pvsw=$(echo "$pvseq" | awk 'BEGIN{last="";c=0}{if(last=="")last=$1;else if($1!=last){c++;last=$1}}END{print c}')
  fi
  dls=$(rg -c "deadline_hit kind=Soft" "$f" || true)
  dlh=$(rg -c "deadline_hit kind=Hard" "$f" || true)
  bm=$(rg -oP "bestmove \K\S+" "$f" | tail -1 || true)
  printf "%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n" "$base" "$threads" "$run" "$asp" "$maxd" "$maxsd" "$pvsw" "$dls" "$dlh" "$bm"
done
