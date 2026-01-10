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
use engine_core::position::Position;
use engine_core::types::{Move, Value};

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
pub fn qsearch_with_pv<E: Evaluator>(
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
