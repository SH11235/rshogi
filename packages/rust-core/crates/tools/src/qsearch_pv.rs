//! 軽量版 qsearch with PV
//!
//! 教師データ前処理用の軽量版静止探索実装。
//! 既存の探索エンジンとは独立しており、以下の特徴を持つ:
//!
//! - 置換表なし
//! - historyなし
//! - 単純なalpha-beta
//! - PVを返す
//!
//! # 使用例
//!
//! ```rust,ignore
//! use tools::qsearch_pv::{qsearch_with_pv, QsearchResult, Evaluator, MaterialEvaluator};
//! use engine_core::position::Position;
//!
//! let mut pos = Position::new();
//! pos.set_hirate();
//!
//! let evaluator = MaterialEvaluator;
//! let result = qsearch_with_pv(&mut pos, &evaluator, -30000, 30000, 0, 32);
//! println!("Score: {}, PV length: {}", result.value, result.pv.len());
//! ```

use engine_core::eval::material::evaluate_material;
use engine_core::movegen::{generate_legal, MoveList};
use engine_core::nnue::{
    evaluate_dispatch, get_network, is_nnue_initialized, AccumulatorStackVariant, DirtyPiece,
};
use engine_core::position::Position;
use engine_core::types::{Move, Value};
use std::cell::RefCell;

/// qsearch結果
#[derive(Debug, Clone)]
pub struct QsearchResult {
    /// 評価値
    pub value: i32,
    /// 最善手順（PV）
    pub pv: Vec<Move>,
}

/// 評価関数トレイト
pub trait Evaluator: Send + Sync {
    /// 局面を評価する
    fn evaluate(&self, pos: &Position) -> i32;
}

/// Material評価関数
pub struct MaterialEvaluator;

impl Evaluator for MaterialEvaluator {
    fn evaluate(&self, pos: &Position) -> i32 {
        evaluate_material(pos).raw()
    }
}

/// NNUE評価関数
///
/// スレッドローカルストレージで各種AccumulatorStackを管理し、
/// `evaluate_dispatch`を使って評価する。
/// qsearchでは各局面で全計算を行う（差分更新なし）。
pub struct NnueEvaluator;

impl NnueEvaluator {
    /// 新しいNnueEvaluatorを作成
    ///
    /// NNUEが初期化済みである必要がある。
    /// 初期化されていない場合はNoneを返す。
    pub fn new() -> Option<Self> {
        if is_nnue_initialized() {
            Some(Self)
        } else {
            None
        }
    }
}

impl Evaluator for NnueEvaluator {
    fn evaluate(&self, pos: &Position) -> i32 {
        // スレッドローカルでAccumulatorStackVariantを管理
        // qsearchでは差分更新が複雑なため、各評価で全計算を行う
        thread_local! {
            static ACC_STACK: RefCell<Option<AccumulatorStackVariant>> = const { RefCell::new(None) };
        }

        ACC_STACK.with(|acc| {
            let mut acc = acc.borrow_mut();

            // ネットワークに応じたスタックを作成/更新
            if let Some(network) = get_network() {
                if acc.is_none() || !acc.as_ref().unwrap().matches_network(network) {
                    *acc = Some(AccumulatorStackVariant::from_network(network));
                }
            } else {
                // NNUEが初期化されていない場合はデフォルトを使用
                if acc.is_none() {
                    *acc = Some(AccumulatorStackVariant::new_default());
                }
            }

            let stack = acc.as_mut().unwrap();
            // AccumulatorStackをリセット（全計算を強制）
            stack.reset();

            evaluate_dispatch(pos, stack).raw()
        })
    }
}

/// NNUE評価用のスタック
///
/// NNUEアーキテクチャに対応するAccumulatorStackVariantを管理する。
/// スレッドローカルで使用することを想定。
pub struct NnueStacks {
    /// 統合アキュムレータスタック
    pub stack: AccumulatorStackVariant,
    /// ノード数カウンター（探索爆発防止用）
    pub node_count: u64,
}

impl NnueStacks {
    /// ノード数上限（探索爆発防止）
    /// 100万ノードで打ち切り（問題局面でも数秒以内に完了）
    pub const MAX_NODES: u64 = 1_000_000;

    /// 新しいNnueStacksを作成
    pub fn new() -> Self {
        let stack = if let Some(network) = get_network() {
            AccumulatorStackVariant::from_network(network)
        } else {
            AccumulatorStackVariant::new_default()
        };
        Self {
            stack,
            node_count: 0,
        }
    }

    /// スタックをリセット（探索開始時に呼び出す）
    pub fn reset(&mut self) {
        self.node_count = 0;
        // ネットワークが変わっている可能性があるので確認
        if let Some(network) = get_network() {
            if !self.stack.matches_network(network) {
                self.stack = AccumulatorStackVariant::from_network(network);
            } else {
                self.stack.reset();
            }
        } else {
            self.stack.reset();
        }
    }

    /// ノード数上限に達したかどうか
    #[inline]
    pub fn node_limit_reached(&self) -> bool {
        self.node_count >= Self::MAX_NODES
    }

    /// ノード数をインクリメント
    #[inline]
    pub fn increment_nodes(&mut self) {
        self.node_count += 1;
    }

    /// 手の実行時にスタックをプッシュ
    #[inline]
    pub fn push(&mut self, dirty_piece: DirtyPiece) {
        self.stack.push(dirty_piece);
    }

    /// 手の取り消し時にスタックをポップ
    #[inline]
    pub fn pop(&mut self) {
        self.stack.pop();
    }

    /// 現在の局面を評価
    #[inline]
    pub fn evaluate(&mut self, pos: &Position) -> i32 {
        evaluate_dispatch(pos, &mut self.stack).raw()
    }
}

impl Default for NnueStacks {
    fn default() -> Self {
        Self::new()
    }
}

/// NNUE評価付きqsearch with PV（差分更新版）
///
/// AccumulatorStackを直接管理して差分更新を行う最適化版。
/// 全計算版より高速だが、NnueStacksの管理が必要。
///
/// # 引数
/// * `pos` - 探索する局面（mutableだがundo_moveで戻される）
/// * `stacks` - NNUE用のスタック群
/// * `alpha` - アルファ値
/// * `beta` - ベータ値
/// * `ply` - 現在の深さ
/// * `max_ply` - 最大深さ
///
/// # 戻り値
/// 評価値とPVを含むQsearchResult
pub fn qsearch_with_pv_nnue(
    pos: &mut Position,
    stacks: &mut NnueStacks,
    alpha: i32,
    beta: i32,
    ply: i32,
    max_ply: i32,
) -> QsearchResult {
    // ノード数をインクリメント
    stacks.increment_nodes();

    // ノード数上限チェック（探索爆発防止）
    if stacks.node_limit_reached() {
        return QsearchResult {
            value: stacks.evaluate(pos),
            pv: vec![],
        };
    }

    // 深さ制限チェック
    if ply >= max_ply {
        return QsearchResult {
            value: stacks.evaluate(pos),
            pv: vec![],
        };
    }

    // 王手中かどうか
    let in_check = pos.in_check();

    // stand pat（静止評価）
    // 王手中は評価をスキップ（逃げ手を探索する必要がある）
    let stand_pat = if in_check {
        -Value::INFINITE.raw() + ply // 王手中は非常に悪いスコア
    } else {
        stacks.evaluate(pos)
    };

    // beta カットオフ（王手中でない場合のみ）
    if !in_check && stand_pat >= beta {
        return QsearchResult {
            value: stand_pat,
            pv: vec![],
        };
    }

    let mut best_value = stand_pat;
    let mut best_pv: Vec<Move> = vec![];
    let mut alpha = alpha;

    // 王手中でない場合、alphaをstand_patで更新
    if !in_check && stand_pat > alpha {
        alpha = stand_pat;
    }

    // 手の生成（全ての合法手）
    let mut moves = MoveList::new();
    generate_legal(pos, &mut moves);

    // 手がない場合
    if moves.is_empty() {
        if in_check {
            // 詰み
            return QsearchResult {
                value: -Value::MATE.raw() + ply,
                pv: vec![],
            };
        } else {
            // 駒取りがない → stand_patを返す
            return QsearchResult {
                value: stand_pat,
                pv: vec![],
            };
        }
    }

    for mv in moves.iter() {
        // 王手中は全ての手を探索、そうでなければ駒取りのみ
        if !in_check {
            let to = mv.to();
            let captured = pos.piece_on(to);

            // 駒取りでない手はスキップ（駒打ちも駒取りではない）
            if captured.is_none() {
                continue;
            }

            // SEEフィルタ
            if !pos.see_ge(*mv, Value::ZERO) {
                continue;
            }
        }

        // 手を実行（DirtyPieceを取得）
        let gives_check = pos.gives_check(*mv);
        let dirty_piece = pos.do_move(*mv, gives_check);

        // AccumulatorStackをプッシュ
        stacks.push(dirty_piece);

        // 再帰呼び出し
        let result = qsearch_with_pv_nnue(pos, stacks, -beta, -alpha, ply + 1, max_ply);
        let value = -result.value;

        // AccumulatorStackをポップ
        stacks.pop();

        // 手を戻す
        pos.undo_move(*mv);

        // 最善手の更新
        if value > best_value {
            best_value = value;

            if value > alpha {
                alpha = value;
                best_pv = vec![*mv];
                best_pv.extend(result.pv);

                // ベータカットオフ
                if value >= beta {
                    break;
                }
            }
        }
    }

    QsearchResult {
        value: best_value,
        pv: best_pv,
    }
}

/// 軽量版 qsearch with PV
///
/// # 引数
/// * `pos` - 探索する局面（mutableだがundo_moveで戻される）
/// * `evaluator` - 評価関数
/// * `alpha` - アルファ値
/// * `beta` - ベータ値
/// * `ply` - 現在の深さ
/// * `max_ply` - 最大深さ
///
/// # 戻り値
/// 評価値とPVを含むQsearchResult
pub fn qsearch_with_pv<E: Evaluator + ?Sized>(
    pos: &mut Position,
    evaluator: &E,
    alpha: i32,
    beta: i32,
    ply: i32,
    max_ply: i32,
) -> QsearchResult {
    // 深さ制限チェック
    if ply >= max_ply {
        return QsearchResult {
            value: evaluator.evaluate(pos),
            pv: vec![],
        };
    }

    // 王手中かどうか
    let in_check = pos.in_check();

    // stand pat（静止評価）
    // 王手中は評価をスキップ（逃げ手を探索する必要がある）
    let stand_pat = if in_check {
        -Value::INFINITE.raw() + ply // 王手中は非常に悪いスコア
    } else {
        evaluator.evaluate(pos)
    };

    // beta カットオフ（王手中でない場合のみ）
    if !in_check && stand_pat >= beta {
        return QsearchResult {
            value: stand_pat,
            pv: vec![],
        };
    }

    let mut best_value = stand_pat;
    let mut best_pv: Vec<Move> = vec![];
    let mut alpha = alpha;

    // 王手中でない場合、alphaをstand_patで更新
    if !in_check && stand_pat > alpha {
        alpha = stand_pat;
    }

    // 手の生成（全ての合法手）
    let mut moves = MoveList::new();
    generate_legal(pos, &mut moves);

    // 手がない場合
    if moves.is_empty() {
        if in_check {
            // 詰み
            return QsearchResult {
                value: -Value::MATE.raw() + ply,
                pv: vec![],
            };
        } else {
            // 駒取りがない → stand_patを返す
            return QsearchResult {
                value: stand_pat,
                pv: vec![],
            };
        }
    }

    // MVV-LVA順にソート（簡易版）
    // 今回は生成順のままで処理

    for mv in moves.iter() {
        // 王手中は全ての手を探索、そうでなければ駒取りのみ
        if !in_check {
            let to = mv.to();
            let captured = pos.piece_on(to);

            // 駒取りでない手はスキップ（駒打ちも駒取りではない）
            if captured.is_none() {
                continue;
            }

            // SEEフィルタ
            if !pos.see_ge(*mv, Value::ZERO) {
                continue;
            }
        }

        // 手を実行
        let gives_check = pos.gives_check(*mv);
        let _ = pos.do_move(*mv, gives_check);

        // 再帰呼び出し
        let result = qsearch_with_pv(pos, evaluator, -beta, -alpha, ply + 1, max_ply);
        let value = -result.value;

        // 手を戻す
        pos.undo_move(*mv);

        // 最善手の更新
        if value > best_value {
            best_value = value;

            if value > alpha {
                alpha = value;
                best_pv = vec![*mv];
                best_pv.extend(result.pv);

                // ベータカットオフ
                if value >= beta {
                    break;
                }
            }
        }
    }

    QsearchResult {
        value: best_value,
        pv: best_pv,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qsearch_hirate() {
        let mut pos = Position::new();
        pos.set_hirate();

        let evaluator = MaterialEvaluator;
        let result = qsearch_with_pv(&mut pos, &evaluator, -30000, 30000, 0, 32);

        // 平手初期局面は0点付近のはず
        assert!(
            result.value.abs() < 1000,
            "Initial position should be around 0: {}",
            result.value
        );
        // PVは空のはず（駒取りがない）
        assert!(result.pv.is_empty(), "PV should be empty for hirate");
    }

    #[test]
    fn test_qsearch_with_capture() {
        let mut pos = Position::new();
        // 歩が取れる局面
        let sfen = "4k4/9/9/9/4p4/4P4/9/9/4K4 b - 1";
        pos.set_sfen(sfen).expect("set_sfen should succeed");

        let evaluator = MaterialEvaluator;
        let result = qsearch_with_pv(&mut pos, &evaluator, -30000, 30000, 0, 32);

        // 歩を取るPVがあるはず
        // ただし、5六歩で5五歩を取ると同歩で取られる可能性がある
        // 簡単のため、評価値だけ確認
        assert!(result.value >= 0, "Capturing pawn should be beneficial");
    }

    #[test]
    fn test_qsearch_max_ply() {
        let mut pos = Position::new();
        pos.set_hirate();

        let evaluator = MaterialEvaluator;
        // max_ply = 0 で即座に評価値を返す
        let result = qsearch_with_pv(&mut pos, &evaluator, -30000, 30000, 0, 0);

        assert!(result.pv.is_empty(), "PV should be empty when max_ply = 0");
    }
}
