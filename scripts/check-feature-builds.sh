#!/usr/bin/env bash
# 既定 feature では一度もコンパイルされない構成を `cargo check` で検査する。
#
# workspace の clippy / test は既定 feature (edition-universal) だけを対象にするため、
# 配布に使う固定 edition や opt-in feature でだけ生きる cfg 経路の破損はそこでは見つからない。
# ここでは全 edition と opt-in feature を個別に check し、失敗した構成をまとめて報告する。
# 検査できるのはコンパイルの成否だけで、各構成のテスト実行は含まない。
#
# 使い方:
#   bash scripts/check-feature-builds.sh          # 全構成
#   SHARD_INDEX=0 SHARD_TOTAL=4 bash scripts/check-feature-builds.sh
#                                                 # 構成を 4 分割した 0 番目だけ (CI の並列化用)
# 失敗した構成は、表示された `cargo check ...` をそのまま実行すれば再現できる。
# CI は RUSTFLAGS に `-D warnings` を足して実行するため、warning による失敗を
# 再現するときは同じ指定を付ける。
set -u

cd "$(dirname "$0")/.." || exit 1

shard_index=${SHARD_INDEX:-0}
shard_total=${SHARD_TOTAL:-1}
config_index=0
failed=()

run() {
  local mine=$((config_index % shard_total))
  config_index=$((config_index + 1))
  if [ "${mine}" -ne "${shard_index}" ]; then
    return
  fi
  echo "::group::cargo check $*"
  if cargo check "$@"; then
    echo "::endgroup::"
  else
    echo "::endgroup::"
    echo "::error::失敗: cargo check $*"
    failed+=("cargo check $*")
  fi
}

# rshogi-usi の全 edition。一覧は Cargo.toml の [features] から取り、追加時の更新漏れを防ぐ。
# rshogi-core にだけ定義した edition はここに現れない。
editions=$(awk '/^\[features\]/{f=1;next} /^\[/{f=0} f && /^edition-[^ ]+ *=/{print $1}' \
  crates/rshogi-usi/Cargo.toml)
if [ -z "${editions}" ]; then
  echo "::error::crates/rshogi-usi/Cargo.toml から edition を取得できない"
  exit 1
fi
for edition in ${editions}; do
  run -p rshogi-usi --all-targets --no-default-features --features "${edition}"
done

# 既定 edition へ単独で追加できる opt-in feature (rshogi-usi と rshogi-core の両方にあるもの)。
# 新しい opt-in feature を足したらこの一覧にも追加する。
# nnue-progress-diff は build.rs が対応 edition に限定しているため単独では検査せず、
# それを含む edition の check に任せる。threat-profile-* のうち edition に含まれるものも同様で、
# どの edition にも含まれない threat-profile-cross-side だけをここで検査する。
for feature in prepacked-nnue search-stats nnue-stats diagnostics allocation-stats tt-write-stats tt-trace \
  threat-profile-cross-side; do
  run -p rshogi-usi --all-targets --features "${feature}"
  run -p rshogi-core --all-targets --features "${feature}"
done

# 固定 edition と opt-in feature の組み合わせ、および tools 経由の有効化。
run -p rshogi-usi --all-targets --no-default-features \
  --features edition-layerstacks-halfka_hm_merged-1536x16x32-none,prepacked-nnue
run -p tools --all-targets --features prepacked-nnue
run -p tools --all-targets --features diagnostics

if [ ${#failed[@]} -ne 0 ]; then
  echo
  echo "失敗した構成 (${#failed[@]}):"
  printf '  %s\n' "${failed[@]}"
  exit 1
fi
echo "OK: shard ${shard_index}/${shard_total} の全構成で cargo check が成功"
