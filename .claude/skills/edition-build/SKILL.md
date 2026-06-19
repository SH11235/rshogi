---
description: rshogi engine の preset edition build と NNUE format 互換性管理。xtask で多数の architecture variant (HalfKP / HalfKA / HalfKA_HM、LayerStack 1536x16x32 / 1536x32x32 / 768x16x32 / 512x16x32、PSQT / Threat / progress-diff) を build し engines/ 配下に配置する。古い engine binary が新 NNUE ckpt format (`Unknown NNUE version: 0x...`) を load できない場合の rebuild 手順、preset から外れた手動 cargo build 時の feature 表、build profile (release / production) 選択基準を扱う。「engine build」「edition build」「format 互換性」「Unknown NNUE version」「preset 一覧」等のリクエストに使用する。
user-invocable: true
---

# rshogi engine build / edition 管理スキル

rshogi は HalfKP / HalfKA / HalfKA_HM (split / merged) などの feature-set、LayerStack
1536x16x32 / 1536x32x32 / 768x16x32 / 512x16x32 などの dim、PSQT / Threat 等の拡張、を
組み合わせた多数の architecture variant をサポートする。これらは Cargo feature の組合せで
build し、preset として `xtask` から呼び出せる。

本 SKILL は engine binary の build と NNUE format 互換性管理を扱う。
selfplay / SPRT 等の対局運用は [`selfplay` SKILL](../selfplay/SKILL.md) を参照。

## 1. preset edition の取得

利用可能な preset 一覧:

```bash
cd /path/to/rshogi
cargo run --release -p xtask -- list-editions
```

代表例 (2026-06 時点):

| preset 名 | 説明 |
|---|---|
| `edition-ls-halfka_hm_merged-1536x16x32-none` | LayerStack 1536x16x32 + HalfKA_HM merged (default 推奨) |
| `edition-ls-halfka_hm_merged-1536x16x32-psqt` | 同上 + PSQT |
| `edition-ls-halfka_hm_merged-1536x16x32-threat` | 同上 + Threat |
| `edition-ls-halfka_hm_merged-1536x16x32-psqt_threat` | 同上 + PSQT + Threat |
| `edition-ls-halfka_hm_merged-1536x32x32-none` | 旧 L1=32 系 |
| `edition-ls-halfka_hm_merged-768x16x32-none` | 縮小 L0=768 |
| `edition-ls-halfka_hm_merged-512x16x32-none` | 縮小 L0=512 |
| `edition-ls-halfka_hm_split-1536x16x32-none` | HalfKA_HM split |
| `edition-ls-halfka_split-1536x16x32-none` | HalfKA split |
| `edition-ls-halfka_merged-1536x16x32-none` | HalfKA merged |
| `edition-ls-halfkp-1536x16x32-none` | HalfKP |
| `edition-halfka_hm_merged-screlu` | 非 LayerStack 旧 (SCReLU) |
| `edition-halfkp-crelu` | 非 LayerStack 旧 (CReLU) |
| `edition-ls-any-any-any` | 動的 dispatch 全部入り (計測用) |

## 2. xtask build (推奨経路)

preset 名 (`edition-` 接頭辞省略可、複数指定可) を指定:

```bash
# 単一 edition
cargo run --release -p xtask -- build --edition ls-halfkp-1536x16x32-none --profile production

# 複数 edition (順次 build)
cargo run --release -p xtask -- build \
  --edition ls-halfkp-1536x16x32-none \
  --edition ls-halfka_hm_merged-1536x16x32-none \
  --profile production

# 全 preset
cargo run --release -p xtask -- build --all-presets --profile production
```

出力:

- `engines/rshogi-usi-<edition>` (binary)
- `engines/rshogi-usi-<edition>.meta.toml` (build trace: edition / profile / commit / built_at / rustc)

`engines/` は gitignored なので長期保持される (`target/production/` は `cargo clean`
で消える、`/tmp/` は再起動で揮発するため使わない)。

## 3. build profile

NPS 比較を含む評価では **全 engine 同一 profile** で揃える:

| profile | LTO | codegen-units | overflow-checks | 用途 |
|---|---|---|---|---|
| `release` (default) | thin | 4 | true | 開発・短時間検証 |
| `production` | fat | 1 | false | **配布・SPRT・性能計測** (推奨) |

production は release より約 1.5% 低い instructions/node を達成する。異 profile を混在
すると evaluator/search に無関係な NPS 差が生まれ、評価結果がバイアスを受ける。

古い binary を流用する場合は **必ず profile 確認** (`<binary>.meta.toml` の `profile`
フィールド)。不明なら再 build。

## 4. NNUE format 互換性 (pre-flight 必須)

新しい NNUE ckpt format (例: `num_buckets` explicit version = `0x7af32f21`、新 quant
flag を使った export) は古い engine binary で **load 失敗 → fatal panic** する:

```
info string Error loading NNUE file: Unknown NNUE version: 0x7af32f21.
            Expected 0x7af32f16 (HalfKP) or 0x7af32f20 (HalfKaHmMerged^)
thread 'main' panicked: EvalFile was explicitly set but failed to load.
```

selfplay / SPRT 起動前に以下 3 段の pre-flight check を踏むこと。

### (a) ckpt の version 確認

`inspect_ft.py` は tatara (NNUE trainer) 側の ckpt 検査 helper（本 repo には含まれない）。

```bash
python3 /path/to/inspect_ft.py <ckpt.bin> | head -3
# nnue_ver: 0x7af32f21 (current)   ← num_buckets explicit、最新 engine 必要
# nnue_ver: 0x7af32f20 (legacy)    ← num_buckets implicit (=9)、旧 engine 互換
```

### (b) engine binary の build commit 確認

```bash
ls -la engines/<binary>
cat engines/<binary>.meta.toml
# edition / profile / commit / built_at が記録されている (features は edition 名から逆引き)
```

ckpt 作成時点の rshogi commit より engine build commit が古ければ rebuild 必要。

### (c) 試金石: usi-isready で load test

```bash
echo -e 'usi
setoption name EvalFile value <ckpt.bin>
setoption name FV_SCALE value 28
setoption name LS_BUCKET_MODE value progress8kpabs
setoption name LS_PROGRESS_COEFF value <progress.bin>
isready
quit' | timeout 30 engines/<binary> 2>&1 | grep -E 'readyok|Error|Unknown NNUE version|panic'
```

`readyok` 確認できれば OK。`Unknown NNUE version: 0x...` 等の error が出たら `xtask
build` で再構築。

## 5. Cargo feature の構造 (preset から外れた実験的組合せ用)

通常は preset で十分なので xtask 推奨。preset で対応できない実験的組合せのみ手動 build。

feature は 4 カテゴリの組み合わせ:

1. **feature-set** (どれか 1 つ): `ft-halfkp` / `ft-halfka_split` / `ft-halfka_merged`
   / `ft-halfka_hm_split` / `ft-halfka_hm_merged`
2. **dispatch 除去**: `ls-arch` (LayerStack モデルでは常に指定。HalfKP/HalfKA
   dispatch を除去し `evaluate_dispatch` を直接呼ぶ)
3. **L1×L2 dim** (どれか 1 つ): `ls-size-1536x16x32` (default) / `ls-size-1536x32x32`
   / `ls-size-768x16x32` / `ls-size-512x16x32`。複数同時指定は cycles +5.5% 退行
4. **拡張 / 最適化**: `ls-ext-psqt` / `ls-ext-threat` / `nnue-progress-diff` (L0=1536 限定、
   L0=768/512 では cache pressure 増加で cycles +2-6% 退行するため指定しない)
5. **Threat profile** (Threat 使用時に任意で 1 つ): `threat-profile-same-class` (id1) /
   `threat-profile-same-class-major-pawn` (id2) / `threat-profile-cross-side` (id10)。
   除外 pair で次元を削減した変種で、未指定は full (id0)。edition grammar に profile 軸が
   無いため preset は無く、`ls-ext-threat` に手動で 1 つ足す。engine と学習 net の profile は
   一致必須 (不一致は EvalFile load 時に reject)。

最新 feature 名は実コード確認:

```bash
grep -E '^[a-z][a-z0-9-]+ =' crates/rshogi-core/Cargo.toml
```

### モデル → feature 対応表

preset 名から逆引きできる。preset と等価な features は下表、または rshogi-core
Cargo.toml の `edition-*` feature 定義で確認する (meta.toml には features は載らない)。

| アーキ | preset 名 (`xtask build --edition`) | 手動 build features |
|---|---|---|
| LayerStack 1536x16x32 | `ls-halfka_hm_merged-1536x16x32-none` | `ls-arch,nnue-progress-diff` (default に `ls-size-1536x16x32` 含む) |
| 同上 + PSQT | `ls-halfka_hm_merged-1536x16x32-psqt` | `ls-arch,ls-ext-psqt,nnue-progress-diff` |
| 同上 + Threat | `ls-halfka_hm_merged-1536x16x32-threat` | `ls-arch,ls-ext-threat,nnue-progress-diff` |
| 同上 + PSQT + Threat | `ls-halfka_hm_merged-1536x16x32-psqt_threat` | `ls-arch,ls-ext-psqt,ls-ext-threat,nnue-progress-diff` |
| 同上 + Threat (cross-side, id10) | (preset 無し、profile 軸は手動) | `ls-arch,ls-ext-threat,threat-profile-cross-side,nnue-progress-diff` |
| LayerStack 1536x32x32 (旧 L1=32) | `ls-halfka_hm_merged-1536x32x32-none` | `--no-default-features --features search-no-pass-rules,ls-arch,ls-size-1536x32x32,nnue-progress-diff` |
| LayerStack 768x16x32 | `ls-halfka_hm_merged-768x16x32-none` | `--no-default-features --features search-no-pass-rules,ls-arch,ls-size-768x16x32` |
| LayerStack 512x16x32 | `ls-halfka_hm_merged-512x16x32-none` | `--no-default-features --features search-no-pass-rules,ls-arch,ls-size-512x16x32` |
| LayerStack 1536x16x32 + HalfKP | `ls-halfkp-1536x16x32-none` | `ls-arch,ft-halfkp,nnue-progress-diff` |
| 非 LayerStack 旧 (CReLU/SCReLU) | `halfkp-crelu` / `halfka_hm_merged-screlu` 等 | preset 推奨 |

### 手動 cargo build (preset 外実験)

```bash
cargo build --profile production -p rshogi-usi \
  --no-default-features \
  --features search-no-pass-rules,ls-arch,ls-size-768x16x32
# binary を engines/ にコピーして退避
cp target/production/rshogi-usi engines/rshogi-usi-custom-768x16x32-<purpose>
```

注意:

- `ls-size-1536x16x32` がデフォルト feature。他 dim 使うには `--no-default-features`
  で外し、`search-no-pass-rules` (default 含) を再指定する
- 過去の `layerstacks-1536` (L1/L2 区別なし) feature は廃止済
- `cargo build` は同一 profile・同一 crate で feature が異なっても同じ出力パスに書き出す。
  別 feature 構成を残したい場合は **build 直後に別名 cp** 必須 (xtask は自動でやる)
- 過剰 feature は `readyok` を通過してしまい evaluator に余分な accumulator field 残留
  で NPS バイアスになる。**build cmd 自体の grep 検査が唯一の検出手段** (preset 経由なら
  meta.toml の edition 名から構成を特定可)

## 6. troubleshooting

| 症状 | 原因 | 対処 |
|---|---|---|
| `Unknown NNUE version: 0x7af32f21. Expected 0x7af32f16 / 0x7af32f20` | engine が旧 format しか対応していない (rshogi 側 format 拡張前の build) | `xtask build --edition <name> --profile production` で最新再 build |
| `EvalFile was explicitly set but failed to load` | 上記 format mismatch、もしくは ckpt path 間違い、ckpt の feature-set/dim と engine の build が不一致 | (1) `inspect_ft.py` で ckpt の `arch` 確認、(2) engine の preset/features を arch に合わせる |
| `readyok` 通過するが evaluator 出力おかしい | 過剰 feature で余分な accumulator field 残留 / 必要 feature 不足 | preset 経由で build し直し、対応表で feature 確認 |
| NPS 差が説明できない | binary 群が release / production 混在 | 全 binary を同一 profile で rebuild |

## 7. 関連 SKILL

- 対局運用 (selfplay / SPRT): [`selfplay` SKILL](../selfplay/SKILL.md)
- NNUE 検証 / 数値同等性: [`verify-nnue` SKILL](../verify-nnue/SKILL.md)
