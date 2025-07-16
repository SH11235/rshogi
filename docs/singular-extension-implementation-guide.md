# Singular Extension Implementation Guide

> このドキュメントは、将棋AIにSingular Extension (SE)を実装するためのガイドです。

## 1. 概要

Singular Extensionは、特定の手が他の手と比べて著しく優れている場合に、その手の探索を延長する技術です。これにより、重要な手順を見逃すリスクを減らし、探索の質を向上させます。

## 2. 理論的背景

### 2.1 基本概念
- **Singular Move**: 他の手と比べて著しく優れている手（Stockfish 派生実装では「2 番手より margin だけ良い手」を定量化します）
- **Extension**: 通常の探索深度を1-2手延長する

### 2.2 適用条件
1. 十分な探索深度（8 ply 以上）
2. Cut-node
3. 置換表に良い手の情報がある
4. 現在の局面が静的に良好（ SE 固有というより Extension 全般のヒューリスティックで、必須条件ではありません）

## 3. 実装詳細

### 3.1 基本的な実装フロー

```rust
// 疑似コード
if depth >= SINGULAR_EXTENSION_DEPTH &&
   tt_entry.exists() &&
   tt_entry.bound == Bound::Lower &&  // fail-high (LOWER) でのみ SE を試みます
   tt_entry.depth >= depth - 3 {
    
    // singular_betaの計算
    let margin = calculate_singular_margin(depth);
    let singular_beta = tt_value - margin;
    let singular_depth = (depth - 1) / 2;  // Stockfish由来の式
    
    // tt_moveを除外して探索
    excluded_move = Some(tt_move);
    let value = search(pos, singular_beta - 1, singular_beta, singular_depth);
    excluded_move = None;
    
    if value < singular_beta {
        // この手は特別に良い - 延長
        extension = 1;
        
        // 二重延長（Shogiでは慎重に適用）
        if double_extension_enabled && value < singular_beta - 50 {
            extension = 2;
        }
    } else if singular_beta >= beta {
        // 他にもβを超える手があった＝単独ではない」ので延長せず return
        return singular_beta;
    }
}

// マージン計算関数（グローバル定数を使用）
fn calculate_singular_margin(depth: i32) -> i32 {
    const SINGULAR_MARGIN_BASE: i32 = 80;  // 基本マージン (centipawns)
    const SINGULAR_MARGIN_SLOPE: i32 = 10; // 深度比例係数
    
    SINGULAR_MARGIN_BASE + depth * SINGULAR_MARGIN_SLOPE
}
```

### 3.2 実装上の注意点

#### 3.2.1 パフォーマンスへの影響
- SEは追加の探索を必要とするため、適用条件を慎重に設定する
- 浅い探索での誤判定を防ぐため、十分な深度制限を設ける

#### 3.2.2 パラメータチューニング
```rust
// グローバル定数として定義（深度依存のため構造体ではなく定数で管理）
const SINGULAR_MARGIN_BASE: i32 = 80;    // 基本マージン
const SINGULAR_MARGIN_SLOPE: i32 = 10;   // 深度比例係数

pub struct SingularExtensionParams {
    /// SEを適用する最小深度
    pub min_depth: i32,  // 推奨: 7-10
    
    /// 二重延長の有効/無効フラグ
    /// Shogiは探索木が急激に肥大化するため、double-extensionが
    /// 効果より害の方が大きい場合が多い。デフォルトでは無効を推奨
    pub double_extension_enabled: bool,  // 推奨: false
    
    /// 二重延長の閾値（有効時のみ使用）
    /// 終盤局面のみ、またはLMRと併用時のみ有効化を検討
    pub double_extension_margin: i32,  // 推奨: 50-100
    
    /// TTエントリの深度要求
    pub tt_depth_margin: i32,  // 推奨: 3
}
```

### 3.3 高度な実装

#### 3.3.1 履歴ヒューリスティックとの統合
```rust
// 履歴スコアによるフィルタリング
let history_score = history.get(pos, tt_move);
let max_history = moves.iter()
    .map(|m| history.get(pos, *m))
    .max()
    .unwrap_or(0);

// 相対的に良い手のみSE候補とする（割合ベースの判定）
// 履歴スコアが上位10%以内でない場合は除外
if history_score < max_history * 9 / 10 {
    // TTエントリが十分良い場合は例外
    if tt_entry.value < beta + 100 {
        return None;  // SE適用しない
    }
}
```

#### 3.3.2 ゲームフェーズ別調整
```rust
// フェーズごとの設定（ローカルコピーまたはbuilderパターンを使用）
let params = match game_phase {
    GamePhase::Opening => {
        // 序盤は控えめに
        SingularExtensionParams {
            min_depth: base_params.min_depth + 2,
            double_extension_enabled: false,  // 序盤は無効
            ..base_params
        }
    }
    GamePhase::EndGame => {
        // 終盤は積極的に
        SingularExtensionParams {
            min_depth: base_params.min_depth - 1,
            double_extension_enabled: true,   // 終盤のみ有効化を検討
            ..base_params
        }
    }
    _ => base_params.clone()
};

// マージンもフェーズに応じて調整
let margin = match game_phase {
    GamePhase::Opening => SINGULAR_MARGIN_BASE + depth * SINGULAR_MARGIN_SLOPE * 3 / 2,
    GamePhase::EndGame => SINGULAR_MARGIN_BASE / 2 + depth * SINGULAR_MARGIN_SLOPE,
    _ => calculate_singular_margin(depth)
};
```

## 4. テストと検証

### 4.1 単体テスト例
```rust
#[test]
fn test_singular_extension_detection() {
    let pos = Position::from_sfen("...").unwrap();
    let mut searcher = Searcher::new();
    
    // TTに良い手を登録
    searcher.tt.store(
        pos.hash(),
        Move::from_str("7g7f").unwrap(),
        1000,  // 高い評価値 (centipawns)
        500,
        10,    // 十分な深度
        Bound::Lower
    );
    
    // 実際にsearch()を呼び出し、拡張が適用されたかを検証
    let depth = 12;
    let alpha = -1000;
    let beta = 1000;
    
    // 探索前のノード数を記録
    let nodes_before = searcher.nodes_searched;
    
    // 探索実行
    let _ = searcher.search(&pos, alpha, beta, depth);
    
    // ノード統計から拡張が適用されたことを確認
    let extension_nodes = searcher.extension_stats.singular_extensions;
    assert!(extension_nodes > 0, "Singular extension should have been applied");
}
```

### 4.2 性能測定
- **Elo向上**: Shogiでは+10-30 Elo程度（Chessより分岐数が多いため効果は控えめ）
- **探索深度**: 平均0.5-1手深くなる
- **時間増加**: Cut-node限定で深度>10plyなら5-15%程度（序盤や浅い局面では増加率が大きくなる可能性）

## 5. 実装チェックリスト

- [ ] 基本的なSE判定ロジック
- [ ] excluded_moveの管理（スレッドセーフな実装）
- [ ] TTエントリの適切な読み取り
- [ ] 他の手がβを超える場合の処理
- [ ] 二重延長の実装（デフォルト無効、LMRとの併用順序に注意）
- [ ] 履歴ヒューリスティックとの統合
- [ ] ゲームフェーズ別調整
- [ ] パフォーマンステスト
- [ ] Elo測定

## 6. 参考文献

1. Stockfish源码分析 - Singular Extension
2. "Singular Extensions Revisited" - ICGA Journal
3. Chess Programming Wiki - Singular Extensions

## 7. トラブルシューティング

### 7.1 探索爆発
- min_depthを増やす
- margin_baseを大きくする
- PVノードのみに制限する

### 7.2 効果が見られない
- TTの精度を確認
- 履歴テーブルの更新を確認
- パラメータの再調整

### 7.3 時間制御の問題
- 時間管理モジュールとの連携確認
- 緊急時のSE無効化機能の実装

## 8. 補足

### 8.1 評価値の単位
このドキュメントでは評価値の単位としてcentipawn（歩100点）を前提としています。
Shogiエンジンでは1駒=1000点相当の実装も多いため、マージン値の調整時は評価値スケールに注意してください。

### 8.2 排他移動（excluded_move）の実装
再帰呼び出し間でexcluded_moveを管理する場合、スレッドセーフな実装が必要です。
ローカル変数として引数で渡す方が安全です。

### 8.3 LMRとの併用
Double extensionとLate-Move Reductionsを併用する場合は、先にLMRを適用してから
extension調整を行うのが一般的です。これにより副作用を最小限に抑えることができます。
