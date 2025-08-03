# SIMD Architecture Design Document

## 概要
本ドキュメントは、Shogi EngineにおけるSIMD最適化のアーキテクチャと設計判断を記録する。

## 現在のSIMD実装

### 1. NNUE SIMD (`/src/evaluation/nnue/simd/`)
**目的**: ニューラルネットワーク評価関数の高速化

**主要関数**:
- `affine_transform_avx2/sse41` - 行列積演算
- `clipped_relu_avx2/sse41` - 活性化関数
- `transform_features_avx2/sse41` - 特徴変換
- `update_accumulator_avx2/sse41` - アキュムレータ更新

**特徴**:
- 8/16/32ビット整数の大量演算
- タイリング最適化（8x32タイル）
- ベクトル・行列演算に特化

### 2. TT SIMD (`/src/search/tt_simd.rs`)
**目的**: トランスポジションテーブルのアクセス高速化

**主要関数**:
- `find_matching_key_avx2/sse2` - 64ビットキー検索
- `calculate_priority_scores_avx2` - 優先度スコア計算

**特徴**:
- 64ビット整数の比較演算
- 4エントリ（バケット）単位の処理
- キャッシュライン最適化

### 3. 共通インフラ (`/src/simd/`)
**目的**: SIMD実装間で共有される基盤機能

**提供機能**:
- CPU機能検出（AVX2, SSE4.1, SSE2）
- SIMD レベル自動選択
- アライメントユーティリティ

## 設計判断

### なぜNNUEとTTのSIMDを分離するか？

#### 1. **処理特性の違い**
| 項目 | NNUE | TT |
|------|------|-----|
| データ型 | i8, i16, i32 | u64 |
| 演算 | 積和、型変換 | 比較、マスク抽出 |
| データ量 | 数千～数万要素 | 4要素（バケット） |
| メモリパターン | ストリーミング | ランダムアクセス |

#### 2. **SIMD命令セットの違い**
- **NNUE**: `_mm256_madd_epi16`, `_mm256_cvtepi8_epi16`
- **TT**: `_mm256_cmpeq_epi64`, `_mm256_movemask_epi8`

共通の命令はほとんど使用されない。

#### 3. **最適化戦略の違い**
- **NNUE**: スループット重視、レジスタ利用最大化
- **TT**: レイテンシ重視、キャッシュヒット率向上

### 共通化すべき部分

以下の機能のみ共通インフラとして提供：

1. **CPU機能検出**
   - Runtime feature detection
   - SIMD level selection

2. **基本ユーティリティ**
   - Alignment helpers
   - Vector size constants

3. **ベンチマーク基盤**
   - 性能測定フレームワーク
   - SIMD vs Scalar比較

## パフォーマンス指標

### NNUE
- **AVX2**: スカラー比 3-4倍高速
- **SSE4.1**: スカラー比 2-3倍高速

### TT
- **AVX2 (key search)**: スカラー比 最大4倍高速
- **SSE2 (key search)**: スカラー比 2倍高速
- **Priority calculation**: 2-3倍高速

## 今後の拡張計画

### Phase 1: 既存最適化の改善
- [ ] TT bucketへのSIMD統合
- [ ] NNUE incremental updateの最適化

### Phase 2: 新規SIMD適用領域
- [ ] Move generation（駒の移動生成）
- [ ] Bitboard operations
- [ ] SEE (Static Exchange Evaluation)

### Phase 3: アーキテクチャ拡張
- [ ] ARM NEON support
- [ ] WebAssembly SIMD
- [ ] AVX-512 (将来的に)

## ベストプラクティス

1. **Safety First**
   - `unsafe`ブロックには詳細なSafetyコメント
   - Debug assertionsで境界チェック

2. **Fallback必須**
   - すべてのSIMD実装にスカラー版を用意
   - Runtime CPU feature detection

3. **テスト戦略**
   - SIMD vs Scalar一致性テスト
   - Random input testing
   - Boundary condition tests

4. **ドキュメント**
   - 各SIMD関数にSafety要件を明記
   - パフォーマンス特性を記録

## 参考資料
- [Intel Intrinsics Guide](https://www.intel.com/content/www/us/en/docs/intrinsics-guide/)
- [Agner Fog's Optimization Manuals](https://www.agner.org/optimize/)
- [NNUE Architecture](https://github.com/official-stockfish/nnue-pytorch)