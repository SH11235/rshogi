#!/usr/bin/env bash
set -euo pipefail
# moved: scripts/utils/quick_usi_once.sh

# Quick one-shot USI search with custom options for a given moves sequence.
# Usage:
#   scripts/quick_usi_once.sh "target/release/engine-usi" "<moves>" 10000 \
#     "FinalizeSanity.Enabled=false,RootSeeGate=false,PostVerify=false,MultiPV=1"

ENG=${1:?engine path}
MOVES=${2:?moves}
BYO=${3:-10000}
OPTS=${4:-}

to_usi_bool() {
  local v=$1
  case "$v" in
    true|on|1|True) echo true ;;
    false|off|0|False) echo false ;;
    *) echo "$v" ;;
  esac
}

sets=""
IFS=',' read -ra kvs <<< "$OPTS"
for kv in "${kvs[@]}"; do
  [[ -z "$kv" ]] && continue
  k="${kv%%=*}"; v="${kv#*=}"
  v=$(to_usi_bool "$v")
  sets+=$'setoption name '"$k"$' value '"$v"$'\n'
done

cat > /tmp/usi_once.in <<EOS
usi
isready
${sets}isready
position startpos moves ${MOVES}
go btime 0 wtime 0 byoyomi ${BYO}
quit
EOS

timeout $(( (BYO/1000) + 20 ))s "$ENG" < /tmp/usi_once.in | tee /tmp/usi_once.out
