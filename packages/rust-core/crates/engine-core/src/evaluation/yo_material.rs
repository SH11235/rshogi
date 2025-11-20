//! Strict YaneuraOu MATERIAL_LEVEL 3 compatible evaluator (black-view + side-to-move flip).
//!
//! このモジュールは、YaneuraOu の `MATERIAL_LEVEL == 3` に対応する `compute_eval` を
//! 可能な範囲でそのまま Rust に移植したものです。拡張版 MaterialEvaluator とは別に、
//! 「教材用のベースライン」として数値互換性を検証するために用います。

use crate::evaluation::evaluate::evaluate_material_apery_only;
use crate::shogi::board_constants::SHOGI_BOARD_SIZE;
use crate::shogi::{Color, Position, Square};

/// Lv.3 用の距離別利き重み（our_effect_value / their_effect_value）。
///
/// YaneuraOu evaluate_material.cpp の `add_options()` に合わせる:
///   our = 68*1024/(d+1), their = 96*1024/(d+1)
fn lv3_our_effect_values() -> &'static [i32; 9] {
    use std::sync::OnceLock;
    static CELL: OnceLock<[i32; 9]> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut a = [0; 9];
        for (d, slot) in a.iter_mut().enumerate() {
            *slot = (68 * 1024) / ((d + 1) as i32);
        }
        a
    })
}

fn lv3_their_effect_values() -> &'static [i32; 9] {
    use std::sync::OnceLock;
    static CELL: OnceLock<[i32; 9]> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut a = [0; 9];
        for (d, slot) in a.iter_mut().enumerate() {
            *slot = (96 * 1024) / ((d + 1) as i32);
        }
        a
    })
}

/// チェビシェフ距離（YaneuraOu の dist() 相当, 0..8）。
fn chebyshev_distance(a: Square, b: Square) -> usize {
    let df = (a.file() as i32 - b.file() as i32).abs();
    let dr = (a.rank() as i32 - b.rank() as i32).abs();
    df.max(dr) as usize
}

/// YaneuraOu MATERIAL_LEVEL 3 の `compute_eval` に対応する評価値を返す。
///
/// - まずブラックビューで `score` を構成し、最後に side_to_move に応じて符号を反転する。
/// - 利きの本数は `compute_effect_counts` 相当の簡易版で再計算する。
pub fn evaluate_yo_material_lv3(pos: &Position) -> i32 {
    // ベースとなる material（Apery 駒価値＋盤上1/10減算）は既存の関数で計算し、
    // それを「手番側視点」から「先手視点」に変換してから使う。
    let side_to_move = pos.side_to_move;
    let material_stm = evaluate_material_apery_only(pos);
    let material_black = match side_to_move {
        Color::Black => material_stm,
        Color::White => -material_stm,
    };

    let mut score = material_black;

    // 全マスの利き本数を [color][square] で集計（YO の board_effect 相当）。
    let mut effects = [[0u8; SHOGI_BOARD_SIZE]; 2];
    let occupied = pos.board.all_bb;
    for color in [Color::Black, Color::White] {
        let color_idx = color as usize;
        for piece_type in crate::shogi::ALL_PIECE_TYPES {
            let mut bb = pos.board.piece_bb[color_idx][piece_type as usize];
            while let Some(sq) = bb.pop_lsb() {
                let piece = pos.board.piece_on(sq).expect("piece must exist");
                let attacks = crate::evaluation::evaluate::piece_attack_bitboard(
                    pos, sq, piece, occupied,
                );
                let mut atk = attacks;
                while let Some(t) = atk.pop_lsb() {
                    let idx = t.index();
                    effects[color_idx][idx] = effects[color_idx][idx].saturating_add(1);
                }
            }
        }
    }

    let our_ev = lv3_our_effect_values();
    let their_ev = lv3_their_effect_values();

    // 王位置（先手/後手）。
    let black_king = match pos.board.king_square(Color::Black) {
        Some(sq) => sq,
        None => return 0,
    };
    let white_king = match pos.board.king_square(Color::White) {
        Some(sq) => sq,
        None => return 0,
    };

    // 全マスについて king-safety を加算。
    for idx in 0..SHOGI_BOARD_SIZE {
        let sq = Square(idx as u8);
        let effects_black = effects[Color::Black as usize][idx] as i32;
        let effects_white = effects[Color::White as usize][idx] as i32;

        // color=BLACK/WHITE に対して YO と同じ符号付けで加算する。
        for &color in &[Color::Black, Color::White] {
            let king_sq = if color == Color::Black { black_king } else { white_king };
            let d = chebyshev_distance(sq, king_sq).min(8);
            let s1 = if color == Color::Black { effects_black } else { effects_white }
                * our_ev[d]
                / 1024;
            let s2 = if color == Color::Black { effects_white } else { effects_black }
                * their_ev[d]
                / 1024;
            score += if color == Color::Black { s1 - s2 } else { s2 - s1 };
        }
    }

    // 最終的なスコアは side_to_move 視点に変換。
    match side_to_move {
        Color::Black => score,
        Color::White => -score,
    }
}
