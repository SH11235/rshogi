#!/usr/bin/env bash
# usage: make_rshogi_wrapper.sh OUT_DIR SIDE RSHOGI_BINARY
# Prints the path of a wrapper that execs RSHOGI_BINARY and appends its stderr to
# OUT_DIR/engine-stderr/SIDE-XXXXXX.log (one file per process), so the
# "nnue shared weights" marker of every process survives the run.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: make_rshogi_wrapper.sh OUT_DIR SIDE RSHOGI_BINARY" >&2
  exit 2
fi
out=$1
side=$2
bin=$3

case "$side" in
  '' | . | .. | *[!A-Za-z0-9._-]*)
    printf 'invalid engine side: %s\n' "$side" >&2
    exit 2
    ;;
esac
for old_log in "$out"/engine-stderr/*.log; do
  if test -e "$old_log"; then
    printf 'existing engine stderr log found; use a fresh OUT: %s\n' "$out" >&2
    exit 1
  fi
done
if ! real_bin=$(readlink -f -- "$bin") || ! test -x "$real_bin"; then
  printf 'rshogi binary is not executable: %s\n' "$bin" >&2
  exit 1
fi
side_dir="$out/engine-stderr/$side"
mkdir -p "$out/engine-stderr"
if ! mkdir "$side_dir"; then
  printf 'engine side already exists or cannot be created (%s); use a fresh OUT: %s\n' \
    "$side" "$out" >&2
  exit 1
fi
printf '%s\n' "$real_bin" > "$side_dir/real-bin"
cat > "$side_dir/run" <<'SH'
#!/bin/sh
set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
log_dir=$(dirname -- "$script_dir")
side=$(basename -- "$script_dir")
IFS= read -r real_bin < "$script_dir/real-bin"
log_file=$(mktemp "$log_dir/$side-XXXXXX.log")
exec "$real_bin" "$@" 2>> "$log_file"
SH
chmod 700 "$side_dir/run"
printf '%s\n' "$side_dir/run"
