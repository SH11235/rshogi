# 並列探索ベンチマークガイド

このガイドでは、並列探索のパフォーマンスを測定・監視するための `parallel_benchmark` ツールについて説明します。

## クイックスタート

### 基本的なベンチマークの実行

```bash
# デフォルト設定での実行（1,2,4スレッド、深さ8）
cargo run --release --bin parallel_benchmark

# カスタム設定での実行
cargo run --release --bin parallel_benchmark -- \
  --threads 1,2,4 \
  --depth 8 \
  --fixed-total-ms 1000 \
  --dump-json results.json
```

## `parallel_benchmark` ツール詳細

### 概要

`parallel_benchmark` は並列探索の包括的なパフォーマンス測定ツールです。複数のスレッド構成でのテストを実行し、統計情報の収集、JSON形式での結果保存、回帰検知などの機能を提供します。

### 主要機能

- **マルチスレッド性能測定**: 1〜Nスレッドでの性能比較
- **統計情報収集**: NPS、標準偏差、外れ値比率、重複率などを計測
- **環境情報記録**: CPU情報、git commit、rustcバージョンなどを自動収集
- **JSON出力**: CI統合や長期的な性能追跡のための構造化データ出力
- **回帰検知**: ベースラインとの比較による性能劣化の自動検出
- **詳細な性能分析**: 実効スピードアップ、TTヒット率、PV一貫性などの高度なメトリクス

### コマンドラインオプション

```bash
cargo run --release --bin parallel_benchmark -- [OPTIONS]
```

| オプション | 説明 | デフォルト |
|-----------|------|------------|
| `-t, --threads <THREADS>` | テストするスレッド数（カンマ区切り） | `1,2,4` |
| `-d, --depth <DEPTH>` | 探索深さ | `8` |
| `-m, --fixed-total-ms <MS>` | 固定探索時間（ミリ秒）※深さ指定より優先 | なし |
| `-i, --iterations <N>` | 各局面の反復回数 | `3` |
| `--tt-size <MB>` | トランスポジションテーブルサイズ | `256` |
| `-p, --positions <FILE>` | 局面ファイル（JSON形式） | `benchmark_positions.json` |
| `-s, --skip-positions <INDICES>` | スキップする局面番号（カンマ区切り） | なし |
| `--material` | Material評価関数を使用 | false |
| `--sharded-tt` | シャード化TTを使用 | false |
| `--dump-json <FILE>` | 結果をJSON形式で保存 | なし |
| `--baseline <FILE>` | 比較用ベースラインJSONファイル | なし |
| `--strict` | 回帰検知時にexit(1)で終了 | false |
| `--log-level <LEVEL>` | ログレベル（debug/info/warn/error） | `info` |

### 使用例

#### 1. 基本的なベンチマーク実行

```bash
# シンプルな実行
cargo run --release --bin parallel_benchmark

# 詳細ログ付き
RUST_LOG=info cargo run --release --bin parallel_benchmark
```

#### 2. CI/CD統合用

```bash
# JSON出力付きベンチマーク
cargo run --release --bin parallel_benchmark -- \
  --threads 1,2,4,8 \
  --fixed-total-ms 1000 \
  --iterations 5 \
  --dump-json benchmark-$(date +%Y%m%d-%H%M%S).json

# ベースラインとの比較（回帰検知）
cargo run --release --bin parallel_benchmark -- \
  --threads 1,2,4 \
  --fixed-total-ms 1000 \
  --baseline baseline.json \
  --dump-json current.json \
  --strict  # 回帰があれば失敗
```

#### 3. 特定局面でのテスト

```bash
# 最初の3局面のみテスト
cargo run --release --bin parallel_benchmark -- \
  --skip-positions 3,4,5,6,7,8,9,10,11,12,13,14,15,16 \
  --iterations 10 \
  --threads 1,2
```

## 出力形式

### コンソール出力

```
Performance Summary for 2 thread(s):
  NPS: 320478 ± 155696
  Duplication: 32.0%
  Effective Speedup: 0.60x
  TT Hit Rate: 0.0%

=== BENCHMARK SUMMARY ===
Threads | NPS      | Speedup | Efficiency | Duplication | Effective
--------|----------|---------|------------|-------------|----------
      1 |   410550 |    1.00x |     100.0% |        16.3% |     1.00x
      2 |   320478 |    0.78x |      39.0% |        32.0% |     0.60x

=== TARGET STATUS ===
2T Speedup (≥1.25x): ✗ (actual: 0.78x)
4T Speedup (≥1.8x): ✗ (actual: 0.57x)
Duplication (<50%): ✓
```

### JSON出力形式

```json
{
  "metadata": {
    "version": "0.1.0",
    "commit_hash": "abc123def456",
    "cpu_info": {
      "model": "AMD Ryzen 9 5950X",
      "cores": 16,
      "threads": 32,
      "cache_l3": "64MB"
    },
    "build_info": {
      "profile": "release",
      "features": ["parallel"],
      "rustc_version": "rustc 1.88.0"
    },
    "timestamp": 1754699601,
    "config": {
      "tt_size_mb": 256,
      "num_threads": [1, 2, 4],
      "depth_limit": 8,
      "iterations": 3,
      "positions_count": 10
    }
  },
  "results": [
    {
      "thread_count": 1,
      "mean_nps": 410550.0,
      "std_dev": 131777.16,
      "outlier_ratio": 0.0,
      "avg_speedup": 1.0,
      "avg_efficiency": 1.0,
      "duplication_percentage": 16.31,
      "effective_speedup": 1.0,
      "tt_hit_rate": 0.0,
      "pv_consistency": 0.0
    }
  ]
}
```

## パフォーマンス目標

ベンチマークでは以下のパフォーマンス目標をチェックします：

| メトリクス | 目標値 | 説明 |
|-----------|--------|------|
| **2T Speedup** | ≥1.25x | 2スレッドでのスピードアップ |
| **4T Speedup** | ≥1.8x | 4スレッドでのスピードアップ |
| **Duplication** | <50% | ノード探索の重複率 |
| **TT Hit Rate** | >20% | トランスポジションテーブルのヒット率 |
| **Efficiency** | >60% (2T), >40% (4T) | 並列化効率 |

## 回帰検知

### ベースライン管理

```bash
# 初回：ベースラインを作成
cargo run --release --bin parallel_benchmark -- \
  --dump-json baseline.json

# 以降：ベースラインと比較
cargo run --release --bin parallel_benchmark -- \
  --baseline baseline.json \
  --dump-json current.json

# CI用：厳密モード
cargo run --release --bin parallel_benchmark -- \
  --baseline baseline.json \
  --strict  # 5%以上の性能劣化でexit(1)
```

### 回帰検知基準

- **速度低下**: 実効スピードアップが5%以上低下
- **重複率増加**: 重複率が10%以上増加

## ベンチマーク局面フォーマット

局面ファイルは JSON 形式を使用します。標準的なベンチマーク局面セットは `crates/engine-core/resources/benchmark_positions.json` に用意されています。

```json
[
  {
    "name": "startpos",
    "sfen": "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
  },
  {
    "name": "midgame_complex",
    "sfen": "ln1g1g1nl/1ks4r1/1pppp1bpp/p3spp2/9/P1P1P4/1P1PSPPPP/1BK1GS1R1/LN3G1NL b - 17"
  }
]
```

## トラブルシューティング

### メモリ不足

```bash
# TTサイズを減らす
cargo run --release --bin parallel_benchmark -- --tt-size 64
```

### 測定のばらつきが大きい

```bash
# イテレーション数を増やす
cargo run --release --bin parallel_benchmark -- --iterations 10

# 固定時間を長くする
cargo run --release --bin parallel_benchmark -- --fixed-total-ms 5000
```

### デバッグ情報が必要

```bash
# デバッグログを有効化
RUST_LOG=debug cargo run --release --bin parallel_benchmark

# 特定局面の詳細調査
cargo run --release --bin debug_position -- \
  --sfen "問題のSFEN文字列" \
  --depth 8
```

## 関連ツール

- [**lazy_smp_benchmark**](../README.md) - Lazy SMP専用ベンチマーク
- [**shogi_benchmark**](benchmark-guide.md) - 汎用探索ベンチマーク
- [**debug_position**](../debug-position-tool.md) - 特定局面の詳細調査

## 更新履歴

| 日付 | バージョン | 変更内容 |
|------|-----------|----------|
| 2025-08-09 | 1.1.0 | 統計情報機能、JSON出力、回帰検知を追加 |
| 2025-08-08 | 1.0.0 | 重複率追跡機能を追加 |
