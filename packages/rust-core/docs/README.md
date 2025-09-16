# Shogi Engine Documentation

Shogi AIエンジン (rust-core) の技術ドキュメント集です。

## 📚 カテゴリ別ドキュメント

### 🚀 Getting Started
- [**エンジンタイプ選択ガイド**](engine-types-guide.md) - 4種類のエンジンタイプの選択指針
- [**デバッグポジションツール**](debug-position-tool.md) - 特定局面の調査・デバッグツール
- [**生成→学習→ログ→ガントレット（ハンズオン）**](guides/pipeline_walkthrough.md) - 最短で一周回すための手順書

### 📜 Charter & Plans
- [**00_charter**](00_charter.md) - 測定条件・昇格Gate・データ/ログの横断契約
- [**10_pipeline（運用改善）**](10_pipeline.md) - P2/P3 の残タスク計画（ガントレット/Gate 連携）
- [**20_engine（棋力向上）**](20_engine.md) - B/C 以降の残タスク（Phase 1: Classic NNUE）
- [**ガントレット後フォローアップ計画**](plans/gauntlet_followup.md) - 直近結果を踏まえた次のアクション（モデル改善の本筋）

### 📑 Specs & Schemas
- Specs: [011 LRスケジュール](specs/011_lr_schedule.md) / [012 重み付け](specs/012_weighting.md) / [013 ガントレット](specs/013_gauntlet.md) / [017 生成ストリーミング](specs/017_generate_streaming.md)
- Schemas: [structured_v1](schemas/structured_v1.schema.json) / [gauntlet_out](schemas/gauntlet_out.schema.json) / [manifest_v2](schemas/manifest_v2.schema.json)

### 🧪 Fixtures（CI/手元検証用）
- JSONL（structured_v1 サンプル）: `docs/reports/fixtures/jsonl_sample.jsonl`
- PSV（10行サンプル）: `docs/reports/fixtures/psv_sample.psv`
- 開幕ブック（代表/アンチ）:
  - `docs/reports/fixtures/opening/representative.epd`
  - `docs/reports/fixtures/opening/anti.epd`

使用例（抜粋）:
- Gauntlet（代表ブック版）: `--book docs/reports/fixtures/opening/representative.epd`
- Streaming smoke: `cat docs/reports/fixtures/psv_sample.psv | <generate_cmd> ...`

### 🏗️ Architecture & Design
- [**統一探索フレームワーク設計**](unified-searcher-design.md) - const genericsを活用した探索エンジン設計
- [**ABDADA実装**](abdada-implementation.md) - 並列探索の重複作業削減技術
- [**座標系の説明**](coordinate-system.md) - 将棋盤の座標表現
- [**SIMD アーキテクチャ**](simd-architecture.md) - SIMD最適化の設計
- [**ゲームフェーズモジュール**](../crates/engine-core/docs/game-phase-module-guide.md) - 統一されたゲームフェーズ検出システム

### 📊 Performance & Benchmarking
- [**パフォーマンスドキュメント総合**](performance/README.md) - パフォーマンス関連ドキュメントのインデックス
- [**ベンチマークガイド**](performance/benchmark-guide.md) - 各種ベンチマークツールの使用方法
- [**並列探索ベンチマーク**](performance/parallel-benchmark-guide.md) - 並列探索性能測定ツール
- [**プロファイリングガイド**](performance/profiling-guide.md) - flamegraph等のプロファイリング手法
- [**ベースライン管理**](benchmark-baseline-guide.md) - ベンチマーク結果の継続的管理

### 🔧 Development
- [**並列探索改善計画**](parallel-search-improvement.md) - Lazy SMP探索の改善実装記録
- [**TDD完全ガイド**](development/tdd-complete-guide.md) - テスト駆動開発の実践ガイド
- [**AIテストカバレッジ計画**](development/ai-test-coverage-plan.md) - AI機能のテスト戦略

### 🛠️ Tools
- [**Opening Book ツール**](tools/opening-book-tools-guide.md) - 定跡データ変換・検証ツール
- [**NNUE 教師データ生成**](tools/nnue-training-data-guide.md) - generate_nnue_training_data の運用ガイド（構造化ログ/manifest v2）
- [**曖昧掘りオーケストレーション**](tools/ambiguous-mining-orchestrator.md) - 抽出→再注釈→マージを1コマンドで実行（系譜/整合性を記録）
 - [**PSV→JSONL 直変換（psv2jsonl）**](tools/psv2jsonl.md) - YaneuraOu PSV(yo_v1) を学習用 JSONL にストリーム変換

### 📝 Implementation Notes
- [**Rustプリプロセッシング計画**](implementation/rust-preprocessing-scripts-plan.md) - Rust実装の計画文書

### 📖 Reference
- [**YaneuraOu SFEN形式**](reference/yaneuraou-sfen-format.md) - SFEN形式の仕様
- [**Manifest v2（NNUE教師生成）**](reference/manifest_v2.md) - 親/partの責務、summaryのrunスコープ、K=3メトリクス

### 🔬 Performance Analysis
- [**NNUE性能分析**](performance/analysis/nnue-performance.md) - NNUE評価関数の性能分析
- [**PVテーブル性能**](performance/analysis/pv-table-performance.md) - 主要変化テーブルの性能
- [**SEE性能分析**](performance/analysis/see-performance.md) - 静的交換評価の性能
- [**SEE統合テスト**](performance/integration/see-integration.md) - SEE統合テストフレームワーク

### 💾 Transposition Table
- [**TT最適化サマリー**](performance/tt-optimization-summary.md) - CAS最適化、Prefetch分析、性能改善の統合記録

## 📈 ドキュメント状態

| カテゴリ | ドキュメント | 状態 | 最終更新 | 備考 |
|---------|------------|------|----------|------|
| **Architecture** | unified-searcher-design.md | ✅ Active | 2025-08 | 実装完了 |
| **Architecture** | abdada-implementation.md | ✅ Active | 2025-08 | 実装済み |
| **Architecture** | game-phase-module-guide.md | ✅ Active | 2025-08 | Phase 4実装完了 |
| **Performance** | parallel-benchmark-guide.md | ✅ Active | 2025-08-09 | 新機能反映済み |
| **Performance** | parallel-search-improvement.md | ✅ Completed | 2025-08-09 | Phase 6まで完了 |
| **Performance** | tt-optimization-summary.md | ✅ Active | 2025-08-09 | 3文書を統合 |
| **Tools** | debug-position-tool.md | ✅ Active | 2025-08 | CLAUDE.mdに記載 |
| **Tools** | opening-book-tools-guide.md | ✅ Active | 2025-07 | 実装完了 |

## 🔧 主要ツール

### ベンチマークツール
```bash
# 並列探索ベンチマーク（推奨）
cargo run --release --bin parallel_benchmark -- \
  --threads 1,2,4 \
  --fixed-total-ms 1000 \
  --dump-json results.json

# 汎用探索ベンチマーク  
cargo run --release --bin shogi_benchmark

# Lazy SMPベンチマーク
cargo run --release --bin lazy_smp_benchmark
```

### デバッグツール
```bash
# 特定局面の調査
cargo run --release --bin debug_position -- \
  --sfen "SFEN文字列" \
  --depth 10 \
  --engine enhanced_nnue
```

### プロファイリング
```bash
# Flamegraph生成
cargo flamegraph --bin see_flamegraph -o flamegraph.svg
```

## 📋 開発ガイドライン

開発時は以下のドキュメントも参照してください：

- [**CLAUDE.md**](../CLAUDE.md) - Claude Code向けの開発ガイドライン
- [**Cargo.toml**](../Cargo.toml) - プロジェクト設定

## 🔄 更新履歴

| 日付 | 内容 |
|------|------|
| 2025-08-09 | ドキュメント全体を再構成、カテゴリ別に整理 |
| 2025-08-08 | parallel_benchmarkツールに統計機能・JSON出力追加 |
| 2025-07 | Opening Book関連ドキュメント統合 |

## 📌 メンテナンス方針

- 実装と乖離したドキュメントは速やかに更新または削除
- 関連する複数のドキュメントは適切に統合
- 新機能実装時は対応するドキュメントも同時に更新
- 定期的にドキュメントの状態を確認し、この README を更新
