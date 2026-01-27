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
| HalfKA_hm512 | 512 | 8 | 96 | HalfKA_hm (73,305次元) |
| HalfKA_hm1024 | 1024 | 8 | 96 | HalfKA_hm (73,305次元) |

### 測定項目

- **refresh_accumulator**: Feature Transformer の全計算（駒配置から特徴量を計算）
- **evaluate**: ネットワーク推論（Accumulator → 評価値）
- **total**: refresh + evaluate の合計

## ベンチマーク実行方法

### ビルド

```bash
# リポジトリルートで実行

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
  --nnue-file <HALFKP_256_NNUE_FILE> \
  --iterations 500000 \
  --warmup 10000 \
  --compare
```

### オプション

| オプション | 説明 | デフォルト |
|-----------|------|-----------|
| `--nnue-file` | NNUEファイルのパス | 必須 |
| `--iterations` | 測定反復回数 | 1,000,000 |
| `--warmup` | ウォームアップ回数 | 10,000 |
| `--compare` | static vs const generics 比較モード | false |

NNUEファイルの配置規約は [eval/README.md](../../eval/README.md) を参照。

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

測定結果は `nnue-eval-results.json` に記録する。

```bash
# 結果の確認
cat docs/performance/nnue-eval-results.json | jq .
```
