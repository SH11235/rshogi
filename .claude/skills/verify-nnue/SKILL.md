---
description: NNUE モデルの正当性検証。refresh vs update 一致テスト + bullet-shogi とのクロス実装検証を実行する。「モデルを検証して」「Golden Forward テストして」等のリクエストに使用する。
user-invocable: true
---

# NNUE モデル検証スキル

学習済み quantised.bin の正当性を 2 段階で検証する。
アーキテクチャ（素の LayerStacks / PSQT / Threat / PSQT+Threat）を問わず使用可能。

## 入力パラメータ

`$ARGUMENTS` から以下を受け取る。不足時は質問して補完する。

### 必須
- **quantised.bin パス**: 検証対象のモデルファイル

### オプション
- **checkpoint_dir**: bullet クロス検証用。省略時は quantised.bin の親ディレクトリ
- **--threat**: Threat モデルの場合に bullet 側 `shogi_layerstack_eval` に渡す
- **threat-profile**: Threat 次元削減プロファイル。bullet/rshogi 両方のビルドに同一 feature を指定する必要がある
  - `threat-profile-same-class` (profile 1): 同種ペア全除外 (192,640 dims)
  - `threat-profile-same-class-major-pawn` (profile 2): 同種 + 大駒→歩除外 (173,568 dims)

### デフォルト値（変更不要なら省略可）
- **progress.bin**: `/home/sh11235/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin`
- **bucket-mode**: `progress8kpabs`
- **教師データ**: `/home/sh11235/development/bullet-shogi/data/DLSuisho15b_deduped_shuffled.bin`
- **moves**: `100`

## 検証手順

### Step 1: refresh vs update 一致テスト

```bash
cargo run --release --bin verify_nnue_accumulator -- \
  --nnue-file <quantised.bin> \
  --ls-progress-coeff <progress.bin> \
  --moves 100
```

**判定**: `ALL PASSED` なら OK。`MISMATCH` があれば差分更新にバグ。

### Step 2: クロス実装検証

#### Step 2a: bullet 側で参照値生成

```bash
cd /home/sh11235/development/bullet-shogi
cargo run --release --example shogi_layerstack_eval -- \
  --checkpoint <checkpoint_dir> \
  --pack data/DLSuisho15b_deduped_shuffled.bin \
  --integer-forward --samples 1 \
  --bucket-mode progress8kpabs \
  --progress-coeff data/progress/nodchip_progress_e1_f1_cuda.bin \
  [--threat]
```

出力から `SFEN:` と `final_score:` を記録。

#### Step 2b: rshogi 側で同一局面を評価

```bash
# diagnostics ビルド（初回のみ）
cargo build --release -p rshogi-usi --features diagnostics

printf 'setoption name EvalFile value <quantised.bin>
setoption name LS_BUCKET_MODE value progress8kpabs
setoption name LS_PROGRESS_COEFF value <progress.bin>
isready
position sfen <Step 2a の SFEN>
eval diag
quit
' | RUST_LOG=info ./target/release/rshogi-usi 2>&1 | grep "score:"
```

**判定**: `final_score` が完全一致すれば OK。

---

## 具体的なケース別コマンド例

### Case A: PSQT 単体 (v88 等)

```bash
# Step 1
cargo run --release --bin verify_nnue_accumulator -- \
  --nnue-file /home/sh11235/development/bullet-shogi/checkpoints/v88/v88-20/quantised.bin \
  --ls-progress-coeff /home/sh11235/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin \
  --moves 100

# Step 2a (bullet): --threat なし（PSQT は arch_str から自動検出）
cd /home/sh11235/development/bullet-shogi
cargo run --release --example shogi_layerstack_eval -- \
  --checkpoint checkpoints/v88/v88-20 \
  --pack data/DLSuisho15b_deduped_shuffled.bin \
  --integer-forward --samples 1 \
  --bucket-mode progress8kpabs \
  --progress-coeff data/progress/nodchip_progress_e1_f1_cuda.bin
```

### Case B: Threat 単体 (v89 等)

```bash
# Step 1
cargo run --release --bin verify_nnue_accumulator -- \
  --nnue-file /path/to/v89-checkpoint/quantised.bin \
  --ls-progress-coeff /home/sh11235/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin \
  --moves 100

# Step 2a (bullet): --threat 必須
cd /home/sh11235/development/bullet-shogi
cargo run --release --example shogi_layerstack_eval -- \
  --checkpoint /path/to/v89-checkpoint \
  --pack data/DLSuisho15b_deduped_shuffled.bin \
  --integer-forward --samples 1 \
  --bucket-mode progress8kpabs \
  --progress-coeff data/progress/nodchip_progress_e1_f1_cuda.bin \
  --threat
```

### Case C: PSQT + Threat (v90 等)

```bash
# Step 1: 同一コマンド（モデルが自動判定）
cargo run --release --bin verify_nnue_accumulator -- \
  --nnue-file /path/to/v90-checkpoint/quantised.bin \
  --ls-progress-coeff /home/sh11235/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin \
  --moves 100

# Step 2a (bullet): --threat 必須（PSQT は自動検出）
cd /home/sh11235/development/bullet-shogi
cargo run --release --example shogi_layerstack_eval -- \
  --checkpoint /path/to/v90-checkpoint \
  --pack data/DLSuisho15b_deduped_shuffled.bin \
  --integer-forward --samples 1 \
  --bucket-mode progress8kpabs \
  --progress-coeff data/progress/nodchip_progress_e1_f1_cuda.bin \
  --threat
```

### Case D: 素の LayerStacks（PSQT なし / Threat なし, v87 等）

```bash
# Step 1
cargo run --release --bin verify_nnue_accumulator -- \
  --nnue-file /path/to/v87-checkpoint/quantised.bin \
  --ls-progress-coeff /home/sh11235/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin \
  --moves 100

# Step 2a (bullet): オプションなし
cd /home/sh11235/development/bullet-shogi
cargo run --release --example shogi_layerstack_eval -- \
  --checkpoint /path/to/v87-checkpoint \
  --pack data/DLSuisho15b_deduped_shuffled.bin \
  --integer-forward --samples 1 \
  --bucket-mode progress8kpabs \
  --progress-coeff data/progress/nodchip_progress_e1_f1_cuda.bin
```

### Case E: Threat + 次元削減プロファイル (v93 等)

Threat profile を使うモデルは **bullet/rshogi 両方のビルドで同一 profile feature を指定**する必要がある。
profile が不一致だと THREAT_DIMENSIONS が異なり、weight 読み込みが破壊的にずれる。

```bash
# Step 1: rshogi を profile 付きでビルド（初回のみ）
# ※ verify_nnue_accumulator は tools crate なのでそちらの feature も指定
cargo build --release -p tools --no-default-features \
  --features layerstacks-768,nnue-threat,threat-profile-same-class
cargo run --release --bin verify_nnue_accumulator -- \
  --nnue-file /home/sh11235/development/bullet-shogi/checkpoints/v93/v93-20/quantised.bin \
  --ls-progress-coeff /home/sh11235/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin \
  --moves 100

# Step 2a (bullet): --threat 必須、profile feature でビルド
cd /home/sh11235/development/bullet-shogi
cargo run --release --no-default-features --features cpu,threat-profile-same-class \
  --example shogi_layerstack_eval -- \
  --checkpoint checkpoints/v93/v93-20 \
  --pack data/DLSuisho15b_deduped_shuffled.bin \
  --integer-forward --samples 32 \
  --bucket-mode progress8kpabs \
  --progress-coeff data/progress/nodchip_progress_e1_f1_cuda.bin \
  --threat

# Step 2b (rshogi): diagnostics + profile 付きでビルド
cargo build --release -p rshogi-usi --no-default-features \
  --features rshogi-core/layerstack-only,rshogi-core/layerstacks-768,rshogi-core/nnue-threat,rshogi-core/threat-profile-same-class,rshogi-core/search-no-pass-rules,rshogi-usi/layerstack-only,rshogi-usi/nnue-threat,rshogi-usi/threat-profile-same-class,rshogi-usi/diagnostics

# 以降は Case B と同様に eval diag で比較
```

**⚠️ 重要**: profile 2 (`threat-profile-same-class-major-pawn`) の場合は上記の `threat-profile-same-class` を `threat-profile-same-class-major-pawn` に全て置き換えること。

---

## 不一致時のデバッグ

### 比較すべき中間値

| 中間値 | bullet 出力キー | rshogi ログキー |
|--------|-----------------|-----------------|
| FT accumulator | `FT acc[stm] first 8` | `us_combined (piece+threat) first 16` ※Threat 時 |
| | | `us_acc (piece) first 16` ※非Threat 時 |
| Product Pooling | `PP out first 8` | `transformed first 32` |
| L1 出力 | `L1 out` | `l1_out` |
| L1 skip | `L1 skip` | `l1_skip` |
| raw_score | `raw_score` | `raw_score (with skip)` |
| PSQT value | `psqt_value` | `psqt_value` |
| 最終スコア | `final_score` | `score:` の最初の数値 |

### よくある原因

- **FT accumulator が不一致**: feature index 計算の差異、weight layout の転置
- **PSQT だけ不一致**: PSQT の read サイズが input_size vs halfka_dim で不整合
- **final_score だけ不一致**: fv_scale の差異、PSQT /2 の有無
- **rshogi diagnostics が Threat を反映しない**: `evaluate_with_diagnostics` の Threat 結合漏れ（過去に発見・修正済み）

## 注意事項

- rshogi の diagnostics ビルドは `--features diagnostics` が必要
- `NNUE_ARCHITECTURE` USI オプションは**指定しない**（arch_str から自動検出させる）
- bullet 側 `--threat` は Threat モデル時に必須（`get_active_features` の入力型分岐に必要）
- PSQT の有無は bullet/rshogi 両方で arch_str から自動検出される
- **Threat profile は bullet/rshogi 両方のビルドで必ず同一 feature を指定すること**。不一致だと THREAT_DIMENSIONS が異なり weight 読み込みが破壊的にずれる。profile 0 (全ペア) はデフォルトで feature 指定不要
