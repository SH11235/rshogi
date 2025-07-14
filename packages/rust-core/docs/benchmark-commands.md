# ベンチマークコマンド一覧

## NNUE性能ベンチマーク

### 基本的なNNUE vs Material評価関数の比較
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

## SIMD実装ベンチマーク

### 各SIMD実装の詳細比較
```bash
cargo run --release --bin simd_benchmark
```

期待される出力例：
```
=== SIMD Implementation Benchmark ===

CPU Features:
  SSE4.1: true
  AVX2:   true

=== Affine Transform Benchmark ===
Scalar: 349 ms (285929 ops/sec)
SSE4.1: 130 ms (766585 ops/sec)
AVX2:   66 ms (1504957 ops/sec)

=== ClippedReLU Benchmark ===
Scalar: 0 ms (22727272727273 ops/sec)
SSE4.1: 23 ms (42713423 ops/sec)
AVX2:   14 ms (70649240 ops/sec)
```

### SSE4.1実装の個別テスト
```bash
cargo run --release --bin sse41_only_test
```

### SIMD実装の確認
```bash
cargo run --release --bin simd_check
```

## パフォーマンス結果サマリー（2025年7月15日）

### NNUE評価関数の直接評価性能
- **Material評価関数**: 12,106,317 評価/秒
- **NNUE評価関数**: 1,140,383 評価/秒
- **速度比**: NNUE は Material の 10.6倍遅い

### 探索での実効性能（NPS）
- **Material評価関数**: 5,343,723 NPS
- **NNUE評価関数**: 1,160,527 NPS  
- **速度比**: NNUE は Material の 4.6倍遅い
- **改善理由**: 探索中は評価関数の呼び出し頻度が低いため、オーバーヘッドが緩和される

### affine_transform（最重要関数）
- **スカラー**: 285,929 ops/sec
- **SSE4.1**: 766,585 ops/sec (2.68倍高速化)
- **AVX2**: 1,504,957 ops/sec (5.26倍高速化)

### 高速化の効果
- SSE4.1: スカラー比で約2.68倍高速（2008年以降のCPUで利用可能）
- AVX2: スカラー比で約5.26倍高速（2013年以降のCPUで利用可能）

## ビルドとテスト

### リリースビルド
```bash
cargo build --release
```

### ネイティブCPU最適化でのビルド
```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

### 全テストの実行
```bash
cargo test
```

### 特定のテストの実行
```bash
# SIMD正確性テスト
cargo test simd_correctness

# NNUE関連テストのみ
cargo test nnue
```

## プロファイリング

### 基本的な探索ベンチマーク
```bash
cargo run --release --bin shogi_benchmark
```

### メモリ使用量の確認
```bash
# Linux/macOSの場合
/usr/bin/time -v cargo run --release --bin nnue_benchmark

# または
valgrind --tool=massif cargo run --release --bin nnue_benchmark
```

## トラブルシューティング

### AVX2が使用されているか確認
1. `simd_check`を実行してCPU機能を確認
2. ベンチマーク結果のNPSが900K以上ならAVX2が使用されている

### SSE4.1のみの環境でテスト
現在の実装では実行時にCPU機能を自動検出するため、SSE4.1のみの環境では自動的にSSE4.1実装が使用されます。

### パフォーマンスが期待より低い場合
1. リリースビルドを使用しているか確認（`--release`フラグ）
2. CPU周波数が適切か確認（省電力モードになっていないか）
3. 他の重いプロセスが動作していないか確認
