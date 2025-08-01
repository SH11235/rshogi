# 統一検索フレームワーク設計

## 概要

本ドキュメントでは、Shogi Engineの検索エンジンの統一アーキテクチャについて説明します。このフレームワークは、const genericsを活用して異なる探索戦略を共通のインターフェースで実装し、コードの再利用性と保守性を大幅に向上させています。

## アーキテクチャの詳細

### 1. コア設計思想

統一検索フレームワークは以下の設計原則に基づいています：

- **型安全性**: const genericsにより探索戦略をコンパイル時に決定
- **モジュール性**: 各コンポーネント（枝刈り、移動順序付け、PV管理など）を独立したモジュールとして実装
- **拡張性**: 新しい探索戦略を簡単に追加できる構造
- **パフォーマンス**: 実行時のオーバーヘッドを最小限に抑える

### 2. 主要コンポーネント

#### 2.1 UnifiedSearcher構造体

```rust
pub struct UnifiedSearcher<
    E,
    const USE_TT: bool = true,
    const USE_PRUNING: bool = true,
    const TT_SIZE_MB: usize = 16,
> where
    E: Evaluator + Send + Sync + 'static,
{
    evaluator: E,
    tt: Option<TranspositionTable>,
    history: History,
    ordering: MoveOrdering,
    pv_table: PVTable,
    stats: SearchStats,
    context: SearchContext,
}
```

評価関数の型（E）とconst booleanパラメータにより、コンパイル時に探索戦略が決定されます。

#### 2.2 モジュール構成

- **SearchContext**: 探索の状態を管理（履歴テーブル、キラームーブなど）
- **PruningModule**: 枝刈り戦略を実装（NULL移動、futility pruning、LMRなど）
- **OrderingModule**: 移動順序付けを実装（MVV-LVA、履歴ヒューリスティックなど）
- **NodeModule**: ノード処理ロジックを実装（alpha-beta探索、静止探索など）

### 3. const genericsの利点

#### 3.1 コンパイル時最適化

```rust
impl<const ENGINE_TYPE: EngineType> UnifiedSearcher<ENGINE_TYPE> {
    fn should_use_null_move(&self) -> bool {
        match ENGINE_TYPE {
            EngineType::Basic => false,
            EngineType::Enhanced | EngineType::Nnue | EngineType::EnhancedNnue => true,
        }
    }
}
```

このようなコードは、コンパイル時に定数として評価され、不要な分岐が削除されます。

#### 3.2 型安全な戦略選択

エンジンタイプごとに異なる探索深度や枝刈りパラメータを型レベルで保証できます：

```rust
const fn get_lmr_reduction<const ENGINE_TYPE: EngineType>(depth: i32, move_index: usize) -> i32 {
    match ENGINE_TYPE {
        EngineType::Basic => 0,
        EngineType::Enhanced | EngineType::Nnue | EngineType::EnhancedNnue => {
            // LMR計算ロジック
        }
    }
}
```

#### 3.3 ゼロコスト抽象化

const genericsにより、抽象化による実行時オーバーヘッドがありません。各エンジンタイプは独立したバイナリコードとして生成されます。

### 4. モジュール詳細

#### 4.1 PruningModule

枝刈り戦略を管理するモジュール：

- **Null Move Pruning**: Enhanced以上で有効
- **Futility Pruning**: 全エンジンで実装（閾値が異なる）
- **Late Move Reduction (LMR)**: Enhanced以上で有効
- **Razoring**: EnhancedNnueでのみ有効

#### 4.2 OrderingModule

移動順序付けを管理するモジュール：

- **MVV-LVA**: 全エンジンで実装
- **キラームーブ**: Enhanced以上で有効
- **履歴ヒューリスティック**: Enhanced以上で有効
- **カウンタームーブ**: EnhancedNnueで有効

#### 4.3 NodeModule

ノード処理を管理するモジュール：

- **Alpha-Beta探索**: 全エンジンで実装
- **Principal Variation Search**: Enhanced以上で有効
- **静止探索**: 実装の深さがエンジンタイプで異なる
- **探索延長**: Enhanced以上で有効

### 5. 実装例

#### 5.1 エンジンタイプによる条件分岐

```rust
impl<const ENGINE_TYPE: EngineType> UnifiedSearcher<ENGINE_TYPE> {
    fn search_internal(&mut self, alpha: Score, beta: Score, depth: i32) -> Score {
        // Null Move Pruning
        if ENGINE_TYPE.supports_null_move() && self.can_do_null_move() {
            let null_score = self.null_move_search(beta, depth);
            if null_score >= beta {
                return null_score;
            }
        }
        
        // 移動生成と順序付け
        let moves = self.ordering.order_moves(&self.context);
        
        // 各移動を探索
        for (index, move_) in moves.iter().enumerate() {
            let reduction = self.pruning.get_lmr_reduction(depth, index);
            // ...
        }
    }
}
```

#### 5.2 モジュール間の連携

```rust
// PruningModuleとOrderingModuleの連携
let moves = self.ordering.order_moves(&self.context);
let pruned_moves = self.pruning.filter_moves(moves, depth);

// NodeModuleでの処理
let score = self.node.process_node(pruned_moves, alpha, beta, depth);
```

### 6. 今後の拡張方針

#### 6.1 新しい探索アルゴリズムの追加

##### Monte Carlo Tree Search (MCTS)の追加

MCTSは従来のアルファベータ探索とは異なるアプローチのため、新しいモジュールとして実装：

```rust
// MCTSのための新しい探索構造体
pub struct MctsSearcher<E: Evaluator> {
    evaluator: E,
    tree: MctsTree,
    simulations: usize,
}

// UnifiedSearcherと共通のインターフェースを実装
impl<E: Evaluator> Searcher for MctsSearcher<E> {
    fn search(&mut self, pos: &mut Position, limits: SearchLimits) -> SearchResult {
        // MCTS実装
    }
}
```

または、UnifiedSearcherを拡張して条件付きコンパイルで切り替え：

```rust
pub struct UnifiedSearcher<
    E,
    const USE_TT: bool = true,
    const USE_PRUNING: bool = true,
    const USE_MCTS: bool = false,  // 新規追加
    const TT_SIZE_MB: usize = 16,
>
```

#### 6.2 並列探索の統合

##### Lazy SMP（Lazy Symmetric MultiProcessing）の実装

複数のスレッドで同じ局面を異なる深さ・パラメータで探索：

```rust
pub struct ParallelSearcher<E, const USE_TT: bool, const USE_PRUNING: bool> {
    // 各スレッドが独自のUnifiedSearcherを持つ
    searchers: Vec<UnifiedSearcher<E, USE_TT, USE_PRUNING, 0>>, // TTは共有
    shared_tt: Arc<TranspositionTable>,
    thread_pool: ThreadPool,
}

impl<E: Evaluator + Clone> ParallelSearcher<E, true, true> {
    pub fn new(evaluator: E, num_threads: usize) -> Self {
        let shared_tt = Arc::new(TranspositionTable::new(256)); // 共有TT
        let searchers = (0..num_threads)
            .map(|_| {
                let mut searcher = UnifiedSearcher::new(evaluator.clone());
                searcher.set_shared_tt(shared_tt.clone());
                searcher
            })
            .collect();
        
        Self {
            searchers,
            shared_tt,
            thread_pool: ThreadPool::new(num_threads),
        }
    }
}
```

##### YBWC（Young Brothers Wait Concept）の実装

より高度な並列探索アルゴリズム：

```rust
pub struct YbwcSearcher<E: Evaluator> {
    master: UnifiedSearcher<E, true, true, 16>,
    helpers: Vec<UnifiedSearcher<E, true, true, 0>>,
    split_points: Arc<Mutex<Vec<SplitPoint>>>,
}
```

#### 6.3 学習機能の統合

##### 強化学習のための探索

評価関数の学習に必要な情報を収集する探索モード：

```rust
pub struct LearningSearcher<E: Evaluator + Learnable> {
    searcher: UnifiedSearcher<E, true, true, 16>,
    learning_data: Vec<TrainingExample>,
    search_log: SearchLog,
}

pub trait Learnable: Evaluator {
    fn extract_features(&self, pos: &Position) -> Features;
    fn update_weights(&mut self, examples: &[TrainingExample]);
}

impl<E: Evaluator + Learnable> LearningSearcher<E> {
    pub fn search_and_learn(&mut self, pos: &mut Position, limits: SearchLimits) -> SearchResult {
        // 通常の探索を実行
        let result = self.searcher.search(pos, limits);
        
        // 探索中の全ての局面と評価値を記録
        self.collect_training_data(&self.searcher.get_search_tree());
        
        result
    }
    
    fn collect_training_data(&mut self, tree: &SearchTree) {
        for node in tree.nodes() {
            self.learning_data.push(TrainingExample {
                position: node.position.clone(),
                score: node.score,
                depth: node.depth,
                best_move: node.best_move,
            });
        }
    }
}
```

##### 教師あり学習のためのデータ生成

プロ棋士の棋譜や強いエンジンの探索結果から学習データを生成：

```rust
pub struct SupervisedDataGenerator<E: Evaluator> {
    searcher: UnifiedSearcher<E, true, true, 32>, // 大きなTTで深い探索
    output_format: TrainingDataFormat,
}

impl<E: Evaluator> SupervisedDataGenerator<E> {
    pub fn generate_from_games(&mut self, games: &[Game]) -> Vec<TrainingExample> {
        let mut examples = Vec::new();
        
        for game in games {
            for position in game.positions() {
                // 深い探索で「正解」の手と評価値を生成
                let result = self.searcher.search(
                    &mut position.clone(), 
                    SearchLimitsBuilder::default().depth(20).build()
                );
                
                examples.push(TrainingExample {
                    position: position.clone(),
                    best_move: result.best_move,
                    score: result.score,
                    pv: result.stats.pv.clone(),
                });
            }
        }
        
        examples
    }
}
```

### 7. パフォーマンス特性

#### 7.1 メモリ使用量

- 各エンジンタイプで必要なモジュールのみがインスタンス化される
- 不要な機能のメモリオーバーヘッドがない

#### 7.2 実行速度

- コンパイル時の最適化により、実行時の条件分岐が最小限
- インライン化により関数呼び出しのオーバーヘッドが削減

#### 7.3 バイナリサイズ

- 各エンジンタイプは独立したコードとして生成される
- 使用しないエンジンタイプのコードは含まれない（リンカーで削除）

### 8. テスト戦略

#### 8.1 ユニットテスト

各モジュールは独立してテスト可能：

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_null_move_pruning() {
        let pruning = PruningModule::<{ EngineType::Enhanced }>::new();
        // テスト実装
    }
}
```

#### 8.2 統合テスト

異なるエンジンタイプの動作を検証：

```rust
#[test]
fn test_engine_behavior_consistency() {
    let basic = UnifiedSearcher::<{ EngineType::Basic }>::new();
    let enhanced = UnifiedSearcher::<{ EngineType::Enhanced }>::new();
    // 同じ局面での探索結果を比較
}
```

### 9. まとめ

統一検索フレームワークは、Rustのconst generics機能を最大限活用し、異なる探索戦略を型安全かつ高性能に実装しています。このアーキテクチャにより：

- コードの重複を排除し、保守性が向上
- 新しい探索戦略の追加が容易
- パフォーマンスの低下なしに抽象化を実現
- 型システムによる安全性の保証

今後も、このフレームワークを基盤として、より高度な探索アルゴリズムの実装を進めていく予定です。