//! NNUE 差分更新用のヘルパ
//!
//! `StateInfo::dirty_piece` に基づいて、HalfKP の active index の増減を計算する。

use super::bona_piece::{halfkp_index, BonaPiece};
use crate::position::Position;
use crate::types::{Color, Piece};

/// 差分更新用: 変化した特徴量を取得
///
/// - 戻り値:
///   - removed: 1→0 になった特徴量（削除）
///   - added:   0→1 になった特徴量（追加）
///
/// 玉が動いた場合や判定ができない場合は（removed, added）とも空を返し、
/// 呼び出し側で全計算にフォールバックする前提とする。
pub fn get_changed_features(pos: &Position, perspective: Color) -> (Vec<usize>, Vec<usize>) {
    let state = pos.state();
    if pos.previous_state().is_none() {
        // 前の局面が無い（初期状態など）
        return (Vec::new(), Vec::new());
    }

    // 玉が動いた場合は全計算が必要（HalfKP は自玉位置×駒配置のため）
    if state.dirty_piece.king_moved[perspective.index()] {
        return (Vec::new(), Vec::new());
    };

    let king_sq = pos.king_square(perspective);
    let mut removed = Vec::new();
    let mut added = Vec::new();

    for dp in &state.dirty_piece.pieces {
        // 盤上から消える側（old）
        if dp.old_piece != Piece::NONE {
            if let Some(sq) = dp.old_sq {
                let bp = BonaPiece::from_piece_square(dp.old_piece, sq, perspective);
                if bp != BonaPiece::ZERO {
                    removed.push(halfkp_index(king_sq, bp));
                }
            }
        }

        // 盤上に現れる側（new）
        if dp.new_piece != Piece::NONE {
            if let Some(sq) = dp.new_sq {
                let bp = BonaPiece::from_piece_square(dp.new_piece, sq, perspective);
                if bp != BonaPiece::ZERO {
                    added.push(halfkp_index(king_sq, bp));
                }
            }
        }
    }

    // 手駒の変化を反映
    for hc in &state.dirty_piece.hand_changes {
        // やねうら王同様、手駒は種類×枚数の組み合わせで 1 つの BonaPiece を表現する。
        if hc.old_count != hc.new_count {
            // 旧カウント分の特徴量を削除
            if hc.old_count > 0 {
                let bp_old =
                    BonaPiece::from_hand_piece(perspective, hc.owner, hc.piece_type, hc.old_count);
                if bp_old != BonaPiece::ZERO {
                    removed.push(halfkp_index(king_sq, bp_old));
                }
            }
            // 新カウント分の特徴量を追加
            if hc.new_count > 0 {
                let bp_new =
                    BonaPiece::from_hand_piece(perspective, hc.owner, hc.piece_type, hc.new_count);
                if bp_new != BonaPiece::ZERO {
                    added.push(halfkp_index(king_sq, bp_new));
                }
            }
        }
    }

    (removed, added)
}
