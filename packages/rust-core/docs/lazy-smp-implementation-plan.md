# Lazy SMP実装計画（並列探索）

> **総評**: "とても良いたたき台。ただし Lazy SMP 特有の落とし穴をもう一段潰すと安全に伸びます"

## 1. 現状分析

### 現在のアーキテクチャ
- **シングルスレッド探索**: UnifiedSearcherが単一スレッドで動作
- **TTの実装**: 既にロックフリー（AtomicU64使用）で並列アクセス対応済み
- **SearchContext**: AtomicBoolを使った停止フラグ管理済み
- **エンジン管理**: Engine構造体がMutexで各Searcherを保護

### 並列化に有利な点
1. **TranspositionTable (TT)**: 
   - AtomicU64ベースのロックフリー実装
   - compare_exchange_weakを使用したCAS操作
   - 4エントリ/バケット構造で競合を軽減

2. **停止制御**:
   - AtomicBoolによる停止フラグ
   - Orderingの適切な使用（Acquire/Release）

3. **評価関数**:
   - Arc<E>で共有可能な設計
   - NNUEはMutex保護だが、読み取り専用で使用可能

### 並列化の課題と解決策
1. **グローバル状態の共有**:
   - **History（履歴ヒューリスティック）**: 
     - 課題: ローカルコピーだけではスレッド間の学習が断絶
     - 解決: lock-free add (fetch_add) と周期的減衰で"ゆるく"共有
   - **KillerTable（キラームーブ）**: 
     - 課題: 頻繁な更新で競合が発生
     - 解決: スレッドローカルとし、反復深化毎にshallow copyで十分
   - **SearchStats（統計情報）**: 
     - 解決: AtomicU64でノードカウントを集約

2. **PVTable（主要変動）**:
   - 課題: Mutexで包むとPV更新毎に全スレッドが待機
   - 解決: 
     - スレッドローカルPV + 世代番号方式
     - best_move/best_scoreのみAtomic*で即時公開

3. **時間管理**:
   - 課題: 複数スレッドからの同時アクセス
   - 解決: TimeManagerは単独スレッドで周期チェック

4. **重複探索の抑制**:
   - 課題: 序盤はTTミスが多く重複率が高い
   - 解決: 
     - 深さシフト表をroot move indexで決定（Stockfish方式）
     - Phase 3でABDADAの簡易版を検討

## 2. Lazy SMP設計方針

### 基本方針
- **シンプルな実装**: 各スレッドが異なる深さから探索開始
- **TT共有**: 全スレッドが同一のTTを共有
- **最小限の同期**: 必要最小限の同期機構のみ使用

### スレッド構成
```rust
pub struct ParallelSearcher<E> {
    // 共有リソース
    tt: Arc<TranspositionTable>,
    evaluator: Arc<E>,
    time_manager: Arc<TimeManager>,
    shared_history: Arc<SharedHistory>,  // lock-free共有
    
    // スレッドローカル
    threads: Vec<SearchThread<E>>,
    
    // 同期用（lock-free）
    stop_flag: Arc<AtomicBool>,
    best_move: Arc<AtomicU32>,     // Move as u32, lock-free
    best_score: Arc<AtomicI32>,    // lock-free
    best_depth: Arc<AtomicU8>,     // 深さフィルタリング用
    nodes_searched: Arc<AtomicU64>, // 全スレッドのノード数
}

/// Lock-free共有History
struct SharedHistory {
    table: Vec<AtomicU32>,  // fetch_add で更新
}
```

## 3. 実装ステップ

### Phase 1: 基盤整備（1週間）
1. **SearchThread構造体の作成**
   - UnifiedSearcherのラッパー
   - スレッドID管理
   - ローカルな履歴/キラーテーブル

2. **共有状態の分離**
   - グローバル状態とローカル状態の明確化
   - Arc/Mutexによる共有機構の実装

3. **PVTableの並列化対応**
   - スレッドローカルPV + 世代番号管理
   - best_move/best_scoreのlock-free更新
   - 深さベースのフィルタリング

4. **Thread Sanitizer/Miri対応**
   - データ競合の早期検出環境構築
   - cargo +nightly miri testでUBチェック

### Phase 2: 基本的な並列探索（1週間）
1. **ParallelSearcherの実装**
   - スレッドプールの管理
   - 探索深度の割り当て（スレッドID + オフセット）
   - 結果の集約

2. **探索ループの並列化**
   - 各スレッドで異なる深さから開始
   - 停止条件の共有
   - ベストムーブの更新

3. **時間管理の調整**
   - 全スレッドがAtomicU64にノード数を加算
   - TimeManagerは単独スレッドで周期チェック
   - 残りノード早期終了式の共通化

4. **重複率の計測**
   - unique_nodes / total_nodesをログ出力
   - 重複探索の改善指標として活用

### Phase 3: 最適化（1週間）
1. **ヘルパースレッド戦略**
   - メインスレッドは通常の反復深化
   - ヘルパーは深さをスキップ

2. **同期オーバーヘッドの削減**
   - ローカルキャッシュの活用
   - バッチ更新の実装

3. **動的スレッド数調整**
   - 探索深度に応じたスレッド数
   - CPUコア数の自動検出

4. **高度な重複抑制**
   - Root move indexベースの深さシフト（Stockfish方式）
   - ABDADA簡易版の実装検討

5. **Core PinningとNUMA対応**
   - libc::sched_setaffinity or core_affinity crateで実装
   - NUMA環境での局所性向上

6. **UCI "Threads"オプション実装**
   - 実対局ベンチマークの効率化

## 4. 実装詳細

### 4.1 SearchThread構造
```rust
struct SearchThread<E> {
    id: usize,
    searcher: UnifiedSearcher<E, true, true, 0>, // TT_SIZE_MB=0 (共有TT使用)
    local_history: History,
    local_killers: KillerTable,
    thread_local_pv: Vec<Move>,  // スレッドローカルPV
    generation: u64,             // PV世代番号
}
```

### 4.2 探索開始深度の計算
```rust
// 基本的な深さシフト方式
fn get_start_depth(thread_id: usize, iteration: usize) -> u8 {
    if thread_id == 0 {
        // メインスレッドは通常の反復深化
        iteration as u8
    } else {
        // ヘルパースレッドは深さをスキップ
        let skip = thread_id % 2 + 1;
        (iteration + skip) as u8
    }
}

// Root move indexベースの割り当て（Stockfish方式）
fn assign_root_moves(thread_id: usize, total_moves: usize) -> Vec<usize> {
    // スレッド0: 0,4,8,12... スレッド1: 1,5,9,13... など
    (thread_id..total_moves).step_by(num_threads()).collect()
}
```

### 4.3 Lock-freeベスト更新
```rust
// PVTable競合削減の実装例
thread_local! {
    static LOCAL_PV: RefCell<Vec<Move>> = RefCell::new(Vec::new());
}

// ベスト更新 (lock-free)
fn maybe_update_best(
    score: i32, 
    mv: Move, 
    depth: u8,
    best_score: &AtomicI32,
    best_move: &AtomicU32,
    best_depth: &AtomicU8
) {
    // 深さで先にフィルタ
    let old_depth = best_depth.load(Ordering::Relaxed);
    if depth < old_depth { return; }

    // スコア更新を試みる
    if best_score.fetch_max(score, Ordering::Relaxed) < score {
        best_move.store(mv.to_u16() as u32, Ordering::Relaxed);
        best_depth.store(depth, Ordering::Release);
    }
}
```

### 4.4 ノードカウント集約
```rust
// 各スレッドからの更新
fn increment_nodes(global_nodes: &AtomicU64, local_count: u64) {
    global_nodes.fetch_add(local_count, Ordering::Relaxed);
}

// TimeManagerでの周期的チェック
fn check_time_limit(nodes: &AtomicU64, stop_flag: &AtomicBool) {
    let current_nodes = nodes.load(Ordering::Relaxed);
    if should_stop(current_nodes) {
        stop_flag.store(true, Ordering::Release);
    }
}
```

## 5. テスト計画

### 単体テスト
- スレッド間の同期テスト
- TT競合のストレステスト
- 停止処理の正確性（race条件のfuzzingテスト）
- Lock-free更新の正確性検証

### 統合テスト
- シングルスレッドとの結果比較
- スケーラビリティテスト（1-16スレッド）
- 時間制限下での動作確認

### パフォーマンステスト
- NPS（Nodes Per Second）の測定
- スレッド数とスピードアップの関係
- メモリ使用量の確認
- **重複率（dup%）の測定**: unique_nodes / total_nodes
- CI全スレッド構成（1,2,4,8）のregression bench追加

### 停止処理テスト
```rust
// Fuzzingによる停止遅延テスト
fn test_stop_responsiveness() {
    // 1万局面で停止フラグ伝搬の遅延を計測
    // 遅延 ≤ 5ms（1秒制限テスト）を確認
}
```

## 6. 期待される効果

### パフォーマンス向上（現実的な見積もり）
- 4コア: 2.2-3.0倍のスピードアップ（実測値ベース）
- 8コア: 4.0-5.5倍のスピードアップ
- 16コア: 9-10倍のスピードアップ（Stockfish実績ベース）

※ 同期コストゼロの理論値ではなく、実測で0.55-0.65 * Nが上限

### 探索深度の向上
- 同一時間で2-3手深い探索が可能
- 戦術的な読み抜けの減少
- エンドゲームでの精度向上

## 7. リスクと対策

### リスク
1. **同期オーバーヘッド**: 過度な同期によるパフォーマンス低下
2. **探索の重複**: 同じ局面を複数スレッドが探索
3. **メモリ競合**: TTへの同時アクセスによるキャッシュミス

### 対策
1. **最小限の同期**: 必要最小限のロックのみ使用
2. **TT共有の活用**: 重複探索をTTで回避
3. **NUMA対応**: 将来的にNUMA環境での最適化

## 8. 実装優先順位

1. **必須機能**（Phase 1-2）
   - 基本的な並列探索
   - 停止処理の正確性
   - 結果の正しい集約

2. **最適化**（Phase 3）
   - ヘルパースレッド戦略
   - 動的スレッド調整
   - NUMA最適化

3. **将来の拡張**
   - UCIでのスレッド数設定
   - 探索の協調制御
   - より高度な並列アルゴリズム（YBW等）

## 9. 成功指標（修正版）

- [ ] 4スレッドで**2.2倍以上**のNPS向上（dup% ≤ 35%）
- [ ] 停止/タイムアウトの誤差 ≤ 5ms（1秒制限テスト）
- [ ] UCI "go depth 8"でsingle↔multiのPV一致率 ≥ 99%
- [ ] CI全スレッド構成（1,2,4,8）のregression ≤ 2%
- [ ] メモリ使用量の適切な管理
- [ ] 既存テストの全パス
- [ ] Thread Sanitizer/Miriでのデータ競合ゼロ

## 10. 実装上の注意点とベストプラクティス

### Lock-free設計の原則
1. **Atomic操作の適切な使用**
   - fetch_add/fetch_maxで更新の原子性を保証
   - Orderingは用途に応じて選択（Relaxed/Acquire/Release）

2. **競合回避の工夫**
   - History: fetch_addによる加算と周期的減衰
   - PVTable: スレッドローカル + 世代管理
   - best_move/score: 深さフィルタによる無駄な更新抑制

3. **メモリ順序の考慮**
   - 停止フラグ: Release/Acquireで確実な伝搬
   - ベスト更新: 深さチェック後にReleaseで公開

### テスト戦略
1. **段階的検証**
   - Phase 1完了後: Thread Sanitizer実行
   - Phase 2完了後: 重複率測定開始
   - Phase 3完了後: フルベンチマーク

2. **継続的インテグレーション**
   - 全スレッド構成でのregression test
   - PV一致率の自動検証