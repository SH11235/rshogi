//! HalfKA 特徴量
//!
//! King + All pieces (non-mirror)
//!
//! 主な特徴:
//! - キング位置: 81マス直指定（ミラーなし）
//! - 入力次元: 138,510 (81×1710)
//!
//! 注意: nnue-pytorchのcoalesce済みモデル専用。
//! Factorizationの重みはBase側に畳み込み済みのため、推論時はBaseのみで計算する。

use super::{Feature, TriggerEvent, BOARD_PIECE_TYPES};
use crate::nnue::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use crate::nnue::bona_piece::PIECE_BASE;
use crate::nnue::bona_piece_halfka::{halfka_index, king_bonapiece, king_index, BonaPieceHalfKA};
use crate::nnue::constants::HALFKA_DIMENSIONS;
use crate::position::Position;
use crate::types::{Color, PieceType, Square};

/// HalfKA 特徴量
///
/// キング位置は81マス直指定（ミラーなし）。
/// 自玉が動いた場合にアキュムレータの全計算が必要になる。
pub struct HalfKA;

impl Feature for HalfKA {
    /// 特徴量の次元数: 81×1710 = 138,510
    const DIMENSIONS: usize = HALFKA_DIMENSIONS;

    /// 同時にアクティブになる最大数（合法局面での理論上限）
    ///
    /// 将棋の合法局面では駒の総数は40個固定:
    /// - 盤上駒（王含む）+ 手駒 = 40
    ///
    /// coalesce済みモデルでは各駒が1特徴量なので MAX_ACTIVE = 40。
    ///
    /// 注意: この値は理論上限。実際のIndexListは`MAX_ACTIVE_FEATURES = 54`を使用し、
    /// テスト用の非合法局面（駒数超過）にも対応できる安全マージンを持つ。
    const MAX_ACTIVE: usize = 40;

    /// 自玉が動いた場合に全計算
    const REFRESH_TRIGGER: TriggerEvent = TriggerEvent::FriendKingMoved;

    /// アクティブな特徴量インデックスを追記
    ///
    /// 各駒について:
    /// Base特徴: king_index * PIECE_INPUTS + bonapiece
    #[inline]
    fn append_active_indices(
        pos: &Position,
        perspective: Color,
        active: &mut IndexList<MAX_ACTIVE_FEATURES>,
    ) {
        let king_sq = pos.king_square(perspective);
        let k_index = king_index(king_sq, perspective);

        // 盤上の駒（駒種・色ごとにループ）- 王以外
        for color in [Color::Black, Color::White] {
            let is_friend = (color == perspective) as usize;

            for &pt in &BOARD_PIECE_TYPES {
                let base = PIECE_BASE[pt as usize][is_friend];
                let bb = pos.pieces(color, pt);

                for sq in bb.iter() {
                    // 視点に応じてマスを変換
                    let sq_index = if perspective == Color::Black {
                        sq.index()
                    } else {
                        sq.inverse().index()
                    };

                    // BonaPieceを生成
                    let bp = BonaPieceHalfKA::new(base + sq_index as u16);

                    // Base特徴量（coalesce済みモデルではこれのみ）
                    // 合法局面では溢れないため戻り値を無視
                    let _ = active.push(halfka_index(k_index, bp.value() as usize));
                }
            }
        }

        // 両方の王の特徴量を追加
        // nnue-pytorchのtraining_data_loader.cppでは、EvalList内の全40駒
        // （両方の王を含む）を特徴量として使用している。
        // 自玉の位置はking_indexにも反映されるが、特徴量としても追加する。

        // 自玉の特徴量
        let friend_king_sq_index = if perspective == Color::Black {
            king_sq.index()
        } else {
            king_sq.inverse().index()
        };
        let friend_king_bp = king_bonapiece(friend_king_sq_index, true); // is_friend=true
                                                                         // 合法局面では溢れないため戻り値を無視
        let _ = active.push(halfka_index(k_index, friend_king_bp.value() as usize));

        // 敵玉の特徴量
        let enemy = perspective.opponent();
        let enemy_king_sq = pos.king_square(enemy);
        let enemy_king_sq_index = if perspective == Color::Black {
            enemy_king_sq.index()
        } else {
            enemy_king_sq.inverse().index()
        };
        let enemy_king_bp = king_bonapiece(enemy_king_sq_index, false); // is_friend=false
                                                                        // 合法局面では溢れないため戻り値を無視
        let _ = active.push(halfka_index(k_index, enemy_king_bp.value() as usize));

        // 手駒の特徴量
        // HalfKPと同様に、手駒の枚数分すべての特徴量を追加する
        // 例: 歩を3枚持っている場合、1枚目・2枚目・3枚目の特徴量をそれぞれ追加
        for owner in [Color::Black, Color::White] {
            for pt in [
                PieceType::Pawn,
                PieceType::Lance,
                PieceType::Knight,
                PieceType::Silver,
                PieceType::Gold,
                PieceType::Bishop,
                PieceType::Rook,
            ] {
                let count = pos.hand(owner).count(pt) as u8;
                if count == 0 {
                    continue;
                }
                // 手駒の枚数分、すべての特徴量を追加
                for i in 1..=count {
                    let bp = BonaPieceHalfKA::from_hand_piece(perspective, owner, pt, i);
                    if bp != BonaPieceHalfKA::ZERO {
                        // Base特徴量（coalesce済みモデルではこれのみ）
                        // 合法局面では溢れないため戻り値を無視
                        let _ = active.push(halfka_index(k_index, bp.value() as usize));
                    }
                }
            }
        }
    }

    /// 変化した特徴量インデックスを追記
    ///
    /// DirtyPieceから変化した特徴量を計算する。
    /// coalesce済みモデル専用のため、Base特徴量のみを処理。
    #[inline]
    fn append_changed_indices(
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
        removed: &mut IndexList<MAX_CHANGED_FEATURES>,
        added: &mut IndexList<MAX_CHANGED_FEATURES>,
    ) {
        let k_index = king_index(king_sq, perspective);

        // 盤上駒の変化を処理
        for dp in dirty_piece.pieces() {
            // 盤上から消える側（old）
            if dp.old_piece.is_some() {
                if let Some(sq) = dp.old_sq {
                    // 視点に応じてマスを変換
                    let sq_index = if perspective == Color::Black {
                        sq.index()
                    } else {
                        sq.inverse().index()
                    };

                    // 王の場合は king_bonapiece を使用
                    // (BonaPiece::from_piece_square は King に対して ZERO を返すため)
                    let bp = if dp.old_piece.piece_type() == PieceType::King {
                        let is_friend = dp.color == perspective;
                        king_bonapiece(sq_index, is_friend)
                    } else {
                        BonaPieceHalfKA::from_piece_square(dp.old_piece, sq, perspective)
                    };

                    if bp != BonaPieceHalfKA::ZERO {
                        // 合法局面では溢れないため戻り値を無視
                        let _ = removed.push(halfka_index(k_index, bp.value() as usize));
                    }
                }
            }

            // 盤上に現れる側（new）
            if dp.new_piece.is_some() {
                if let Some(sq) = dp.new_sq {
                    // 視点に応じてマスを変換
                    let sq_index = if perspective == Color::Black {
                        sq.index()
                    } else {
                        sq.inverse().index()
                    };

                    // 王の場合は king_bonapiece を使用
                    // (BonaPiece::from_piece_square は King に対して ZERO を返すため)
                    let bp = if dp.new_piece.piece_type() == PieceType::King {
                        let is_friend = dp.color == perspective;
                        king_bonapiece(sq_index, is_friend)
                    } else {
                        BonaPieceHalfKA::from_piece_square(dp.new_piece, sq, perspective)
                    };

                    if bp != BonaPieceHalfKA::ZERO {
                        // 合法局面では溢れないため戻り値を無視
                        let _ = added.push(halfka_index(k_index, bp.value() as usize));
                    }
                }
            }
        }

        // 手駒の変化を反映
        // HalfKAでは手駒の枚数分すべての特徴量がアクティブになる設計
        // 例: 歩3枚 → 歩1枚目、歩2枚目、歩3枚目の3つの特徴量がすべてアクティブ
        // したがって差分更新では:
        // - 増加時: 増えた分だけ追加（既存はそのまま維持）
        // - 減少時: 減った分だけ削除（残る分は維持）
        for hc in dirty_piece.hand_changes() {
            if hc.old_count < hc.new_count {
                // 枚数増加: old_count+1 から new_count までの特徴量を追加
                for i in (hc.old_count + 1)..=hc.new_count {
                    let bp =
                        BonaPieceHalfKA::from_hand_piece(perspective, hc.owner, hc.piece_type, i);
                    if bp != BonaPieceHalfKA::ZERO {
                        let _ = added.push(halfka_index(k_index, bp.value() as usize));
                    }
                }
            } else if hc.old_count > hc.new_count {
                // 枚数減少: new_count+1 から old_count までの特徴量を削除
                for i in (hc.new_count + 1)..=hc.old_count {
                    let bp =
                        BonaPieceHalfKA::from_hand_piece(perspective, hc.owner, hc.piece_type, i);
                    if bp != BonaPieceHalfKA::ZERO {
                        let _ = removed.push(halfka_index(k_index, bp.value() as usize));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_halfka_dimensions() {
        assert_eq!(HalfKA::DIMENSIONS, 138_510);
    }

    #[test]
    fn test_halfka_max_active() {
        assert_eq!(HalfKA::MAX_ACTIVE, 40);
    }

    #[test]
    fn test_halfka_refresh_trigger() {
        assert_eq!(HalfKA::REFRESH_TRIGGER, TriggerEvent::FriendKingMoved);
    }
}
