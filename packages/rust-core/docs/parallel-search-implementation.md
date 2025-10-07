# 並列探索実装詳細

本ドキュメントでは、並列探索の実装詳細について説明します。

## Root Move Ordering（手順生成と優先順位付け）

Root move ordering は探索効率を左右する重要な要素です。本実装では3段階のフォールバック機構を採用しています。

### 優先順位ルール

```
1. TT hint（置換表ヒント）       ← 最優先
2. prev_root_lines（前反復PV）    ← 第2優先
3. best_hint_next_iter（反復間ヒント） ← フォールバック
```

#### 実装詳細（`crates/engine-core/src/search/ab/driver.rs`）

```rust
// 1. TT hint取得
let mut root_tt_hint_mv: Option<Move> = None;
if let Some(tt) = &self.tt {
    if let Some(entry) = tt.probe(root_key, root.side_to_move) {
        if let Some(ttm) = entry.get_move() {
            root_tt_hint_mv = Some(ttm);
        }
    }
}

// 2. フォールバックヒント構築
let hint_for_picker = root_tt_hint_mv.or_else(|| {
    if prev_root_lines.is_none() {
        best_hint_next_iter.map(|(m, _)| m)
    } else {
        None
    }
});

// 3. RootPickerで優先順位適用
let mut root_picker = ordering::RootPicker::new(
    root,
    list.as_slice(),
    hint_for_picker,      // TTヒント or 反復間ヒント
    prev_root_lines,       // 前反復PV（あれば最優先）
    root_jitter,
);
```

### 反復間ヒント（A: Root並びヒント）

前反復のベスト手を次反復の並び順ヒントとして活用します。

**メリット**:
- 初回反復や前反復が未完了の場合でも手順の連続性を保つ
- TT衝突時のフォールバック手段として機能
- 決定的動作（同一session_id/root_key/worker_idで再現可能）

**実装箇所**:
```rust
// 反復ループ前
let mut best_hint_next_iter: Option<(Move, i32)> = None;

// pv_idx==1ブロックで更新
if pv_idx == 1 {
    best = Some(m);
    best_score = local_best;
    prev_score = local_best;
    local_best_for_next_iter = Some((m, local_best));
    // ...
}

// 反復末尾で持ち回り
best_hint_next_iter = local_best_for_next_iter;
```

## Aspiration Window管理

Aspiration windowは探索窓を狭めることでnode数を削減する最適化技術です。

### 基本パラメータ（`crates/engine-core/src/search/constants.rs`）

```rust
pub const ASPIRATION_DELTA_INITIAL: i32 = 30;  // 初期窓幅（±30 centipawns）
pub const ASPIRATION_DELTA_MAX: i32 = 350;     // 最大窓幅上限
```

### 平滑化機構（C: Aspiration中心値の平滑化）

`prev_root_lines`が不在（前反復が未完了）の場合、前反復スコアと反復間ヒントスコアの加重平均を使用します。

```rust
let aspiration_center = if d > 1 && prev_root_lines.is_none() {
    if let Some((_, hint_score)) = best_hint_next_iter {
        // 加重平均: 70% prev_score, 30% hint_score
        (7 * prev_score + 3 * hint_score) / 10
    } else {
        prev_score
    }
} else {
    prev_score
};

let mut alpha = if d == 1 {
    i32::MIN / 2
} else {
    aspiration_center - ASPIRATION_DELTA_INITIAL
};

let mut beta = if d == 1 {
    i32::MAX / 2
} else {
    aspiration_center + ASPIRATION_DELTA_INITIAL
};
```

**効果**:
- スコア変動の激しい局面での窓の安定化
- 再探索頻度の最適化
- fail-high/low時の窓拡大戦略への影響緩和

### 再探索戦略

```rust
loop {
    score = self.ab(/* alpha, beta */);

    if score <= alpha {
        // Fail-low: 窓を下方拡大
        alpha = (alpha - delta).max(i32::MIN / 2);
        delta = (delta * 2).min(ASPIRATION_DELTA_MAX);

    } else if score >= beta {
        // Fail-high: 窓を上方拡大
        beta = (beta + delta).min(i32::MAX / 2);
        delta = (delta * 2).min(ASPIRATION_DELTA_MAX);

    } else {
        // Exact: 探索成功
        break;
    }
}
```

## Helper Snapshot発行とPV整合性

Helper workerは探索途中で暫定結果（snapshot）を発行します。

### PV選択ロジック（`crates/engine-core/src/search/parallel/mod.rs:publish_helper_snapshot`）

```rust
let mut pv: SmallVec<[Move; 32]> = SmallVec::new();
let mut chosen_bound = result.node_type;
let mut chosen_score = result.score;

if let Some(first_line) = result.lines.as_ref().and_then(|ls| ls.first()) {
    let use_lines0 = first_line.bound == NodeType::Exact || result.stats.pv.is_empty();

    if use_lines0 {
        // Exact境界のPVを優先（品質高）
        pv.extend(first_line.pv.iter().copied());
        chosen_bound = first_line.bound;
        chosen_score = first_line.score_cp;
    } else {
        // fail-high/lowでstats.pvあり → stats.pvにフォールバック
        pv.extend(result.stats.pv.iter().copied());
        // chosen_bound/chosen_scoreはresult.*のまま
    }
} else {
    pv.extend(result.stats.pv.iter().copied());
}
```

**重要**: PV、bound、scoreは**三点セットで整合性を保つ**必要があります。

| 選択元 | PV | bound | score |
|-------|----|----|-------|
| `lines[0]` (Exact) | `first_line.pv` | `first_line.bound` | `first_line.score_cp` |
| `stats.pv` (フォールバック) | `result.stats.pv` | `result.node_type` | `result.score` |

### Snapshot発行条件

```rust
// 最小深さフィルタ（浅い結果を抑制）
pub const HELPER_SNAPSHOT_MIN_DEPTH: u32 = 3;

if result.depth < HELPER_SNAPSHOT_MIN_DEPTH || pv.is_empty() {
    return; // 発行しない
}
```

## Jitter Seed（探索多様化）

各ワーカーに固有の乱数シードを割り当て、探索経路を分散させます。

### シード計算式

```rust
pub fn compute_jitter_seed(session_id: u64, worker_id: usize, root_key: u64) -> u64 {
    let combined = session_id ^ root_key ^ (worker_id as u64);
    split_mix_64(combined)
}
```

**特性**:
- **決定性**: 同一(session_id, worker_id, root_key)で常に同じシード
- **安定性**: スレッドプールresize時もworker_id不変なら同一シード
- **多様性**: ワーカー・局面ごとに異なるシードで探索分散

### 適用箇所

```rust
let root_jitter = limits.root_jitter_seed.map(|seed| {
    ordering::RootJitter::new(seed, ordering::constants::ROOT_JITTER_AMPLITUDE)
});

let mut root_picker = ordering::RootPicker::new(
    root,
    list.as_slice(),
    hint_for_picker,
    prev_root_lines,
    root_jitter,  // ← ここで適用
);
```

**効果測定**: `duplication_meter`（未実装）でTT依存の重複率を観測予定

## Heuristics管理

### セッション境界でのクリア（Policy A）

```rust
// WorkerLocal::prepare_for_job()
if self.last_session_id != Some(session_id) {
    self.heuristics.clear_all();
    self.last_session_id = Some(session_id);
}
```

**理由**: 前ゲームのkillerやhistoryが新ゲーム探索を汚染するのを防止

### ジョブ間再利用

同一セッション内では`Heuristics`をジョブ間で再利用し、徐々にmove orderingを改善します。

```rust
// helper経路（think_with_ctx）
let heur = worker_local.prepare_for_job(session_id, root_key);
let result = backend.think_with_ctx(root, limits, ctx, info)?;
worker_local.heuristics = heur; // 更新を保存
```

### Main vs Helper の扱い

| スレッド | スタック管理 | Heuristics管理 |
|---------|------------|--------------|
| Main | TLS（`STACK_CACHE`）、毎回新規 | 反復内で持ち回り |
| Helpers | `WorkerLocal::stack`、リセット再利用 | セッション内再利用 |

## 環境変数とチューニング

### SHOGI_WORKER_STACK_MB
```bash
export SHOGI_WORKER_STACK_MB=16
```
- ワーカースタックサイズをMB単位で指定
- デフォルト: OS依存（通常2-8MB）
- 用途: 深い再帰探索でのスタックオーバーフロー回避

### SHOGI_THREADPOOL_METRICS
```bash
export SHOGI_THREADPOOL_METRICS=1
```
- スレッドプールメトリクス収集を有効化
- 出力: shutdown時にidle/HI/Normalキュー処理回数を標準出力
- 用途: 負荷分散の診断

### SHOGI_CURRMOVE_THROTTLE_MS
```bash
export SHOGI_CURRMOVE_THROTTLE_MS=150   # スロットルを緩める
export SHOGI_CURRMOVE_THROTTLE_MS=off   # スロットル無効化
```
- CurrMoveイベント発火間隔（ms）
- デフォルト: 100ms（並列環境の応答性考慮）
- 用途: USI出力頻度調整（GUI応答性 vs ログ量）

### SHOGI_TEST_FORCE_JITTER
```bash
export SHOGI_TEST_FORCE_JITTER=0  # ジッター無効化（決定的探索）
```
- テスト環境でジッター機能強制ON/OFF
- デフォルト: 1（有効）
- 用途: テストの再現性確保

## 時間管理と停止制御

### 時間チェック頻度

```rust
pub const TIME_CHECK_MASK_NORMAL: u64 = 0x1FFF;  // 8192 nodes
pub const TIME_CHECK_MASK_BYOYOMI: u64 = 0x7FF;  // 2048 nodes（秒読みで高頻度）

// 探索中
if nodes & mask == 0 {
    check_time_limit();
}
```

### 締切管理

```rust
pub const NEAR_DEADLINE_WINDOW_MS: u64 = 50;         // 締切接近判定
pub const LIGHT_POLL_INTERVAL_MS: u64 = 8;           // 軽量ポーリング間隔
pub const MAIN_NEAR_DEADLINE_WINDOW_MS: u64 = 500;   // 新反復開始ガード窓
pub const NEAR_HARD_FINALIZE_MS: u64 = 500;          // Hard締切前finalize窓
```

**戦略**:
1. 通常時: 軽量ポーリング（8ms間隔）
2. 締切50ms前: 高頻度チェック
3. 締切500ms前: 新反復開始禁止
4. Hard締切500ms前: 積極的finalize

### Qsearch制限

```rust
pub const DEFAULT_QNODES_LIMIT: u64 = 300_000;
pub const MIN_QNODES_LIMIT: u64 = 10_000;
pub const QNODES_PER_MS: u64 = 10;
pub const QNODES_DEPTH_BONUS_PCT: u64 = 5;

// 計算式
let qnodes_limit = if let Some(time_ms) = remaining_time_ms {
    let base = (time_ms * QNODES_PER_MS).max(MIN_QNODES_LIMIT);
    let depth_bonus = (depth as u64 * QNODES_DEPTH_BONUS_PCT * base) / 100;
    base + depth_bonus
} else {
    DEFAULT_QNODES_LIMIT
};
```

## パフォーマンス最適化

### TT Prefetching（適応的距離調整）

```rust
// crates/engine-core/src/search/adaptive_prefetcher.rs
pub struct AdaptivePrefetcher {
    stats: PrefetchStats,
    config: PrefetchConfig,
}

// 使用例
if dynp::tt_prefetch_enabled() {
    tt.prefetch_l2(next_key, side_to_move);
}
```

**効果**: メモリレイテンシ隠蔽、hit率に応じた距離調整

### Context Fast-Path（helpers専用）

```rust
// WorkerLocal経由の高速経路
pub fn think_with_ctx(
    &mut self,
    root: &Position,
    limits: SearchLimits,
    ctx: &mut WorkerContext,
    info: Option<&InfoCallback>,
) -> Result<SearchResult, SearchError>
```

**メリット**:
- TLS経由のコピー削減
- スタック・ヒューリスティクスの直接受け渡し
- アロケーション回避

### qnodes集計（共有カウンタ対応）

```rust
// 全ワーカーが同じArc<AtomicU64>をインクリメント
// 集計時は最大値を取得（合計ではない）
let total_qnodes: u64 = results.iter().map(|(_, r)| r.stats.qnodes).max().unwrap_or(0);
```

**理由**: 共有カウンタのため、合計すると二重カウントになる

## コード参照

| 機能 | ファイルパス | 行番号（目安） |
|-----|------------|--------------|
| Root move ordering | `crates/engine-core/src/search/ab/driver.rs` | 465-490 |
| Aspiration平滑化 | `crates/engine-core/src/search/ab/driver.rs` | 556-578 |
| Helper snapshot発行 | `crates/engine-core/src/search/parallel/mod.rs` | 397-445 |
| Jitter seed計算 | `crates/engine-core/src/search/parallel/mod.rs` | 107-123 |
| WorkerLocal管理 | `crates/engine-core/src/search/parallel/thread_pool.rs` | 47-136 |
| StopController | `crates/engine-core/src/search/parallel/stop_ctrl.rs` | 全体 |
