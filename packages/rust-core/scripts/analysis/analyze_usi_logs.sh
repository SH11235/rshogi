#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/analysis/analyze_usi_logs.sh

# Analyze USI logs and extract aspiration failures, max depth/seldepth,
# PV head switches, deadline hits, guard diagnostics, and bestmove.
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

printf "file,threads,run,asp_fail,max_depth,max_seldepth,pv_switches,deadline_soft,deadline_hard,quiet_see_skip,root_see_skip,cap_fut_skip,nmp_verify_hits,singular_probe,singular_hit,singular_miss,near_final_attempted,near_final_confirmed,near_final_skip,near_final_result,bestmove\n"
for f in "${files[@]}"; do
  base=$(basename "$f")
  threads=""; run=""
  if [[ "$base" =~ baseline_threads([0-9]+)_run([0-9]+)\.log ]]; then
    threads="${BASH_REMATCH[1]}"; run="${BASH_REMATCH[2]}"
  fi

  # robust count: print 0 when no matches
  asp=$(rg -c --no-filename "aspiration fail-" "$f" || true)
  if [[ -z "$asp" ]]; then asp=0; fi
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
  dls=$(rg -c --no-filename "deadline_hit kind=Soft" "$f" || true)
  if [[ -z "$dls" ]]; then dls=0; fi
  dlh=$(rg -c --no-filename "deadline_hit kind=Hard" "$f" || true)
  if [[ -z "$dlh" ]]; then dlh=0; fi
  qsee=$(rg -c --no-filename "quiet_see_skip" "$f" || true)
  if [[ -z "$qsee" ]]; then qsee=0; fi
  rsee=$(rg -c --no-filename "root_see_skip" "$f" || true)
  if [[ -z "$rsee" ]]; then rsee=0; fi
  capf=$(rg -c --no-filename "cap_fut_skip" "$f" || true)
  if [[ -z "$capf" ]]; then capf=0; fi
  nmpv=$(rg -c --no-filename "nmp_verify" "$f" || true)
  if [[ -z "$nmpv" ]]; then nmpv=0; fi
  nf_start=$(rg -c --no-filename "near_final_zero_window_start=1" "$f" || true)
  if [[ -z "$nf_start" ]]; then nf_start=0; fi
  s_probe=$(rg -c --no-filename "singular_probe" "$f" || true); if [[ -z "$s_probe" ]]; then s_probe=0; fi
  s_hit=$(rg -c --no-filename "singular_hit" "$f" || true); if [[ -z "$s_hit" ]]; then s_hit=0; fi
  s_miss=$(rg -c --no-filename "singular_miss" "$f" || true); if [[ -z "$s_miss" ]]; then s_miss=0; fi
  nf_skip=$(rg -c --no-filename "near_final_zero_window_skip=1" "$f" || true)
  if [[ -z "$nf_skip" ]]; then nf_skip=0; fi
  nf_result=$(rg -c --no-filename "near_final_zero_window_result=1" "$f" || true)
  if [[ -z "$nf_result" ]]; then nf_result=0; fi
  nf_confirm=$(rg -c --no-filename "near_final_zero_window_result=1 status=confirmed" "$f" || true)
  if [[ -z "$nf_confirm" ]]; then nf_confirm=0; fi
  bm=$(rg -oP "bestmove \\K\\S+" "$f" | tail -1 || true)
  printf "%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n" \
    "$base" "$threads" "$run" "$asp" "$maxd" "$maxsd" "$pvsw" "$dls" "$dlh" "$qsee" "$rsee" "$capf" "$nmpv" "$s_probe" "$s_hit" "$s_miss" "$nf_start" "$nf_confirm" "$nf_skip" "$nf_result" "$bm"
done
