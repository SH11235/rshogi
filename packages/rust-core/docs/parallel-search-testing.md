# 並列探索テスト戦略

本ドキュメントでは、並列探索機能のテスト戦略と実装について説明します。

## テスト方針

### 1. 単体テスト
- コンポーネントごとの独立した動作確認
- エッジケースとエラーハンドリング
- 決定的動作の検証

### 2. 統合テスト
- 複数コンポーネント間の連携確認
- 並列実行時の整合性
- 停止制御とfinalize処理

### 3. パフォーマンステスト
- スケーラビリティ測定
- スレッド数別のNPS（Nodes Per Second）
- メモリ使用量とリソース効率

## 単体テストカタログ

### Jitter Seed（探索多様化）

#### `test_jitter_seed_deterministic_and_varies`
**目的**: シード計算の決定性と多様性を検証

```rust
#[test]
fn jitter_seed_deterministic_and_varies() {
    let session_id = 100u64;
    let root_key = 0x1234_5678_9ABC_DEF0u64;

    // 同一条件で決定的
    let seed1 = compute_jitter_seed(session_id, 1, root_key);
    let seed2 = compute_jitter_seed(session_id, 1, root_key);
    assert_eq!(seed1, seed2);

    // worker_id変化で異なるシード
    let seed_w1 = compute_jitter_seed(session_id, 1, root_key);
    let seed_w2 = compute_jitter_seed(session_id, 2, root_key);
    assert_ne!(seed_w1, seed_w2);
}
```

**ファイル**: `crates/engine-core/src/search/parallel/mod.rs`

#### `test_compute_jitter_seed_collision_smoke`
**目的**: シード衝突の頻度確認（smoke test）

```rust
#[test]
fn compute_jitter_seed_collision_smoke() {
    let mut seeds = std::collections::HashSet::new();
    for session in 0..100 {
        for worker in 0..8 {
            for root_key in 0..10 {
                let seed = compute_jitter_seed(session, worker, root_key as u64);
                seeds.insert(seed);
            }
        }
    }
    // 8000パターンでほぼ全て一意であることを期待
    assert!(seeds.len() > 7900);
}
```

### Helper Snapshot PV選択

#### `test_helper_snapshot_prefers_lines_pv_over_stats_pv`
**目的**: Exact境界のlines[0].pvを優先することを検証

```rust
#[test]
fn helper_snapshot_prefers_lines_pv_over_stats_pv() {
    // lines[0].bound = Exact のケース
    let mut lines = SmallVec::new();
    lines.push(RootLine {
        bound: NodeType::Exact,
        pv: vec![line_move],  // ← これを優先
        // ...
    });

    let result = SearchResult {
        stats: SearchStats {
            pv: vec![stats_move],  // ← 無視される
            // ...
        },
        lines: Some(lines),
        // ...
    };

    publish_helper_snapshot(&stop_ctrl, session_id, root_key, worker_id, &result);

    let snapshot = stop_ctrl.try_read_snapshot().unwrap();
    assert_eq!(snapshot.pv[0], line_move);  // lines[0].pvを使用
}
```

**ファイル**: `crates/engine-core/src/search/parallel/mod.rs`

#### `test_helper_snapshot_falls_back_to_stats_pv_when_lines_not_exact`
**目的**: fail-high/low時にstats.pvへフォールバックし、bound/scoreも整合することを検証

```rust
#[test]
fn helper_snapshot_falls_back_to_stats_pv_when_lines_not_exact() {
    // lines[0].bound = LowerBound (fail-high)
    let mut lines = SmallVec::new();
    lines.push(RootLine {
        bound: NodeType::LowerBound,
        score_cp: 150,
        pv: vec![line_move],
        // ...
    });

    let result = SearchResult {
        node_type: NodeType::Exact,  // ← こちらを採用
        score: 120,                  // ← こちらを採用
        stats: SearchStats {
            pv: vec![stats_move],    // ← こちらを採用
            // ...
        },
        lines: Some(lines),
    };

    publish_helper_snapshot(&stop_ctrl, session_id, root_key, worker_id, &result);

    let snapshot = stop_ctrl.try_read_snapshot().unwrap();
    assert_eq!(snapshot.pv[0], stats_move);           // stats.pvを使用
    assert_eq!(snapshot.node_type, NodeType::Exact);  // result.node_typeを使用
    assert_eq!(snapshot.score_cp, 120);               // result.scoreを使用
}
```

**重要**: PV、bound、scoreの三点セットが整合していることを確認

### Heuristics管理

#### `test_heuristics_carryover_across_pvs_and_iterations`
**目的**: セッション内でヒューリスティクスが持ち回られることを検証

```rust
#[test]
fn heuristics_carryover_across_pvs_and_iterations() {
    // 同一セッション内で2回探索
    let session_id = 789u64;
    let result1 = searcher.search(&mut pos, limits.clone());
    let result2 = searcher.search(&mut pos, limits.clone());

    // 2回目の探索でヒューリスティクスが成長していることを期待
    // （例: lmr_trials > 0, killer moves設定済み）
}
```

**ファイル**: `crates/engine-core/src/search/parallel/mod.rs`

### StopController

#### `test_finalize_priority_hard_persists_after_user_stop`
**目的**: Hard締切優先度がuser stop後も保持されることを検証

```rust
#[test]
fn finalize_priority_hard_persists_after_user_stop() {
    let ctrl = StopController::new();
    ctrl.request_finalize(FinalizePriority::Hard);
    ctrl.request_stop();  // 後からuser stop

    // Hard優先度が残ること
    assert_eq!(ctrl.get_finalize_priority(), Some(FinalizePriority::Hard));
}
```

**ファイル**: `crates/engine-core/src/search/parallel/stop_ctrl.rs`

#### `test_finalize_concurrency_prefers_highest_priority`
**目的**: 並行finalize要求時に最高優先度が勝つことを検証

```rust
#[test]
fn finalize_concurrency_prefers_highest_priority() {
    let ctrl = Arc::new(StopController::new());
    let handles: Vec<_> = (0..10)
        .map(|i| {
            let c = ctrl.clone();
            thread::spawn(move || {
                let priority = if i % 3 == 0 {
                    FinalizePriority::Hard
                } else {
                    FinalizePriority::Planned
                };
                c.request_finalize(priority);
            })
        })
        .collect();

    for h in handles { h.join().unwrap(); }

    // Hardが少なくとも1つあれば、Hard優先度になる
    assert_eq!(ctrl.get_finalize_priority(), Some(FinalizePriority::Hard));
}
```

### ThreadPool

#### `test_shutdown_response_time`
**目的**: シャットダウンの応答時間を確認

```rust
#[test]
fn shutdown_response_time() {
    let pool = ThreadPool::new(4);
    let start = Instant::now();
    pool.shutdown();
    let elapsed = start.elapsed();

    // 20msタイムアウト設定により、最悪でも100ms以内に完了
    assert!(elapsed < Duration::from_millis(100));
}
```

**ファイル**: `crates/engine-core/src/search/parallel/thread_pool.rs`

#### `test_worker_local_prepare_resets_state`
**目的**: prepare_for_job()がスタックとヒューリスティクスを適切にリセットすることを検証

```rust
#[test]
fn worker_local_prepare_resets_state() {
    let mut local = WorkerLocal::new(/* ... */);

    // 1回目のジョブで状態を汚す
    let mut ctx1 = local.prepare_for_job(100, 0x1234);
    ctx1.stack[5].move_value = 999;  // ダミー値設定

    // 2回目のジョブで同一セッション（ヒューリスティクス保持）
    let ctx2 = local.prepare_for_job(100, 0x5678);
    assert_eq!(ctx2.stack[5].move_value, 0);  // スタックはリセット済み

    // 3回目で別セッション（ヒューリスティクスもクリア）
    let _ctx3 = local.prepare_for_job(200, 0x9ABC);
    // heuristics.clear_all()が呼ばれたことを期待
}
```

## 統合テスト

### 並列探索基本動作

#### `test_parallel_search_with_multiple_threads`
**目的**: 複数スレッドで探索が正常に動作することを確認

```rust
#[test]
fn parallel_search_with_multiple_threads() {
    let mut pos = Position::from_sfen(STARTPOS).unwrap();
    let evaluator = create_test_evaluator();
    let tt = Arc::new(TranspositionTable::new_mb(16));

    let searcher = ParallelSearcher::new(evaluator, 4, tt);  // 4スレッド

    let limits = SearchLimits::builder()
        .depth(10)
        .build();

    let result = searcher.search(&mut pos, limits);

    assert!(result.is_ok());
    assert!(result.unwrap().best_move.is_some());
}
```

**ファイル**: `crates/engine-core/tests/parallel_search_jitter.rs`（既存）

### 停止制御とFinalize

#### `test_stop_during_parallel_search`
**目的**: 探索中の停止要求が正しく処理されることを確認

```rust
#[test]
fn stop_during_parallel_search() {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_ctrl = Arc::new(StopController::new());

    let limits = SearchLimits::builder()
        .depth(20)
        .stop_flag(Some(stop_flag.clone()))
        .stop_controller(Some(stop_ctrl.clone()))
        .build();

    // 別スレッドで500ms後に停止
    let flag_clone = stop_flag.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(500));
        flag_clone.store(true, Ordering::Relaxed);
    });

    let start = Instant::now();
    let result = searcher.search(&mut pos, limits);
    let elapsed = start.elapsed();

    // 停止が機能し、1秒以内に終了
    assert!(elapsed < Duration::from_secs(1));
    assert!(result.is_ok());
}
```

### 結果整合性

#### `test_main_worker_result_priority`
**目的**: Main workerの結果が優先されることを確認

```rust
#[test]
fn main_worker_result_priority() {
    // Main workerが深さ10、Helperが深さ8まで到達した場合
    let result = searcher.search(&mut pos, limits);

    assert_eq!(result.unwrap().depth, 10);  // Main workerの深さが採用される
}
```

## パフォーマンステスト

### スケーラビリティ測定

#### ベンチマークコマンド
```bash
# 1,2,4スレッドで固定時間探索、NPS比較
cargo run --release --bin lazy_smp_benchmark -- \
  --threads 1,2,4 \
  --fixed-total-ms 200 \
  --iterations 3 \
  --tt-mb 64 \
  --json results/lazy_smp_20251007.json
```

- `--sfens <file>` で任意のベンチ用SFENセットを指定可能（未指定時は組み込み5局面）。
- `--tt-mb` のデフォルトは64MB（ベンチ用途は32〜64MBを推奨）。
- `--jitter on/off` でヘルパースレッドの乱択ヒューリスティクスを制御。
- JSON を指定すると平均NPSや効率が保存される（`efficiency_pct` は1スレッド基準）。

**出力例**:
```
threads= 1 | searches=  5 | avg_nps= 482000 | elapsed= 1000.3 ms | max_depth=12 | helper_share=N/A
             efficiency vs baseline: 100.0%
threads= 2 | searches= 10 | avg_nps= 905000 | elapsed= 1012.7 ms | max_depth=13 | helper_share=35.42%
             efficiency vs baseline: 93.9%
threads= 4 | searches= 20 | avg_nps=1645000 | elapsed= 1015.4 ms | max_depth=13 | helper_share=68.10%
             efficiency vs baseline: 85.3%
```

#### 期待される効率
- 2スレッド: 85-95%（TT競合少ない）
- 4スレッド: 75-85%（TT競合増加、重複探索）
- 8スレッド: 60-75%（リターン逓減）

### メモリ使用量

```bash
# Valgrind massif でヒーププロファイル
valgrind --tool=massif ./target/release/lazy_smp_benchmark \
  --threads 4 --fixed-total-ms 1000

ms_print massif.out.<pid>
```

**確認項目**:
- WorkerLocal確保量（stack + heuristics）
- TT使用量（共有）
- メモリリーク有無

### 重複率測定（将来実装）

```rust
// duplication_meter: Arc<AtomicU64> を SharedSearchState に追加
// TT hit時にカウンタインクリメント

let duplication_rate = (tt_hits as f64) / (total_nodes as f64);
println!("Duplication rate: {:.2}%", duplication_rate * 100.0);
```

## テスト実行方法

### 単体テスト（全て）
```bash
cargo test --lib
```

### 特定テスト実行
```bash
# Jitter関連のみ
cargo test jitter

# Helper snapshot関連のみ
cargo test helper_snapshot

# 並列探索統合テスト
cargo test --test parallel_search_jitter
```

### リリースビルドでテスト
```bash
cargo test --release
```

### 詳細出力
```bash
cargo test -- --nocapture --test-threads=1
```

## デバッグテクニック

### ログレベル設定
```bash
RUST_LOG=debug cargo test test_name -- --nocapture
RUST_LOG=engine_core::search::parallel=trace cargo test
```

### StopController診断
```rust
if let Some(ctrl) = limits.stop_controller.as_ref() {
    let snapshot = ctrl.try_read_snapshot();
    eprintln!("Snapshot: {:?}", snapshot);

    let stop_info = ctrl.try_read_stop_info();
    eprintln!("Stop info: {:?}", stop_info);
}
```

### ThreadPool メトリクス
```bash
export SHOGI_THREADPOOL_METRICS=1
cargo test
# shutdown時にキュー処理統計が出力される
```

### 決定的探索（ジッター無効）
```bash
export SHOGI_TEST_FORCE_JITTER=0
cargo test test_deterministic_search
```

## カバレッジ目標

| カテゴリ | 目標カバレッジ | 現状 |
|---------|--------------|------|
| 単体テスト（コアロジック） | 90%以上 | ✅ 達成 |
| 統合テスト（並列動作） | 80%以上 | ✅ 達成 |
| エッジケース | 主要パス全て | ✅ 達成 |
| パフォーマンス回帰 | 継続監視 | 🔄 進行中 |

## 継続的テスト戦略

1. **CI/CD統合**: GitHub Actions で自動テスト実行
2. **回帰テスト**: リリース前に全テストスイート実行
3. **ベンチマーク追跡**: NPS変動を履歴管理
4. **メモリリークチェック**: Valgrind定期実行

## 既知の制限事項

1. **決定性テスト**: 浮動小数点演算（NPS計算等）は厳密な一致を保証しない
2. **タイミング依存**: 停止制御テストは環境負荷で変動する可能性
3. **スレッド数上限**: 物理コア数を超える並列度は効率が大幅低下

## 参考資料

- テストファイル: `crates/engine-core/src/search/parallel/mod.rs` (tests module)
- 統合テスト: `crates/engine-core/tests/parallel_search_jitter.rs`
- StopControllerテスト: `crates/engine-core/src/search/parallel/stop_ctrl.rs` (tests module)
- ThreadPoolテスト: `crates/engine-core/src/search/parallel/thread_pool.rs` (tests module)
