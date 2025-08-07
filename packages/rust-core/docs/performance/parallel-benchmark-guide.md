# 並列探索ベンチマークガイド

このガイドでは、並列探索のパフォーマンスを測定・監視するためのベンチマークツールについて説明します。

## クイックスタート

### 基本的なベンチマークの実行

```bash
# デフォルト設定での実行（1,2,4,8スレッド、深さ10）
cargo run --release --bin parallel_benchmark

# カスタム設定での実行
cargo run --release --bin parallel_benchmark -- \
  --threads 1,2,4 \
  --depth 8 \
  --output results.json
```

### Criterionベンチマークの実行

```bash
# すべての並列探索ベンチマークを実行
cargo bench --bench parallel_search_benchmarks

# 特定のベンチマークを実行
cargo bench --bench parallel_search_benchmarks -- parallel_search/depth_8
```

## ベンチマークツール

### 1. `parallel_benchmark` - 包括的な並列探索ベンチマーク

**機能:**
- 複数のスレッド構成でのテスト
- NPS、スピードアップ、効率の測定
- 重複率の追跡
- 停止レイテンシの測定
- PV（主要変化）の一貫性チェック

**オプション:**
- `-t, --threads <THREADS>` - テストするスレッド数（カンマ区切り）
- `-d, --depth <DEPTH>` - 探索深さ [デフォルト: 10]
- `-p, --positions <FILE>` - 局面ファイル（JSON形式） [デフォルト: `crates/engine-core/resources/benchmark_positions.json`]
- `-o, --output <FILE>` - 結果の出力ファイル
- `--baseline <FILE>` - 比較用のベースラインファイル
- `--tolerance <PERCENT>` - 回帰許容値 [デフォルト: 2.0]
- `--skip-stop-latency` - 停止レイテンシ測定をスキップ

### 2. `benchmark_compare` - ベンチマーク結果の比較

**使用方法:**
```bash
cargo run --release --bin benchmark_compare -- \
  baseline.json \
  current.json \
  --tolerance 2.0 \
  --format markdown
```

**出力形式:**
- `text` - 人間が読みやすいテキスト出力（デフォルト）
- `json` - さらなる処理のためのJSON形式
- `markdown` - PRコメント用のGitHub風味のマークダウン

## パフォーマンス目標

ベンチマークでは以下のパフォーマンス目標をチェックします：

| 目標 | 要件 | 説明 |
|------|------|------|
| NPS(4T) | ≥ 2.4× | シングルスレッド比での4スレッドスピードアップ |
| 重複率 | ≤ 35% | ノードの重複探索率 |
| PVマッチ | ≥ 97% | 主要変化の一貫性 |
| 停止レイテンシ | ≤ 5ms | 時間制御の精度 |

## ベンチマーク局面フォーマット

局面ファイルはJSON形式を使用します。標準的なベンチマーク局面セットは `crates/engine-core/resources/benchmark_positions.json` に用意されています。

```json
{
  "positions": [
    {
      "name": "startpos",
      "sfen": "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
      "category": "opening"
    },
    {
      "name": "midgame_complex",
      "sfen": "ln1g1g1nl/1ks4r1/1pppp1bpp/p3spp2/9/P1P1P4/1P1PSPPPP/1BK1GS1R1/LN3G1NL b - 17",
      "category": "midgame"
    }
  ]
}
```

### カテゴリ

- `opening`: 序盤の局面
- `midgame`: 中盤の局面
- `endgame`: 終盤の局面
- `tactical`: 戦術的な局面（詰み、フォーク、ピンなど）

## CI統合

### GitHub Actions

ベンチマークは以下のタイミングで自動実行されます：
- mainブランチへのプッシュ時
- Rustコードを変更するプルリクエスト
- 毎日のスケジュール実行
- 手動のワークフローディスパッチ

### 回帰検出

CIは以下をチェックします：
1. パフォーマンス目標の違反
2. 許容値（デフォルト2%）を超える回帰
3. ベースライン結果との比較

## 結果の解釈

### 主要メトリクス

1. **NPS（Nodes Per Second）**
   - 絶対的な探索速度
   - スレッド数に応じてスケールすべき

2. **スピードアップ**
   - シングルスレッド性能に対する比率
   - 理想: リニア（4スレッド = 4倍のスピードアップ）
   - 現実的: 4スレッドで2.4倍

3. **効率**
   - スピードアップをスレッド数で割った値
   - 並列オーバーヘッドを測定
   - 目標: 4スレッドで60%以上

4. **重複率**
   - 冗長なノード探索の割合
   - 低いほど良い
   - 目標: 35%未満

5. **停止レイテンシ**
   - 時間制限での停止時のオーバーシュート
   - 時間制御にとって重要
   - 目標: 5ms未満

### 出力例

```
=== 並列探索ベンチマーク結果 ===

スレッド |      NPS | スピードアップ | 効率   | 重複% | 停止レイテンシ | PVマッチ%
---------|----------|----------------|--------|-------|----------------|----------
       1 |   100000 |          1.00x | 100.0% |   0.0 |         0.0ms |    100.0%
       2 |   180000 |          1.80x |  90.0% |  10.0 |         1.5ms |     98.0%
       4 |   250000 |          2.50x |  62.5% |  25.0 |         2.0ms |     97.5%
       8 |   400000 |          4.00x |  50.0% |  30.0 |         3.0ms |     97.0%

=== パフォーマンス目標 ===
NPS(4T) ≥ 2.4×: ✅
重複率 ≤ 35%: ✅
PVマッチ ≥ 97%: ✅
停止レイテンシ ≤ 5ms: ✅
```

## トラブルシューティング

### ベンチマークが遅い場合

- `--depth` で探索深さを減らす
- テストする局面を減らす
- `--skip-stop-latency` を使用してレイテンシテストをスキップ

### 重複率が高い場合

- Lazy SMP実装を確認
- 深さスキッピングロジックを検証
- ABDADA改善を検討

### スケーリングが悪い場合

- ロック競合を確認
- 共有データ構造を検証
- `perf` などのツールでプロファイル

## 開発

### 新しいベンチマークの追加

1. `benchmark_positions.json` に局面を追加
2. `metrics.rs` に新しいメトリクスを実装
3. 必要に応じてCriterionベンチマークを追加
4. CI設定を更新

### プロファイリング

```bash
# CPUプロファイリング
perf record -g cargo run --release --bin parallel_benchmark -- --threads 4 --depth 8
perf report

# スレッドサニタイザ
RUSTFLAGS="-Z sanitizer=thread" cargo +nightly run --bin parallel_benchmark
```

## 関連ドキュメント

- [総合ベンチマークガイド](benchmark-guide.md)
- [プロファイリングガイド](profiling-guide.md)
- [Lazy SMP実装計画](../lazy-smp-implementation-plan.md)
- [フェーズ3実装計画](../phase3-implementation-plan.md)