# パフォーマンスドキュメント

このディレクトリには、将棋AIエンジンのパフォーマンス関連ドキュメントが含まれています。

## 構成

### ガイド
- [**benchmark-guide.md**](benchmark-guide.md) - 各種ベンチマークコマンドの使用方法
- [**profiling-guide.md**](profiling-guide.md) - flamegraphなどのプロファイリングツールのセットアップと使用方法
- [**tt-architecture**](../implementation/transposition-table.md) - 置換表（TT）設計と運用

### 分析結果
- [**analysis/**](analysis/) - 各機能のパフォーマンス分析結果
  - [nnue-performance.md](analysis/nnue-performance.md) - NNUE評価関数の性能分析
  - [pv-table-performance.md](analysis/pv-table-performance.md) - PVテーブル実装の性能分析
  - [see-performance.md](analysis/see-performance.md) - SEE（静的交換評価）の性能分析

### 統合テスト
- [**integration/**](integration/) - 統合テストとベンチマーク
  - [see-integration.md](integration/see-integration.md) - SEE統合テストフレームワーク

## クイックスタート

### 基本的なベンチマーク実行

```bash
# 総合探索ベンチマーク
cargo run --release --bin shogi_benchmark

# PVテーブル効果測定
cargo run --release --bin pv_simple_bench

# SEEベンチマーク
cargo bench --bench see_bench
```

### プロファイリング

```bash
# flamegraph生成
cargo flamegraph --bin see_flamegraph -o flamegraph.svg
```

詳細は各ドキュメントを参照してください。

## 診断機能（Diagnostics メタフィーチャー）

開発/検証用途のログ・計測を一括で有効化できます。

```bash
cargo run -p engine-usi --release --features diagnostics
```

含まれる機能:
- `tt_metrics`: 置換表（TT）詳細メトリクスの収集と要約出力（終局時に `info string tt_metrics ...`）。
- `nnue_telemetry`: 探索中に1秒間隔で NNUE経路のカウンタを`debug`レベルで出力。
- `pv_debug_logs`: PV構築・検証の詳細ログ（stderr）。

補足（USI出力の強化）:
- 探索中 `info` 行に `hashfull <permille>` を付与。
- 終局時、SinglePVでも `info multipv 1 ... hashfull ... pv ...` を必ず出力。

## パフォーマンス目標

- **探索速度**: 5M+ NPS (Material評価関数)
- **NNUE探索速度**: 1M+ NPS
- **SEE計算**: 2.5M+ 回/秒
- **メモリ使用量**: 最小限に抑える（PVテーブル: 64KB以下）

## 関連リンク

- [CLAUDE.md](../../CLAUDE.md) - 開発ガイドライン（品質チェック含む）
- [QUALITY.md](../../QUALITY.md) - 品質基準とテスト戦略
