# Internal Iterative Deepening (IID) Implementation Guide

> このドキュメントは、将棋AIにInternal Iterative Deepening (IID)を実装するためのガイドです。

## 1. 概要

Internal Iterative Deepening (IID) は、置換表に良い手の情報がない場合に、浅い探索を行って良い手を見つける技術です。これにより、手の順序付けが改善され、全体的な探索効率が向上します。

## 2. 理論的背景

### 2.1 基本概念
- **問題**: 置換表にヒットがない、またはTTエントリが浅い場合、手の順序付けが悪くなる
- **解決策**: 浅い探索で良い手を発見し、それを最初に試す
- **トレードオフ**: 追加の探索コスト vs 順序付けによる効率向上

### 2.2 効果的な適用場面
1. PVノード（beta - alpha > 1）または予想されるCut-node（null-window、β = α+1で呼ばれるノード）
2. 十分な探索深度が残っている
3. 置換表に有用な情報がない
4. 重要な局面（評価値が大きく変動する可能性）

**用語説明**:
- **PVノード**: 主変化（Principal Variation）を探索するノード。beta - alpha > 1 で判定
- **Cut-node**: βカットが発生しそうなノード。実装上はnull-window（β = α+1）で呼び出されることが多い

## 3. 実装詳細

### 3.1 基本的な実装

```rust
pub struct IIDParams {
    /// IIDを適用する最小深度
    pub min_depth: i32,  // 推奨: 4-6
    
    /// IIDの深度削減量
    pub depth_reduction: i32,  // 推奨: 2-3
    
    /// PVノードでのみ適用するか
    pub pv_node_only: bool,  // 推奨: false
    
    /// Cut-nodeでの追加削減
    pub cut_node_reduction: i32,  // 推奨: 1
}

impl Searcher {
    fn internal_iterative_deepening(
        &mut self,
        pos: &Position,
        alpha: i32,
        beta: i32,
        depth: i32,
        pv_node: bool,
        cut_node: bool,
    ) -> Option<Move> {
        // 適用条件チェック
        if depth < self.params.iid.min_depth {
            return None;
        }
        
        // TTに既に良い手がある場合はスキップ
        if let Some(tt_entry) = self.tt.probe(pos.hash()) {
            if tt_entry.best_move.is_some() && 
               tt_entry.depth >= depth - 2 {  // -2が一般的（反復深化での重複を防ぐ）
                return None;
            }
        }
        
        // ノードタイプによる適用判定
        if self.params.iid.pv_node_only && !pv_node {
            return None;
        }
        
        // IID探索の深度計算
        let mut iid_depth = depth - self.params.iid.depth_reduction;
        if cut_node && !pv_node {
            iid_depth -= self.params.iid.cut_node_reduction;
        }
        iid_depth = iid_depth.max(1);
        
        // 深度が浅すぎる場合は早期リターン
        if depth <= self.params.iid.depth_reduction {
            return None;
        }
        
        // 浅い探索を実行（目的はTT更新のみ、スコアは使用しない）
        let _ = self.search(pos, alpha, beta, iid_depth, pv_node);
        
        // TTから最善手を取得
        if let Some(tt_entry) = self.tt.probe(pos.hash()) {
            return tt_entry.best_move;
        }
        
        None
    }
}
```

### 3.2 探索への統合

```rust
impl Searcher {
    fn search(
        &mut self,
        pos: &Position,
        mut alpha: i32,
        mut beta: i32,
        depth: i32,
        pv_node: bool,
    ) -> i32 {
        // ... 前処理 ...
        
        // 置換表の確認
        let tt_move = if let Some(tt_entry) = self.tt.probe(pos.hash()) {
            tt_entry.best_move
        } else {
            None
        };
        
        // ノードタイプの判定
        let pv_node = beta - alpha > 1;
        // cut_nodeは親ノードから渡されるか、子ノードをnull-windowで呼ぶときにtrueにする
        let cut_node = false; // 実際の実装では親から渡される
        
        // IIDの適用
        let iid_move = if tt_move.is_none() {
            self.internal_iterative_deepening(
                pos, alpha, beta, depth, pv_node, cut_node
            )
        } else {
            None
        };
        
        // 手の順序付けに使用する最善手
        let first_move = tt_move.or(iid_move);
        
        // MovePicker初期化
        let mut move_picker = MovePicker::new(
            pos,
            first_move,
            &self.history,
            &self.killers[self.ply],
        );
        
        // ... 探索ループ ...
    }
}
```

### 3.3 高度な実装

#### 3.3.1 適応的IID
```rust
pub struct AdaptiveIID {
    /// ノードタイプ別の成功率統計
    pv_success_rate: f32,
    cut_success_rate: f32,
    all_success_rate: f32,
    
    /// 統計カウンタ
    pv_attempts: u32,
    pv_hits: u32,
    cut_attempts: u32,
    cut_hits: u32,
}

impl AdaptiveIID {
    pub fn should_apply(
        &self,
        depth: i32,
        pv_node: bool,
        cut_node: bool,
        eval: Option<i32>,
    ) -> bool {
        // 基本条件
        if depth < 4 {
            return false;
        }
        
        // 成功率に基づく判定
        let success_rate = if pv_node {
            self.pv_success_rate
        } else if cut_node {
            self.cut_success_rate
        } else {
            self.all_success_rate
        };
        
        // 成功率が低い場合は深度要求を厳しくする
        let min_depth = if success_rate < 0.3 {
            6
        } else if success_rate < 0.5 {
            5
        } else {
            4
        };
        
        depth >= min_depth
    }
    
    pub fn update_stats(&mut self, pv_node: bool, cut_node: bool, found_move: bool) {
        if pv_node {
            self.pv_attempts += 1;
            if found_move {
                self.pv_hits += 1;
            }
            self.pv_success_rate = self.pv_hits as f32 / self.pv_attempts.max(1) as f32;
        } else if cut_node {
            self.cut_attempts += 1;
            if found_move {
                self.cut_hits += 1;
            }
            self.cut_success_rate = self.cut_hits as f32 / self.cut_attempts.max(1) as f32;
        }
    }
}
```

#### 3.3.2 多段階IID
```rust
fn multi_level_iid(
    &mut self,
    pos: &Position,
    alpha: i32,
    beta: i32,
    depth: i32,
) -> Option<Move> {
    // 深度に応じて複数回のIIDを実行
    let levels = match depth {
        0..=7 => vec![depth - 2],
        8..=11 => vec![depth - 4, depth - 2],
        12..=15 => vec![depth - 6, depth - 4, depth - 2],
        _ => vec![depth - 8, depth - 5, depth - 2],
    };
    
    let mut best_move = None;
    
    for &iid_depth in &levels {
        if iid_depth < 1 {
            continue;
        }
        
        // レベルに応じてnull-windowとPV探索を使い分ける
        let (a, b) = if iid_depth < depth - 4 {
            (alpha, alpha + 1)  // 浅いレベルではnull-window
        } else {
            (alpha, beta)       // 深いレベルではPV探索
        };
        self.search(pos, a, b, iid_depth, b - a > 1);
        
        if let Some(tt_entry) = self.tt.probe(pos.hash()) {
            if let Some(move_) = tt_entry.best_move {
                best_move = Some(move_);
                // 十分良い手が見つかった場合は早期終了
                // 100は経験的な値（ポーン約1個分の優位）
                if tt_entry.bound == Bound::Exact &&
                   tt_entry.value > alpha + 100 {
                    break;
                }
            }
        }
    }
    
    best_move
}
```

### 3.4 最適化テクニック

#### 3.4.1 IIDキャッシュ
```rust
/// 最近のIID結果をキャッシュ
pub struct IIDCache {
    entries: Vec<IIDCacheEntry>,
    size_mask: usize,
}

struct IIDCacheEntry {
    hash: u64,
    best_move: Option<Move>,
    depth: i32,
    timestamp: u32,
}

impl IIDCache {
    pub fn probe(&self, hash: u64, depth: i32) -> Option<Move> {
        let index = (hash as usize) & self.size_mask;
        let entry = &self.entries[index];
        
        if entry.hash == hash && 
           entry.depth >= depth - 1 &&
           entry.timestamp + 100 > self.search_id {  // 年齢チェック
            return entry.best_move;
        }
        None
    }
    
    pub fn store(&mut self, hash: u64, best_move: Option<Move>, depth: i32) {
        let index = (hash as usize) & self.size_mask;
        self.entries[index] = IIDCacheEntry {
            hash,
            best_move,
            depth,
            timestamp: self.current_timestamp(),
        };
    }
}
```

#### 3.4.2 選択的IID
```rust
/// 局面の特徴に基づいてIIDを適用
fn selective_iid(&self, pos: &Position, depth: i32) -> bool {
    // 駒の数が少ない終盤では積極的に適用
    if pos.piece_count() < 10 {
        return depth >= 3;
    }
    
    // 王手がかかっている場合は適用しない
    if pos.in_check() {
        return false;
    }
    
    // 戦術的に複雑な局面では適用
    let mobility = self.calculate_mobility(pos);
    if mobility > 50 {  // 多くの合法手がある
        return depth >= 4;
    }
    
    // 通常の条件
    depth >= 5
}
```

## 4. テストと検証

### 4.1 効果測定
```rust
#[test]
fn test_iid_effectiveness() {
    let test_positions = load_tactical_positions();
    let mut with_iid = Searcher::new();
    let mut without_iid = Searcher::new();
    
    with_iid.params.iid.min_depth = 4;
    without_iid.params.iid.min_depth = 999;  // 実質無効
    
    let mut move_ordering_improvement = 0.0;
    let mut node_count_ratio = 0.0;
    
    for pos in test_positions {
        // 両方で探索（search()はスコアを返すので、ノード数は別途取得）
        with_iid.nodes = 0;
        without_iid.nodes = 0;
        
        let _ = with_iid.search(&pos, -INFINITY, INFINITY, 10);
        let _ = without_iid.search(&pos, -INFINITY, INFINITY, 10);
        
        let nodes_with = with_iid.nodes;
        let nodes_without = without_iid.nodes;
        
        node_count_ratio += nodes_with as f64 / nodes_without.max(1) as f64;
        
        // 最初の手のカット率を比較
        move_ordering_improvement += with_iid.first_move_cut_rate();
    }
    
    println!("Node reduction: {:.1}%", 
             (1.0 - node_count_ratio / test_positions.len() as f64) * 100.0);
    println!("Move ordering improvement: {:.1}%", 
             move_ordering_improvement / test_positions.len() as f64 * 100.0);
}
```

### 4.2 パフォーマンス指標
- **ノード削減率**: 5-15%
- **最初の手のカット率向上**: 10-20%
- **Elo向上**: +5-15 Elo（将棋での現実的な値）
- **オーバーヘッド**: 2-5%

## 5. 実装チェックリスト

- [ ] 基本的なIID実装
- [ ] 置換表との適切な連携
- [ ] ノードタイプ別の深度調整
- [ ] 適応的IIDの実装
- [ ] IIDキャッシュの実装
- [ ] 多段階IIDの実装
- [ ] 統計収集機能
- [ ] パフォーマンステスト
- [ ] パラメータチューニング

## 6. パラメータチューニング

### 6.1 推奨設定
```rust
// 保守的な設定（安定性重視）
const CONSERVATIVE_IID: IIDParams = IIDParams {
    min_depth: 6,
    depth_reduction: 2,
    pv_node_only: true,
    cut_node_reduction: 0,
};

// バランス設定（推奨）
const BALANCED_IID: IIDParams = IIDParams {
    min_depth: 5,
    depth_reduction: 2,
    pv_node_only: false,
    cut_node_reduction: 1,
};

// アグレッシブ設定（性能重視）
const AGGRESSIVE_IID: IIDParams = IIDParams {
    min_depth: 4,
    depth_reduction: 3,
    pv_node_only: false,
    cut_node_reduction: 1,
};
```

### 6.2 自動チューニング
```bash
# グリッドサーチ
for min_depth in 3 4 5 6; do
    for reduction in 1 2 3; do
        ./tune-iid --min-depth $min_depth --reduction $reduction
    done
done
```

## 7. トラブルシューティング

### 7.1 効果が見られない
- 置換表のサイズを確認（小さすぎると効果減）
- MovePickerの実装を確認
- より深い探索深度でテスト

### 7.2 オーバーヘッドが大きい
- min_depthを増やす
- pv_node_onlyを有効にする
- IIDキャッシュの導入

### 7.3 探索が不安定
- depth_reductionを小さくする
- 多段階IIDの導入を検討
- 適応的IIDで成功率を監視

## 8. 将棋固有の考慮事項

### 8.1 高い分岐数への対応
将棋の平均分岐数はチェスの約2倍（約80手）であるため：
- min_depthをチェスより深めに設定
- depth_reductionを慎重に設定
- コスト対効果を実測で検証

### 8.2 他の探索技術との連携

#### Late Move Reductions (LMR) との整合
```rust
// IIDで得た最善手はLMRの除外リストに追加
if move == iid_move {
    // LMRを適用しない
    reduction = 0;
}
```

#### Aspiration Search との相互作用
```rust
// Aspiration失敗直後のIID深度を増やす
if aspiration_failed {
    iid_depth += 1;
}
```

### 8.3 統計の温度効果（指数移動平均）
```rust
const EMA_ALPHA: f32 = 0.1;  // 新しいデータの重み

pub fn update_ema_stats(&mut self, success: bool) {
    let value = if success { 1.0 } else { 0.0 };
    self.success_rate = self.success_rate * (1.0 - EMA_ALPHA) + value * EMA_ALPHA;
}
```

## 9. 実装例

```rust
// 完全な実装例
impl Searcher {
    fn search_with_iid(
        &mut self,
        pos: &Position,
        mut alpha: i32,
        mut beta: i32,
        depth: i32,
        cut_node: bool,  // 親ノードから渡される
    ) -> i32 {
        let pv_node = beta - alpha > 1;
        
        // TTプローブ
        let tt_hit = self.tt.probe(pos.hash());
        let tt_move = tt_hit.and_then(|e| e.best_move);
        
        // IID適用判定
        if tt_move.is_none() && 
           depth >= self.params.iid.min_depth &&
           (pv_node || depth >= self.params.iid.min_depth + 2) {
            
            // IIDキャッシュ確認
            if let Some(cached_move) = self.iid_cache.probe(pos.hash(), depth) {
                // キャッシュヒット
            } else {
                // IID実行
                let iid_depth = depth - self.params.iid.depth_reduction;
                self.search(pos, alpha, beta, iid_depth, pv_node);
                
                // 結果を取得
                if let Some(tt_entry) = self.tt.probe(pos.hash()) {
                    if let Some(best_move) = tt_entry.best_move {
                        self.iid_cache.store(pos.hash(), Some(best_move), depth);
                    }
                }
            }
        }
        
        // 通常の探索継続...
    }
}
```

### 9.1 Delayed IIDの実装
```rust
// PVノードでbeta-cutが発生しなかった後にのみIIDを実行
if pv_node && !beta_cut_occurred && depth >= 8 {
    let iid_move = self.internal_iterative_deepening(
        pos, alpha, beta, depth, true, false
    );
    // IIDで得た手で再探索
    if let Some(move_) = iid_move {
        let score = -self.search(&pos.make_move(move_), -beta, -alpha, depth - 1, false);
        // ...
    }
}
```
