# NNUE評価関数ベンチマーク方法

NNUE評価関数の性能測定手順と分析方法を記載する。

## 概要

const-generics ベースの NNUE ネットワーク実装の性能を測定し、
静的実装との比較やデグレ検知を行う。

## 測定対象

### アーキテクチャ

| 種別 | L1 | L2 | L3 | 特徴量 |
|------|-----|-----|-----|--------|
| HalfKP256 | 256 | 32 | 32 | HalfKP (125,388次元) |
| HalfKP512 | 512 | 8 | 96 | HalfKP (125,388次元) |
| HalfKA512 | 512 | 8 | 96 | HalfKA_hm (73,305次元) |
| HalfKA1024 | 1024 | 8 | 96 | HalfKA_hm (73,305次元) |

### 測定項目

- **refresh_accumulator**: Feature Transformer の全計算（駒配置から特徴量を計算）
- **evaluate**: ネットワーク推論（Accumulator → 評価値）
- **total**: refresh + evaluate の合計

## ベンチマーク実行方法

### ビルド

```bash
cd packages/rust-core

# AVX2を有効化してリリースビルド
RUSTFLAGS="-C target-cpu=native" cargo build --release --bin bench_nnue_eval
```

### 実行

```bash
# 基本実行（50万回反復、1万回ウォームアップ）
./target/release/bench_nnue_eval \
  --nnue-file <NNUE_FILE> \
  --iterations 500000 \
  --warmup 10000

# 静的実装とconst generics実装の比較（HalfKP 256x2-32-32のみ）
./target/release/bench_nnue_eval \
  --nnue-file eval/suisho_finetune/suisho5_reconverted.nnue \
  --iterations 500000 \
  --warmup 10000 \
  --compare
```

### NNUEファイル例

| アーキテクチャ | ファイルパス |
|----------------|--------------|
| HalfKP 256x2-32-32 | `eval/suisho_finetune/suisho5_reconverted.nnue` |
| HalfKA 512x2-8-96 | `eval/exp_100epoch_v2/epoch10.nnue` |
| HalfKA 1024x2-8-96 | `eval/halfka_hm_1024x2-8-96/epoch20_v2.nnue` |

## テスト局面

以下の5局面を使用（多様な局面をカバー）：

1. 初期局面
2. 中盤局面（矢倉模様）
3. 中盤局面（居飛車vs振り飛車）
4. 終盤局面（駒が少ない）
5. 複雑な中盤（駒の配置が多い）

## 結果の解釈

### 性能特性

1. **refresh_accumulator はL1サイズに比例**
   - L1=256 → L1=512: 約2倍
   - L1=512 → L1=1024: 約2倍

2. **evaluate はネットワーク構造に依存**
   - L1サイズが大きいほど遅い（入力次元が増える）
   - L2/L3が同じなら差は小さい

3. **実探索での性能**
   - 差分更新により refresh_accumulator の頻度は低下
   - evaluate の性能がより重要になる

### 比較時の注意点

- 同一環境（CPU、Rustバージョン）で測定すること
- ウォームアップ後の測定値を使用すること
- 複数回測定して安定性を確認すること

## 結果ファイル

測定結果は `docs/benchmarks/nnue-eval-results.json` に記録する。

```bash
# 結果の確認
cat docs/benchmarks/nnue-eval-results.json | jq .
```

## NNUEファイル変換

bullet-shogiのチェックポイントからNNUEファイルを生成：

```bash
# HalfKP用
./target/release/convert_bullet_nnue \
  --input <checkpoint>/raw.bin \
  --output eval/halfkp_512x2-8-96.nnue \
  --arch 512x2-8-96 --features HalfKP --scale 1600

# HalfKA_hm用
./target/release/convert_bullet_nnue \
  --input <checkpoint>/raw.bin \
  --output eval/halfka_hm_512x2-8-96.nnue \
  --arch 512x2-8-96 --features HalfKA_hm --scale 400
```
