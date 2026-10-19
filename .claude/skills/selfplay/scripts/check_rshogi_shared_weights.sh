#!/usr/bin/env bash
# usage: check_rshogi_shared_weights.sh OUT_DIR SIDE
# Fails unless every stderr log of SIDE written by make_rshogi_wrapper.sh has a
# "nnue shared weights ...: shared" marker and none has a "local" marker.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: check_rshogi_shared_weights.sh OUT_DIR SIDE" >&2
  exit 2
fi
out=$1
side=$2

found=0
for log in "$out"/engine-stderr/"$side"-*.log; do
  test -e "$log" || continue
  found=1
  if ! grep -Eq 'nnue shared weights( \[[^]]+\])?: shared' "$log" ||
    grep -Eq 'nnue shared weights( \[[^]]+\])?: local' "$log"; then
    printf 'suspect shared weights log: %s\n' "$log" >&2
    exit 1
  fi
done
if test "$found" -ne 1; then
  printf 'missing shared weights log for side: %s\n' "$side" >&2
  exit 1
fi
printf 'shared weights OK for side: %s\n' "$side"
