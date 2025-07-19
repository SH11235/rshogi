# ProbCut Implementation Guide

> このドキュメントは、将棋AIにProbCutを実装するためのガイドです。

## 1. 概要

ProbCut (Probability Cut) は、浅い探索の結果から深い探索の結果を統計的に予測し、早期に枝刈りを行う技術です。計算資源を節約しながら、探索の質を維持できます。

## 2. 理論的背景

### 2.1 基本原理
- 浅い探索の評価値と深い探索の評価値には強い相関がある
- この相関を利用して、深い探索を省略できるかを判定
- 統計的に安全な範囲でのみ枝刈りを実行

### 2.2 数学的基礎
```
深い探索の予測値 v = a·v' + b + ε
v: 深い探索の評価値
v': 浅い探索の評価値
a: 線形回帰の傾き (slope)
b: 線形回帰の切片 (intercept)
ε: 誤差項（平均0、分散σ²の正規分布と仮定）
```

### 2.3 枝刈り条件
ProbCutでは以下の条件で枝刈りを行います：

#### β側カット（fail-high）
```
v' ≥ (β - b + T·σ) / a
T: 安全マージン（標準偏差の倍数）
```

#### α側カット（fail-low）
```
v' ≤ (α - b - T·σ) / a
```

## 3. 実装詳細

### 3.1 基本的な実装

```rust
pub struct ProbCutParams {
    /// ProbCutを適用する最小深度
    pub min_depth: i32,  // 推奨: 7 (depth_reduction + 2以上を保証)
    
    /// 浅い探索の深度削減量
    pub depth_reduction: i32,  // 推奨: 4
    
    /// 安全マージン（標準偏差の倍数）
    pub safety_margin: f32,  // 推奨: 1.5
    
    /// 線形回帰パラメータ
    pub slope: f32,      // 推奨: 0.9-1.1 (理論式のa)
    pub intercept: f32,  // 推奨: -50 to 50 (理論式のb)
    pub std_dev: f32,    // 推奨: 50-150 (誤差の標準偏差σ)
}

impl Searcher {
    fn prob_cut(
        &mut self,
        pos: &Position,
        alpha: i32,
        beta: i32,
        depth: i32,
    ) -> Option<i32> {
        // 適用条件チェック
        if depth < self.params.probcut.min_depth {
            return None;
        }
        
        // 深度が十分あることを保証
        let reduced_depth = depth - self.params.probcut.depth_reduction;
        if reduced_depth < 1 {
            return None;
        }
        
        // 理論式に基づく境界値計算
        // v' ≥ (β - b + T·σ) / a
        let margin = (self.params.probcut.safety_margin * self.params.probcut.std_dev).round() as i32;
        let beta_bound = ((beta - self.params.probcut.intercept as i32 + margin) as f32 
                         / self.params.probcut.slope).round() as i32;
        
        // β側カット（fail-high）の試行
        let value = self.search(
            pos,
            beta_bound - 1,
            beta_bound,
            reduced_depth,
            false  // ProbCut無効化フラグ
        );
        
        if value >= beta_bound {
            // 統計的に深い探索でもベータカットする可能性が高い
            return Some(beta);
        }
        
        // α側カット（fail-low）の試行
        // v' ≤ (α - b - T·σ) / a
        let alpha_bound = ((alpha - self.params.probcut.intercept as i32 - margin) as f32 
                          / self.params.probcut.slope).round() as i32;
        
        let value = self.search(
            pos,
            alpha_bound,
            alpha_bound + 1,
            reduced_depth,
            false
        );
        
        if value <= alpha_bound {
            // 統計的に深い探索でもアルファカットする可能性が高い
            return Some(alpha);
        }
        
        None
    }
}
```

### 3.2 高度な実装

#### 3.2.1 動的パラメータ調整
```rust
impl ProbCutParams {
    /// ゲームフェーズに応じたパラメータ調整
    pub fn adjust_for_phase(&mut self, phase: GamePhase) {
        match phase {
            GamePhase::Opening => {
                // 序盤は控えめに
                self.safety_margin = 2.0;
                self.min_depth = 7;
            }
            GamePhase::EndGame => {
                // 終盤は積極的に
                self.safety_margin = 1.0;
                self.min_depth = 4;
            }
            _ => {}
        }
    }
    
    /// 残り時間に応じた調整
    pub fn adjust_for_time(&mut self, time_pressure: f32) {
        if time_pressure > 0.8 {
            // 時間がない場合は積極的に
            self.safety_margin *= 0.8;
            self.depth_reduction += 1;
        }
    }
}
```

#### 3.2.2 複数の閾値による段階的ProbCut
```rust
fn multi_prob_cut(
    &mut self,
    pos: &Position,
    alpha: i32,
    beta: i32,
    depth: i32,
) -> Option<i32> {
    // 異なる深度で複数回試行
    let reductions = [4, 3, 2];
    let margins = [2.0, 1.5, 1.0];
    
    for (&reduction, &margin) in reductions.iter().zip(margins.iter()) {
        if depth < reduction + 2 {
            continue;
        }
        
        let margin = (margin * self.params.probcut.std_dev).round() as i32;
        let beta_bound = ((beta - self.params.probcut.intercept as i32 + margin) as f32 
                         / self.params.probcut.slope).round() as i32;
        
        let value = self.search(
            pos,
            beta_bound - 1,
            beta_bound,
            depth - reduction,
            false
        );
        
        if value >= beta_bound {
            return Some(beta);
        }
        
        // 早期脱出：明らかに見込みがない
        // slope/interceptも考慮した境界値での判定
        let early_exit_bound = ((alpha - 300 - self.params.probcut.intercept as i32) as f32 
                               / self.params.probcut.slope).round() as i32;
        if value < early_exit_bound {
            break;
        }
    }
    
    None
}
```

### 3.3 統計的パラメータの学習

```rust
/// オフラインでの統計パラメータ学習
pub struct ProbCutLearner {
    samples: Vec<(i32, i32, i32)>,  // (shallow, deep, depth)
}

impl ProbCutLearner {
    pub fn add_sample(&mut self, shallow: i32, deep: i32, depth: i32) {
        self.samples.push((shallow, deep, depth));
    }
    
    pub fn compute_parameters(&self, depth: i32) -> (f32, f32, f32) {
        let depth_samples: Vec<_> = self.samples.iter()
            .filter(|s| s.2 == depth)
            .collect();
        
        if depth_samples.len() < 100 {
            // デフォルト値
            return (1.0, 0.0, 100.0);
        }
        
        // 線形回帰
        let n = depth_samples.len() as f32;
        let sum_x: f32 = depth_samples.iter().map(|s| s.0 as f32).sum();
        let sum_y: f32 = depth_samples.iter().map(|s| s.1 as f32).sum();
        let sum_xx: f32 = depth_samples.iter().map(|s| (s.0 * s.0) as f32).sum();
        let sum_xy: f32 = depth_samples.iter()
            .map(|s| (s.0 * s.1) as f32).sum();
        
        let slope = (n * sum_xy - sum_x * sum_y) / (n * sum_xx - sum_x * sum_x);
        let intercept = (sum_y - slope * sum_x) / n;
        
        // 標準偏差の計算
        let residuals: Vec<f32> = depth_samples.iter()
            .map(|s| s.1 as f32 - (slope * s.0 as f32 + intercept))
            .collect();
        let std_dev = (residuals.iter().map(|r| r * r).sum::<f32>() / n).sqrt();
        
        (slope, intercept, std_dev)
    }
}
```

## 4. 実装上の注意点

### 4.1 パフォーマンス考慮事項
- ProbCutは追加の探索を必要とするため、過度な使用は逆効果
- 十分な深度がある場合のみ適用
- 置換表の活用で重複探索を避ける

### 4.2 Null-Moveとの相互作用

ProbCutとNull-Move Pruningを併用する場合の注意点：

```rust
// ❌ 悪い例：過剰枝刈りのリスク
if allow_probcut {
    if let Some(value) = self.prob_cut(pos, alpha, beta, depth) {
        return value;
    }
}
if allow_null_move {
    if let Some(value) = self.null_move_pruning(pos, beta, depth) {
        return value;
    }
}

// ✅ 良い例：相互排他的な適用
if allow_probcut && !used_null_move {
    if let Some(value) = self.prob_cut(pos, alpha, beta, depth) {
        return value;
    }
} else if allow_null_move && !used_probcut {
    if let Some(value) = self.null_move_pruning(pos, beta, depth) {
        return value;
    }
}
```

### 4.3 実装の落とし穴
```rust
// ❌ 悪い例：無限ループの可能性
fn prob_cut(&mut self, pos: &Position, beta: i32, depth: i32) -> Option<i32> {
    // depth - 4 でも同じprob_cutが呼ばれる可能性
    let value = self.search(pos, beta - 1, beta, depth - 4, false);
}

// ✅ 良い例：ProbCut無効化フラグ
fn search(
    &mut self,
    pos: &Position,
    alpha: i32,
    beta: i32,
    depth: i32,
    allow_probcut: bool,  // フラグで制御
) -> i32 {
    if allow_probcut && depth >= 5 {
        if let Some(value) = self.prob_cut(pos, beta, depth) {
            return value;
        }
    }
}
```

## 5. テストと検証

### 5.1 単体テスト
```rust
#[test]
fn test_probcut_correlation() {
    let positions = load_test_positions();
    let mut learner = ProbCutLearner::new();
    
    for pos in positions {
        let shallow = search(&pos, -INFINITY, INFINITY, 6);
        let deep = search(&pos, -INFINITY, INFINITY, 10);
        learner.add_sample(shallow, deep, 10);
    }
    
    let (slope, intercept, std_dev) = learner.compute_parameters(10);
    
    // 相関係数のチェック
    assert!(slope > 0.8 && slope < 1.2);
    assert!(std_dev < 200);
}
```

### 5.2 性能測定
- **Elo向上**: +5-15 Elo (将棋では他の技術との競合により控えめ)
- **ノード削減率**: 10-20%
- **時間短縮**: 5-15%
- **注意**: Null-Moveとの併用時は効果が減少する可能性あり

## 6. パラメータチューニング

### 6.1 深度別パラメータ
```rust
const PROBCUT_PARAMS: [(i32, f32, f32, f32); 10] = [
    // (depth, slope, intercept, std_dev)
    (5,  0.95, -10.0, 120.0),
    (6,  0.96, -15.0, 110.0),
    (7,  0.97, -20.0, 100.0),
    (8,  0.98, -25.0, 95.0),
    (9,  0.99, -30.0, 90.0),
    (10, 1.00, -35.0, 85.0),
    // ...
];
```

### 6.2 自動チューニング
```bash
# 統計データ収集
./shogi-engine --collect-probcut-stats --games 10000

# パラメータ最適化
./shogi-engine --optimize-probcut --input stats.json

# 検証
./shogi-engine --test-probcut --params optimized.json
```

## 7. 実装チェックリスト

- [ ] 基本的なProbCut実装
- [ ] 統計パラメータの設定
- [ ] 無限ループ防止機構
- [ ] ゲームフェーズ別調整
- [ ] 時間管理との連携
- [ ] パラメータ学習機能
- [ ] 置換表との統合（shallow search用の別スロット検討）
- [ ] 性能測定とチューニング
- [ ] Null-Moveとの相互作用の最適化

## 8. トラブルシューティング

### 8.1 効果が見られない
- パラメータの再調整（特にstd_dev）
- 適用深度の見直し
- 相関分析の実施

### 8.2 探索が不安定
- safety_marginを大きくする
- min_depthを増やす
- 多段階ProbCutの導入

### 8.3 時間超過
- ProbCut自体のコストを測定
- 適用頻度の調整
- 置換表ヒット率の確認
