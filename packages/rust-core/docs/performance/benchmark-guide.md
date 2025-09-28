# ベンチマークガイド

このドキュメントでは、将棋AIエンジンの各種ベンチマークコマンドとその用途を説明します。

> crate固有のベンチ（TT実装など）は各crateのドキュメントに詳細があります。
> engine-core の個別ベンチ一覧と実行条件は [crates/engine-core/docs/benchmarks.md](../../crates/engine-core/docs/benchmarks.md) を参照してください。

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

## Gauntlet Gate CI（昇格判定）

- ワークフロー: `.github/workflows/gauntlet-gate.yml`
- 入力重み: `runs/nnue_local/baseline.nnue` と `runs/nnue_local/candidate.nnue`
- Release asset: `GATE_BASELINE_TAG/GATE_BASELINE_ASSET` と `GATE_CANDIDATE_TAG/GATE_CANDIDATE_ASSET` で GitHub Release のアセット名を指定する（ワークフロー内で必ずダウンロードされる）
- 実行内容:
  - `target/release/gauntlet` を 200局（代表100局面×往復）で実行
  - Gate 条件: スコア率 55% 以上かつ NPS ±3% 以内
  - 固定パラメータ: `--pv-ms ${GAUNTLET_PV_MS}`（既定 300ms）で MultiPV 計測時間を安定化
  - 結果: `docs/reports/gauntlet/ci/<run_id>/` に JSON / Markdown / structured_v1 を保存し、Artifacts と Step Summary に出力
- 失敗時: Gate 判定が未達成、サンプル欠落（NPS ≥ 50 / PV ≥ 30 を下回る）、重み未取得などでジョブがエラー終了。詳細は `runs/gauntlet_gate/console.err` と Step Summary を参照

## ベンチマーク実行例と期待される出力

### NNUE性能ベンチマーク（固定ライン対応）

```bash
cargo run --release -p tools --bin nnue_benchmark -- --single-weights path/to/weights.bin
```

期待される出力例（リリースビルド、単スレ）:
```
=== NNUE Single Benchmark ===
Weights: path/to/weights.bin
Update-only EPS: refresh=1_230_000 apply=2_860_000 chain=2_710_000
Eval-included EPS: refresh=1_150_000 apply=2_100_000 chain=2_040_000
Speedup (apply/refresh eval): 1.83x
Speedup (chain/refresh eval): 1.77x
```

固定ラインモード（MoveGenの影響を排除した再現性の高いEPS）:

```bash
# 事前生成ライン（startpos + 手列指定）
cargo run --release -p tools --bin nnue_benchmark -- \
  --single-weights path/to/weights.bin \
  --fixed-line --startpos --moves "7g7f,3c3d,2g2f,8c8d" \
  --seconds 5 --warmup-seconds 2 \
  --json docs/reports/nnue_fixed_startpos.json \
  --report docs/reports/nnue_fixed_startpos.md

# 決定論ライン（seed で固定）
cargo run --release -p tools --bin nnue_benchmark -- \
  --single-weights path/to/weights.bin \
  --fixed-line --deterministic-line --startpos --seed 0xC0FFEE --length 128 \
  --seconds 5 --json -
```

出力指標（EPS）:
- Update-only 系: `refresh_update_eps`/`apply_update_eps`/`chain_update_eps`
- Eval-included 系: `refresh_eval_eps`/`apply_eval_eps`/`chain_eval_eps`

注: デバッグビルドでは約20倍遅くなります（NNUE評価関数: 約10,000 評価/秒）。比較は常にリリースビルド・単スレで実施してください。
また、実行秒数 `--seconds` が短すぎる（< 2秒）と分散が大きくなります。推奨は `--seconds >= 5` と `--warmup-seconds >= 2` です。

補足: `--seed` は10進と16進（`0x`/`0X`接頭辞）どちらの表記も受け付けます。

#### JSON比較（回帰検知）

固定ラインの JSON 出力同士を比較し、主指標（apply/chain の Update/Eval 系）の相対低下をチェックします。

```bash
cargo run --release -p tools --bin compare_nnue_bench -- \
  docs/reports/head.json docs/reports/base.json \
  --update-threshold -15 --eval-threshold -10 \
  --update-baseline-min 100000 --eval-baseline-min 50000 \
  --fail-on-warn
```

出力:
- JSON: 各指標の delta と warn を標準出力（stdout）
- WARN行: 人間可読の警告を標準エラー（stderr）に出力
- 既定閾値: Update 系 -15%、Eval 系 -10%（ベースが十分に大きいときのみ判定）
- --fail-on-warn: 警告があれば終了コード2で終了（CIゲート向け）

#### 探索中テレメトリのログ（開発時）
- feature `nnue_telemetry` 有効時、探索中に 1 秒ごと `kind=eval_path` / `kind=apply_refresh` を出力します（`RUST_LOG=debug`）。
- 複数スレッドからの同時 `process_events()` を単調時計（起動後の経過秒）でガードし、各秒につき1回のみ `snapshot_and_reset()` を実行します。
- サンプル:
  ```
  kind=eval_path	sec=12	ms=1206	acc=34567	fb=1234	fb_hash=1200	fb_empty=34	fb_feat_off=0	rate=96.6%
  kind=apply_refresh	sec=12	ms=1206	king=12	other=3	total=15
  ```
  - `acc` は差分acc経路の評価回数、`fb_*` はフォールバック理由別の回数です。
  - `king/other` は差分適用が安全側 refresh になった件数（王手・玉移動/その他）です。

有効化例（エンジン実行時）:
```bash
RUST_LOG=debug cargo run --release -p engine-usi
```

注: `nnue_telemetry` はオーバーヘッドを抑えた軽量な集計（Relaxedの原子加算/スワップ）ですが、本番計測ではオフにすることを推奨します。

### 診断系の有効化（開発時）

探索ログやTTメトリクスをまとめて確認したい場合、メタフィーチャー`diagnostics`を利用してください。

```bash
cargo run -p engine-usi --release --features diagnostics -- \
  # 例: 固定時間でのテスト
  <<USI commands>>
```

有効時の目印:
- 探索中の`info`行に`hashfull`が付与されます。
- 終局時、`info multipv 1 ... hashfull ... pv ...`が必ず出力されます。
- `tt-metrics`の要約が`info string tt_metrics ...`として出力されます。

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
