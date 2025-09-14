# USI エンジン（engine-usi）ビルド・運用ガイド

## 概要
`engine-usi` は `engine-core` の機能をフィーチャーで切り替え可能な薄い USI バイナリです。対局・大会・検証の用途に応じて、ビルド時フィーチャーと `RUSTFLAGS` を調整してください。

## 基本
```bash
# 推奨（ネイティブ最適化）
RUSTFLAGS="-C target-cpu=native" cargo run -p engine-usi --release
```

- 起動時に `info string core_features=engine-core:...` を出力（有効フィーチャーの確認用）。
- 最強設定は USI から `setoption name EngineType value EnhancedNnue` を指定。
- NNUE 重みは `setoption name EvalFile value /path/to/weights.nnue`。

## フィーチャー（engine-usi → engine-core 伝播）
- 任意ON（用途別）
  - `fast-fma` → `engine-core/nnue_fast_fma`（FMAで加算高速化、丸め微差を許容）
  - `diff-agg-hash` → `engine-core/diff_agg_hash`（差分集計のHashMap実装をA/B）
  - `nnue-telemetry` → `engine-core/nnue_telemetry`（軽量テレメトリ）
  - `tt-metrics`, `ybwc`, `nightly`（必要に応じて）

注: `nnue_single_diff`（SINGLE 差分NNUE）は恒久化され、常時有効です。ビルド時の切替は不要になりました。

### 例
```bash
# 差分NNUE + FMA
RUSTFLAGS="-C target-cpu=native" cargo run -p engine-usi --release --features fast-fma

# 注: fp32 行加算用 SIMD は Dispatcher に統合済みで常時ON（ランタイム検出: AVX/FMA/SSE2/NEON/Scalar）。`simd` フィーチャは不要です。

# Hash集計A/B（大量差分ケースで比較）
RUSTFLAGS="-C target-cpu=native" cargo run -p engine-usi --release --features diff-agg-hash
```

## USI オプション（抜粋）
- `EngineType`: Material / Enhanced / Nnue / EnhancedNnue（推奨：EnhancedNnue）
- `USI_Hash`: 1–1024（MB）
- `Threads`: 1–256
- `EvalFile`: 学習済みNNUE重みファイルのパス
- `ByoyomiPeriods`: 秒読み回数（`USI_ByoyomiPeriods` エイリアスも可）

## トラブルシュート
- FMA で評価が一致しない: 期待通りです（丸め差）。`fast-fma` を外すか、FMA を含む経路同士で比較・検証してください。

## ベンチの参考
- 固定ラインベンチ（`nnue_benchmark`）で UpdateOnly / EvalOnly / Update+Eval を分離測定
- `RUSTFLAGS="-C target-cpu=native"`、スレッドピニング（`taskset`）推奨
- 起動時の `core_features` をログへ保存し、再現性を担保
