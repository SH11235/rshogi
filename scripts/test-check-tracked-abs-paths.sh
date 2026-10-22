#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
temp_parent=$(cd -- "${TMPDIR:-/tmp}" && pwd -P)
test_root=$(mktemp -d "$temp_parent/rshogi-abs-paths.XXXXXX")
case "$test_root" in
  "$temp_parent"/rshogi-abs-paths.*) ;;
  *) echo "ERROR: unexpected temporary directory: $test_root" >&2; exit 1 ;;
esac
trap 'rm -rf -- "$test_root"' EXIT

# 呼び出し元の Git 環境と親リポジトリから fixture を隔離する。
git_local_env=$(git rev-parse --local-env-vars)
while IFS= read -r variable; do
  unset "$variable"
done <<< "$git_local_env"
export GIT_CEILING_DIRECTORIES="$test_root"
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_GLOBAL=/dev/null

expect_status() {
  local name=$1 expected=$2 directory=$3
  shift 3
  local output status
  if output=$(cd -- "$directory" && "$@" 2>&1); then
    status=0
  else
    status=$?
  fi
  if [[ "$status" -ne "$expected" ]]; then
    printf 'FAIL %s: expected exit %s, got %s\n%s\n' "$name" "$expected" "$status" "$output" >&2
    exit 1
  fi
  if [[ "$expected" -eq 0 ]]; then
    if [[ "$output" != 'OK: '* ]]; then
      printf 'FAIL %s: missing success message\n%s\n' "$name" "$output" >&2
      exit 1
    fi
  elif [[ -z "$output" || "$output" == *'OK: '* ]]; then
    printf 'FAIL %s: missing error or false success message\n%s\n' "$name" "$output" >&2
    exit 1
  fi
  printf 'PASS %s (exit %s)\n' "$name" "$status"
}

repo="$test_root/repository"
mkdir "$test_root/template"
git init --quiet --template="$test_root/template" "$repo"
expect_status empty-repository 0 "$repo" "$BASH" "$script_dir/check-tracked-abs-paths.sh"

mkdir -p "$repo/scripts" "$repo/nested" "$test_root/not-repository" "$test_root/empty-bin"
cp "$script_dir/check-tracked-abs-paths.sh" "$repo/scripts/"
checker="$repo/scripts/check-tracked-abs-paths.sh"
printf '%s\n' 'model=$SHOGI_DATA/nnue/model.bin' > "$repo/tracked.txt"
git -C "$repo" add scripts/check-tracked-abs-paths.sh tracked.txt
expect_status clean-repository-from-subdirectory 0 "$repo/nested" "$BASH" "$checker"

printf '/home/%s/model.bin\n' sh11235 > "$repo/untracked.txt"
expect_status untracked-file-is-ignored 0 "$repo" "$BASH" "$checker"
git -C "$repo" add untracked.txt
expect_status forbidden-home-path 1 "$repo" "$BASH" "$checker"
printf '/mnt/%s/model.bin\n' nvme1 > "$repo/untracked.txt"
expect_status forbidden-data-path 1 "$repo" "$BASH" "$checker"
printf '%s\n' 'model=$SHOGI_DATA/nnue/model.bin' > "$repo/untracked.txt"
expect_status clean-working-tree-content 0 "$repo" "$BASH" "$checker"

expect_status outside-repository 128 "$test_root/not-repository" "$BASH" "$checker"
expect_status missing-git 127 "$repo" env PATH="$test_root/empty-bin" "$BASH" "$checker"
printf '%s\n' 'invalid index' > "$repo/.git/index"
expect_status unreadable-git-index 128 "$repo" "$BASH" "$checker"
