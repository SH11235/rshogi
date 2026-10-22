#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 || $EUID -ne 0 || $(uname -s) != Linux ]]; then
  echo 'usage (Linux root): test-pack-output-mode.sh BINARY UNPRIVILEGED_USER' >&2
  exit 2
fi
binary=$(realpath -- "$1")
if [[ ! -f "$binary" || ! -x "$binary" ]]; then
  echo 'BINARY must be an executable regular file' >&2
  exit 2
fi
run_user=$2
run_uid=$(id -u -- "$run_user")
user_groups=$(id -G -- "$run_user")
foreign_gid=65534
if [[ "$run_uid" -eq 0 || " $user_groups " == *" $foreign_gid "* ]]; then
  echo 'user must be unprivileged and outside group 65534' >&2
  exit 2
fi

umask 077
temp_parent=$(cd -- "${TMPDIR:-/tmp}" && pwd -P)
if [[ "$temp_parent" == / || "$temp_parent" != /* ]]; then
  echo 'temporary parent must be an absolute directory other than /' >&2
  exit 2
fi
test_root=$(mktemp -d -- "$temp_parent/rshogi-pack-mode.XXXXXX")
test_root=$(realpath -- "$test_root")
case "$test_root" in
  "$temp_parent"/rshogi-pack-mode.*) ;;
  *) echo 'unexpected temporary path' >&2; exit 2 ;;
esac
cleanup() {
  case "$test_root" in
    "$temp_parent"/rshogi-pack-mode.*) ;;
    *) echo 'refusing cleanup outside the temporary parent' >&2; return 1 ;;
  esac
  if [[ -L "$test_root" || $(realpath -- "$test_root") != "$test_root" ]]; then
    echo 'refusing cleanup of a replaced temporary path' >&2
    return 1
  fi
  rm -rf -- "$test_root"
}
trap cleanup EXIT

chmod 0700 -- "$test_root"
mkdir -- "$test_root/output"
printf '\001\000\000\000' > "$test_root/input.pack"
chmod 0644 -- "$test_root/input.pack"
output="$test_root/output/existing.psv"
printf 'previous output bytes' > "$output"
chown -- "$run_uid:$foreign_gid" "$output"
chmod 2750 -- "$output"
before=$(sha256sum -- "$output")
before=${before%% *}

# writer だけに書込みを許し、staging に非所属 group を継承させる。
chown -- "$run_uid:$foreign_gid" "$test_root/output"
chmod 2700 -- "$test_root/output"
chmod 0755 -- "$test_root"
if runuser -u "$run_user" -- "$binary" --input "$test_root/input.pack" --output "$output" > "$test_root/run.log" 2>&1; then
  status=0
else
  status=$?
fi
cat -- "$test_root/run.log"
after=$(sha256sum -- "$output")
after=${after%% *}
mode=$(stat -c '%a' -- "$output")
printf 'exit=%s mode=%s before=%s after=%s\n' "$status" "$mode" "$before" "$after"
if [[ "$status" -eq 0 || "$mode" != 2750 || "$before" != "$after" ]]; then
  echo 'FAIL: unsupported mode restoration must preserve existing output and return failure' >&2
  exit 1
fi
if ! grep -Fq -- 'kind: PermissionDenied' "$test_root/run.log" || ! grep -Fq -- 'expected=2750, actual=0750' "$test_root/run.log"; then
  echo 'FAIL: conversion did not report the expected mode mismatch' >&2
  exit 1
fi
shopt -s nullglob dotglob
remaining=("$test_root/output/"*)
if [[ ${#remaining[@]} -ne 1 ]]; then
  echo 'FAIL: unexpected entry remains in the output directory' >&2
  exit 1
fi
if [[ "${remaining[0]}" != "$output" ]]; then
  echo 'FAIL: existing output is missing' >&2
  exit 1
fi
echo 'PASS: foreign-group setgid failure preserves bytes and mode, and removes staging'
