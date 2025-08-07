# ベンチマークガイド

このドキュメントでは、将棋AIエンジンの各種ベンチマークコマンドとその用途を説明します。

## ベンチマークの種類

### 1. 探索エンジンベンチマーク

#### shogi_benchmark
総合的な探索性能を測定します。

```bash
cargo run --release --bin shogi_benchmark
```

**測定内容**:
- 着手生成速度（moves/sec）
- 探索速度（NPS: Nodes Per Second）
- 5秒間の固定時間での探索ノード数

**出力例**:
```
Move Generation: 1,783,817 moves/sec
Search NPS: 5,302,243 nodes/sec
Total Nodes: 26,511,254
Total Time: 5.000秒
```

#### pv_simple_bench
PVテーブルの効果を測定する簡易ベンチマークです。

```bash
cargo run --release --bin pv_simple_bench
```

**測定内容**:
- 反復深化での各深さの探索時間
- 各深さでのPV（主要変化）の長さ
- 最終的なPVの内容

**特徴**:
- 深さ1から7まで段階的に探索
- PVの成長過程を観察可能

### 2. SEE（静的交換評価）ベンチマーク

#### see_bench
SEEアルゴリズムの詳細な性能測定を行います。

```bash
cargo bench --bench see_bench
```

**測定内容**:
- 単純な捕獲のSEE計算時間
- 複雑な交換のSEE計算時間
- X線攻撃を含む局面での性能
- 各種閾値での評価時間

**出力形式**: Criterionによる統計的分析結果

#### see_integration_bench
SEEの統合テストベンチマークです。

```bash
cargo bench --bench see_integration_bench
```

### 3. 評価関数ベンチマーク

#### nnue_benchmark
NNUE評価関数の性能を測定します（NNUE実装時に使用）。

```bash
cargo run --release --bin nnue_benchmark
```

**測定内容**:
- 評価関数の呼び出し速度
- 差分更新の効率
- メモリアクセスパターン

### 4. プロファイリング用ベンチマーク

#### see_flamegraph
フレームグラフ生成用のプロファイリングベンチマークです。

```bash
# フレームグラフ生成（要: cargo-flamegraph）
cargo flamegraph --bin see_flamegraph -o see_flamegraph.svg

# または直接実行
cargo run --release --bin see_flamegraph
```

**用途**:
- ボトルネックの特定
- 関数呼び出しの可視化
- 最適化ポイントの発見

## ベンチマーク実行のベストプラクティス

### 1. 環境準備
```bash
# リリースビルドの確認
cargo build --release

# システムの負荷を下げる
# 他のアプリケーションを終了
```

### 2. 複数回実行
```bash
# 5回実行して平均を取る例
for i in {1..5}; do
    echo "Run $i:"
    cargo run --release --bin shogi_benchmark
done | tee benchmark_results.txt
```

### 3. 結果の記録
- 実行日時
- コミットハッシュ
- ビルド設定
- システム環境

## パフォーマンス比較

### PVテーブル実装前後の比較

| 評価関数 | PVテーブル | NPS |
|---------|-----------|-----|
| Material | なし | 5,343,723 |
| Material | あり | 5,302,243 |
| NNUE | なし | 1,160,527 |
| NNUE | あり | （未測定） |

### 評価関数別の性能

| 評価関数 | 評価速度/秒 | 探索NPS |
|---------|-----------|---------|
| Material | 12,106,317 | 5,343,723 |
| NNUE | 1,140,383 | 1,160,527 |

## トラブルシューティング

### ベンチマークが遅い場合
1. リリースビルドか確認: `--release` フラグ
2. CPU周波数ガバナーを確認
3. 温度スロットリングの確認

### 結果が不安定な場合
1. バックグラウンドプロセスを停止
2. 複数回実行して平均を取る
3. より長い実行時間を設定

## ベンチマーク実行例と期待される出力

### NNUE性能ベンチマーク

```bash
cargo run --release --bin nnue_benchmark
```

期待される出力例（リリースビルド、2025年7月15日測定）：
```
=== NNUE Performance Benchmark ===

1. Direct Evaluation Function Comparison
========================================
Material Evaluator:
  - Evaluations/sec: 12,106,317
  - Avg time: 82 ns

NNUE Evaluator:
  - Evaluations/sec: 1,140,383
  - Avg time: 876 ns

Performance Comparison:
  - NNUE is 10.6x slower than Material evaluator
  - NNUE overhead: 961.6%

2. Search Performance Comparison
=================================
Position 1:
  Material Engine:
    Nodes: 26,718,665
    Time: 5.000009636s
    NPS: 5,343,723
    
  NNUE Engine:
    Nodes: 2,903,757
    Time: 2.502101829s
    NPS: 1,160,527
    
Search Comparison:
  Material NPS: 5,343,723
  NNUE NPS: 1,160,527
  NPS ratio: 4.60x
  NNUE search overhead: 78.3%
```

注: デバッグビルドでは約20倍遅くなります（NNUE評価関数: 約10,000 評価/秒）

### 5. 並列探索ベンチマーク

#### parallel_benchmark
並列探索の性能を包括的に測定します。

```bash
cargo run --release --bin parallel_benchmark
```

**測定内容**:
- 各スレッド数でのNPS（Nodes Per Second）
- スピードアップ（シングルスレッド比）
- 並列効率
- ノード重複率
- 停止レイテンシ
- PV（主要変化）の一貫性

**詳細**: [並列探索ベンチマークガイド](parallel-benchmark-guide.md)を参照

## 関連ドキュメント

- [並列探索ベンチマークガイド](parallel-benchmark-guide.md)
- [PVテーブルのパフォーマンス分析](analysis/pv-table-performance.md)
- [NNUE評価関数のパフォーマンス分析](analysis/nnue-performance.md)
- [SEEのパフォーマンス分析](analysis/see-performance.md)
- [プロファイリングガイド](profiling-guide.md)
- [CLAUDE.md](../../CLAUDE.md) - 開発時の品質チェックコマンド