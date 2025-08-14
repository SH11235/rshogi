# ベンチマーク

このドキュメントでは、engine-coreのベンチマークの実行方法と結果を記載します。
> 全体的なベンチマークの使い方・基礎（Criterion の共通オプション、ベースライン管理 等）は上位ガイドを参照してください。
> [ベンチマークガイド（共通）](../../../docs/performance/benchmark-guide.md)

## ベンチマークの実行方法

### 基本的な実行

```bash
# すべてのベンチマークを実行
cargo bench --bench search_benchmarks

# 特定のベンチマークのみ実行
cargo bench --bench search_benchmarks basic_searcher/depth_fixed

# ベースラインとして保存
cargo bench --bench search_benchmarks -- --save-baseline my_baseline

# ベースラインと比較
cargo bench --bench search_benchmarks -- --baseline my_baseline
```

### クイック実行（開発時）

```bash
# サンプル数を減らして高速実行
cargo bench --bench search_benchmarks -- --quick

# タイムアウト付きで実行（長時間化を防ぐ）
timeout 180s cargo bench --bench search_benchmarks
```

## ベンチマーク項目

### 1. 深さ固定探索（depth_fixed）
指定された深さまで探索を行い、処理時間を測定します。

### 2. 時間固定探索（time_fixed）
指定された時間（10ms）で探索を行い、処理時間を測定します。

### 3. ノード数測定（node_counting）
一定時間内に探索できるノード数を測定します。

### 4. 置換表性能（tt_performance）
置換表（Transposition Table）のプローブ性能を測定します。

## テスト局面

- **startpos**: 開始局面（depth=4）
- **midgame**: 中盤局面（depth=3）
- **endgame**: 終盤局面（depth=3）
- **tactical**: 戦術的局面（depth=3）

## Phase 4完了時点のベンチマーク結果（UnifiedSearcher統合後）

### パフォーマンス比較（開始局面、深さ4）

| エンジンタイプ | 実行時間 | 相対性能 |
|---|---:|---:|
| BasicSearcher（オリジナル） | 138ms | 1.0x（基準） |
| UnifiedSearcher<_, true, false, 8>（基本設定） | 139ms | 1.0x |
| EnhancedSearcher（オリジナル） | 3.8ms | 36.3x |
| UnifiedSearcher<_, true, true, 16>（拡張設定） | 7.3ms | 18.9x |

### 主要な発見

1. **ゼロコスト抽象化の確認**
   - UnifiedSearcherの基本設定（pruning無効）はBasicSearcherとほぼ同じ性能
   - const genericsによるコンパイル時最適化が効いている

2. **枝刈りの効果**
   - 拡張設定では約19倍の高速化
   - オリジナルのEnhancedSearcherより若干遅いが、これは実装の違いによるもの

3. **統合の成功**
   - 単一のコードベースで異なる探索戦略を実現
   - 実行時オーバーヘッドなし

## Phase 3完了時点のベンチマーク結果

### BasicSearcher（基本的なアルファベータ探索）

#### 深さ固定探索
- **開始局面（depth=4）**: 約138ms
  - 安定した性能で深さ4まで探索可能
- **中盤局面（depth=3）**: 約57ms
  - 複雑な局面でも高速に処理
- **終盤局面（depth=3）**: 約154ms
  - 持ち駒が多いため時間がかかる
- **戦術的局面（depth=3）**: 約8.5ms
  - シンプルな局面のため非常に高速

#### 時間固定探索（10ms制限）
- すべての局面で約12ms前後で安定動作

### 今後の改善点

1. **探索深さの最適化**
   - 終盤局面での探索深さをさらに調整する必要がある
   - 局面の複雑さに応じた動的な深さ設定

2. **並列化**
   - 現在はシングルスレッドでの実行
   - 並列探索の実装により大幅な高速化が期待できる

3. **EnhancedSearcherの統合**
   - UnifiedSearcherへの完全移行後のパフォーマンス測定

## 注意事項

- ベンチマーク実行時は他の重いプロセスを停止することを推奨
- 結果は実行環境（CPU、メモリ等）に依存します
- criterionのHTMLレポートは `target/criterion/` に生成されます

---

## TT 衝突クラスター・ベンチ（tt_collision_cluster_bench）

置換表の同一バケットに多数のキーが集中する最悪系パターンで、store/probe の挙動とスループットを測定します。固定サイズバケット（4）および可変バケット（4/8/16）を対象にしています。

### 実行方法（推奨: ベンチバイナリを限定）

```
# このベンチのみ実行（他のベンチの設定に影響されない）
cargo bench -p engine-core --bench tt_collision_cluster_bench

# 関数フィルタ（例: store のみ）
cargo bench -p engine-core --bench tt_collision_cluster_bench -- store_clustered

# グループ指定（例: 可変バケット群）
cargo bench -p engine-core --bench tt_collision_cluster_bench -- tt_collision_flexible
```

Criterion のフィルタ（末尾の引数）はベンチバイナリごとに適用されます。Cargo はデフォルトで全ベンチバイナリを起動するため、他のベンチによる失敗を避けるには `--bench tt_collision_cluster_bench` の指定を推奨します。

### 実行環境（計測条件）の設定

このベンチは以下の環境変数で計測条件を調整できます（未設定時は保守的なデフォルト）。

- BENCH_SAMPLE_SIZE: サンプル数（例: 30）
- BENCH_WARMUP_MS: ウォームアップ時間(ms)
- BENCH_MEASUREMENT_MS: 計測時間(ms)
- BENCH_TABLE_MB: 置換表サイズ(MB)
- BENCH_COLLISION_KEYS: 固定バケット用の衝突キー総数
- BENCH_PREFILL: 固定バケットの事前投入件数
- BENCH_KEYS_PER_ENTRY: 可変バケットでの「バケット容量×倍率」のキー数
- BENCH_PREFILL_MULT: 可変バケットの事前投入倍率

例:

```
BENCH_SAMPLE_SIZE=30 \
BENCH_WARMUP_MS=200 \
BENCH_MEASUREMENT_MS=800 \
BENCH_TABLE_MB=8 \
 cargo bench -p engine-core --bench tt_collision_cluster_bench -- probe_clustered
```

### 出力と指標

- 各ベンチは `Throughput::Elements(1)` を設定しており、1 イテレーション当たりの処理件数をスループットの基準として表示します。
- レポート・プロットは `target/criterion/` 以下に保存されます。

### トラブルシューティング

- 実行時に他のベンチで失敗が出る場合は、必ず `--bench tt_collision_cluster_bench` を付けてこのベンチのみを対象にしてください。
- Linux 環境で gnuplot が無い場合は plotters バックエンドに自動フォールバックします（表示品質が異なる場合があります）。
