#!/usr/bin/env bash
# base ref からの変更 (commit 済み・未 commit・untracked) を含む crate と、それに依存する crate の
# テストだけを nextest で走らせる。workspace 共通の設定が変わったときは全体を走らせる。
# doctest は nextest が走らせないので、作業の最後に `cargo test` で全体を確かめる。
#
# usage: scripts/test-affected.sh [--base <ref>] [nextest の追加引数...]
#   --base の既定は origin/main。追加引数はそのまま `cargo nextest run` に渡す。
set -euo pipefail
repo_root=$(git rev-parse --show-toplevel)
cd -- "$repo_root"

base=origin/main
if [ "${1:-}" = "--base" ]; then
  base=${2:?--base には ref を指定してください}
  shift 2
fi

merge_base=$(git merge-base "$base" HEAD)
changed=$(
  {
    git diff --name-only "$merge_base"
    git ls-files --others --exclude-standard
  } | sort -u
)

run_all=false
crates=()
while IFS= read -r path; do
  [ -n "$path" ] || continue
  case "$path" in
    Cargo.toml | Cargo.lock | rust-toolchain.toml | .cargo/*)
      run_all=true
      ;;
    crates/*/*)
      dir=${path#crates/}
      dir=${dir%%/*}
      manifest="crates/$dir/Cargo.toml"
      [ -f "$manifest" ] || continue
      # crate 直下の Cargo.toml は [package] が先頭にあるので、最初の name 行が package 名になる。
      name=$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$manifest" | head -n 1)
      if [ -z "$name" ]; then
        echo "ERROR: $manifest から package 名を読めません" >&2
        exit 1
      fi
      crates+=("$name")
      ;;
  esac
done <<< "$changed"

if $run_all; then
  echo "workspace 共通の設定が変わったので全体を走らせます" >&2
  exec cargo nextest run --workspace "$@"
fi

if [ "${#crates[@]}" -eq 0 ]; then
  echo "$merge_base 以降、crate の変更はありません" >&2
  exit 0
fi

filter=""
for name in $(printf '%s\n' "${crates[@]}" | sort -u); do
  filter+="${filter:+ | }rdeps(=$name)"
done
echo "filterset: $filter" >&2
exec cargo nextest run --workspace -E "$filter" "$@"
