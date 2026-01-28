# サポートされている NNUE アーキテクチャ

## 概要

rshogi は以下の NNUE アーキテクチャをサポートしています。

## Feature Set 一覧

| Feature Set | 入力次元 | 説明 |
|-------------|---------|------|
| HalfKP | 125,388 | 水匠/tanuki 互換（classic NNUE） |
| HalfKA_hm | 73,305 | Half-Mirror + Factorization |
| HalfKA | 138,510 | Non-mirror |
| LayerStacks | 固定 | 実験的（1536次元 + 9バケット） |

## HalfKP

水匠、tanuki、AobaNNUE などで使用される classic NNUE 形式。

| L1 | L2 | L3 | ファイルサイズ |
|----|----|----|---------------|
| 256 | 32 | 32 | 64,217,066 B (61.2 MB) |
| 512 | 8 | 96 | 128,410,336 B (122.5 MB) |
| 512 | 32 | 32 | 128,432,640 B (122.5 MB) |
| 768 | 16 | 64 | 192,624,724 B (183.7 MB) |
| 1024 | 8 | 32 | 256,814,496 B (244.9 MB) |
| 1024 | 8 | 96 | 256,816,864 B (244.9 MB) |

※ ファイルサイズは hash 有り形式（nnue-pytorch 標準）、arch_len=200 で計算

## HalfKA_hm

bullet-shogi / nnue-pytorch で学習可能な Half-Mirror 形式。

| L1 | L2 | L3 | ファイルサイズ |
|----|----|----|---------------|
| 256 | 8 | 96 | 37,538,196 B (35.8 MB) |
| 256 | 32 | 32 | 37,550,580 B (35.8 MB) |
| 512 | 8 | 96 | 75,077,332 B (71.6 MB) |
| 512 | 32 | 32 | 75,099,636 B (71.6 MB) |
| 1024 | 8 | 32 | 150,148,500 B (143.2 MB) |
| 1024 | 8 | 96 | 150,150,868 B (143.2 MB) |

## HalfKA

Non-mirror 形式。

| L1 | L2 | L3 | ファイルサイズ |
|----|----|----|---------------|
| 256 | 8 | 96 | 70,922,516 B (67.6 MB) |
| 256 | 32 | 32 | 70,934,900 B (67.6 MB) |
| 512 | 8 | 96 | 141,847,076 B (135.3 MB) |
| 512 | 32 | 32 | 141,869,380 B (135.3 MB) |
| 1024 | 8 | 32 | 283,688,340 B (270.5 MB) |
| 1024 | 8 | 96 | 283,690,708 B (270.5 MB) |

## 活性化関数

| 名前 | 説明 | 備考 |
|------|------|------|
| CReLU | Clipped ReLU: `clamp(x, 0, QA)` | **推奨** |
| SCReLU | Squared Clipped ReLU: `clamp(x, 0, QA)²` | 実験的 |
| PairwiseCReLU | Pairwise 乗算: `clamp(a) × clamp(b)` | 実験的 |

※ CReLU と SCReLU はファイルサイズが同じ。PairwiseCReLU は L1 入力次元が半分になるためサイズが異なる。

## 自動検出

nnue-pytorch で生成したファイルはヘッダーが不正確なことがあります。
rshogi はファイルサイズからアーキテクチャを自動検出します。

詳細: [nnue-architecture-detection.md](./nnue-architecture-detection.md)

## 互換性のある評価関数

| ソフト | Feature Set | アーキテクチャ |
|--------|-------------|---------------|
| 水匠 (suisho) | HalfKP | 256x2-32-32 |
| tanuki | HalfKP | 256x2-32-32 |
| AobaNNUE | HalfKP | 768x2-16-64 |
| bullet-shogi | HalfKA_hm | 全 L1 サイズ |
| nnue-pytorch (shogi) | HalfKP | 自動検出対応 |

## 新しいアーキテクチャの追加

新しい L1/L2/L3 の組み合わせを追加する場合:

1. `crates/rshogi-core/src/nnue/{feature_set}/l{L1}.rs` にバリアントを追加
2. `crates/rshogi-core/src/nnue/spec.rs` の `KNOWN_PAYLOADS` にエントリを追加

詳細: [nnue-architecture-detection.md](./nnue-architecture-detection.md)
