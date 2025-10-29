#!/usr/bin/env bash
set -euo pipefail

# Merge gauntlet result.json files produced by shards
# Usage:
#   scripts/nnue/merge-gauntlet-json.sh OUT_ROOT [OUT_JSON]
#   or: scripts/nnue/merge-gauntlet-json.sh result1.json result2.json ...

if [ $# -lt 1 ]; then
  echo "usage: $0 OUT_ROOT|result.json [more.json...]" >&2
  exit 1
fi

inputs=()
if [ -d "$1" ]; then
  root="$1"
  while IFS= read -r -d '' f; do inputs+=("$f"); done < <(find "$root" -maxdepth 2 -name result.json -print0 | sort -z)
  out_json="${2:-$root/merged.result.json}"
else
  inputs=("$@")
  out_json="merged.result.json"
fi

if [ ${#inputs[@]} -eq 0 ]; then
  echo "no inputs" >&2
  exit 1
fi

# jq program to sum wins/losses/draws/games; recompute winrate/draw; average nps_delta_pct weighted by games
JQ='def sumfield(f): map(.summary|f) | add; 
    def wavg_nps: (map({g:(.summary.games//0), v:(.summary.nps_delta_pct//0.0)}) 
      | (map(.g)|add) as $tg | if $tg>0 then (map(.g*.v)|add)/$tg else 0 end);
    {winrate: ( (sumfield(.wins) + 0.5*sumfield(.draws)) / (sumfield(.games)+0.0) ),
     draw: (sumfield(.draws) / (sumfield(.games)+0.0)),
     nps_delta_pct: wavg_nps,
     pv_spread_p90_cp: 0,
     gate: "merged",
     wins: sumfield(.wins), losses: sumfield(.losses), draws: sumfield(.draws), games: sumfield(.games),
     nps_samples: (map(.summary.nps_samples//0)|add), pv_spread_samples: (map(.summary.pv_spread_samples//0)|add)}'

jq -s "$JQ" "${inputs[@]}" > "$out_json"
echo "[ok] merged -> $out_json" >&2

