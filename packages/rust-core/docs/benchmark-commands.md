# ベンチマークコマンド一覧

## NNUE性能ベンチマーク

### 基本的なNNUE vs Material評価関数の比較
```bash
cargo run --release --bin nnue_benchmark
```

期待される出力例：
```
=== NNUE Performance Benchmark ===

Position 1:
  Material Engine:
    Nodes: 27358274
    Time: 5.000008537s
    NPS: 5471645
    
  NNUE Engine:
    Nodes: 2903757
    Time: 3.161356062s
    NPS: 918516
    
Comparison:
  Material NPS: 5471645
  NNUE NPS: 918516
  NNUE overhead: 83.2%
```

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

## パフォーマンス結果サマリー

### NNUE評価関数
- **AVX2実装**: 918,516 NPS
- **SSE4.1実装（推定）**: 550,000-650,000 NPS
- **スカラー実装（推定）**: 300,000 NPS

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