//! 教師データの局面が将棋のルール上ありうるかの検査。

use rshogi_core::bitboard::{FILE_BB, RANK_BB};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, PieceType};

/// 局面のルール違反を検出し、最初に見つかった理由を (分類, 説明) で返す。
///
/// 玉の有無・駒数の上限・行き所のない駒・二歩・手番でない側の玉への王手を見る。
pub fn validate_position(pos: &Position) -> Option<(&'static str, String)> {
    // 1. 玉の存在チェック
    for color in [Color::Black, Color::White] {
        if pos.pieces_pt(PieceType::King) & pos.pieces_c(color) == Default::default() {
            let side = if color == Color::Black {
                "先手"
            } else {
                "後手"
            };
            return Some(("no_king", format!("{side}の玉がない")));
        }
    }

    // 2. 駒数超過チェック
    let piece_limits: &[(PieceType, Option<PieceType>, u32, &str)] = &[
        (PieceType::Pawn, Some(PieceType::ProPawn), 18, "歩"),
        (PieceType::Lance, Some(PieceType::ProLance), 4, "香"),
        (PieceType::Knight, Some(PieceType::ProKnight), 4, "桂"),
        (PieceType::Silver, Some(PieceType::ProSilver), 4, "銀"),
        (PieceType::Gold, None, 4, "金"),
        (PieceType::Bishop, Some(PieceType::Horse), 2, "角"),
        (PieceType::Rook, Some(PieceType::Dragon), 2, "飛"),
        (PieceType::King, None, 2, "玉"),
    ];

    for &(raw_pt, promoted_pt, max, name) in piece_limits {
        let mut total = pos.pieces_pt(raw_pt).count();
        if let Some(ppt) = promoted_pt {
            total += pos.pieces_pt(ppt).count();
        }
        if raw_pt != PieceType::King {
            for color in [Color::Black, Color::White] {
                total += pos.hand(color).count(raw_pt);
            }
        }
        if total > max {
            return Some(("piece_overflow", format!("{name}が{total}枚（上限{max}枚）")));
        }
    }

    // 3. 行き所のない駒チェック
    for color in [Color::Black, Color::White] {
        let color_bb = pos.pieces_c(color);
        let side = if color == Color::Black {
            "先手"
        } else {
            "後手"
        };

        let dead_rank1 = if color == Color::Black {
            RANK_BB[0]
        } else {
            RANK_BB[8]
        };

        if (pos.pieces_pt(PieceType::Pawn) & color_bb & dead_rank1).count() > 0 {
            return Some(("dead_piece", format!("{side}の歩が行き所のない段にある")));
        }
        if (pos.pieces_pt(PieceType::Lance) & color_bb & dead_rank1).count() > 0 {
            return Some(("dead_piece", format!("{side}の香が行き所のない段にある")));
        }

        let dead_rank12 = if color == Color::Black {
            RANK_BB[0] | RANK_BB[1]
        } else {
            RANK_BB[7] | RANK_BB[8]
        };

        if (pos.pieces_pt(PieceType::Knight) & color_bb & dead_rank12).count() > 0 {
            return Some(("dead_piece", format!("{side}の桂が行き所のない段にある")));
        }
    }

    // 4. 二歩チェック
    for color in [Color::Black, Color::White] {
        let pawns = pos.pieces_pt(PieceType::Pawn) & pos.pieces_c(color);
        let side = if color == Color::Black {
            "先手"
        } else {
            "後手"
        };

        for (file_idx, file_bb) in FILE_BB.iter().enumerate() {
            if (pawns & *file_bb).count() >= 2 {
                return Some(("double_pawn", format!("{side}の二歩（{}筋）", file_idx + 1)));
            }
        }
    }

    // 5. 相手の玉に王手がかかった状態
    let them = !pos.side_to_move();
    let their_king = pos.king_square(them);
    let checkers = pos.attackers_to_c(their_king, pos.side_to_move());
    if checkers.count() > 0 {
        return Some(("enemy_in_check", "手番でない側の玉に王手がかかっている".to_string()));
    }

    None
}
