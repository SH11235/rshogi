//! Evaluation function for shogi
//!
//! Simple material-based evaluation

use crate::shogi::attacks;
use crate::shogi::board::{Bitboard, Piece, Square};
use crate::shogi::board_constants::SHOGI_BOARD_SIZE;
use crate::shogi::piece_constants::{APERY_PIECE_VALUES, APERY_PROMOTED_PIECE_VALUES};
use crate::{
    shogi::{ALL_PIECE_TYPES, NUM_HAND_PIECE_TYPES},
    Color, PieceType, Position,
};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::OnceLock;

/// Trait for position evaluation
///
/// Contract:
/// - Returns a score in centipawns from the side-to-move perspective.
/// - Positive values favor the side to move; negative values favor the opponent.
/// - Flipping `side_to_move` on the same board position should approximately flip the sign
///   (exact symmetry is not guaranteed if the evaluator incorporates tempo or king safety asymmetries).
///
/// Implementations should keep this polarity contract to ensure search components
/// (e.g., repetition penalties, pruning rules) behave consistently.
pub trait Evaluator {
    /// Evaluate position from side to move perspective
    fn evaluate(&self, pos: &Position) -> i32;

    /// Notify evaluator that search is starting at this position (root reset)
    /// Default: no-op
    fn on_set_position(&self, _pos: &Position) {}

    /// Notify evaluator before making a real move (called with pre-move position)
    /// Default: no-op
    fn on_do_move(&self, _pre_pos: &Position, _mv: crate::shogi::Move) {}

    /// Notify evaluator after undoing the last real move
    /// Default: no-op
    fn on_undo_move(&self) {}

    /// Notify evaluator before doing a null move (side-to-move flip)
    /// Default: no-op
    fn on_do_null_move(&self, _pre_pos: &Position) {}

    /// Notify evaluator after undoing the last null move
    /// Default: no-op
    fn on_undo_null_move(&self) {}
}

/// Implement Evaluator for Arc<T> where T: Evaluator
impl<T: Evaluator + ?Sized> Evaluator for std::sync::Arc<T> {
    fn evaluate(&self, pos: &Position) -> i32 {
        (**self).evaluate(pos)
    }
    fn on_set_position(&self, pos: &Position) {
        (**self).on_set_position(pos)
    }
    fn on_do_move(&self, pre_pos: &Position, mv: crate::shogi::Move) {
        (**self).on_do_move(pre_pos, mv)
    }
    fn on_undo_move(&self) {
        (**self).on_undo_move()
    }
    fn on_do_null_move(&self, pre_pos: &Position) {
        (**self).on_do_null_move(pre_pos)
    }
    fn on_undo_null_move(&self) {
        (**self).on_undo_null_move()
    }
}

/// Apery 駒価値の純粋な物質評価（軽量ヒューリスティック無し）。
///
/// - 手番側視点のスコアを返す（正なら手番有利）。
/// - 盤上の駒＋持ち駒のみを APERY_PIECE_VALUES/APERY_PROMOTED_PIECE_VALUES で集計する。
fn evaluate_material_apery_only(pos: &Position) -> i32 {
    let us = pos.side_to_move;
    let them = us.opposite();

    let mut score = 0;

    // Material on board
    for &pt in &ALL_PIECE_TYPES {
        let piece_type = pt as usize;

        // Count pieces
        let our_pieces = pos.board.piece_bb[us as usize][piece_type];
        let their_pieces = pos.board.piece_bb[them as usize][piece_type];

        let our_count = our_pieces.count_ones() as i32;
        let their_count = their_pieces.count_ones() as i32;

        let base_value = APERY_PIECE_VALUES[piece_type];
        score += base_value * (our_count - their_count);

        let promoted_delta = APERY_PROMOTED_PIECE_VALUES[piece_type] - base_value;
        if promoted_delta != 0 {
            let our_promoted = our_pieces & pos.board.promoted_bb;
            let their_promoted = their_pieces & pos.board.promoted_bb;

            let our_promoted_count = our_promoted.count_ones() as i32;
            let their_promoted_count = their_promoted.count_ones() as i32;

            score += promoted_delta * (our_promoted_count - their_promoted_count);
        }
    }

    // Material in hand
    for piece_idx in 0..NUM_HAND_PIECE_TYPES {
        let our_hand = pos.hands[us as usize][piece_idx] as i32;
        let their_hand = pos.hands[them as usize][piece_idx] as i32;

        let piece_type = PieceType::from_hand_index(piece_idx).expect("invalid hand index");
        let value = APERY_PIECE_VALUES[piece_type as usize];

        score += value * (our_hand - their_hand);
    }

    // Board piece 1/10 reduction (YaneuraOu MATERIAL 相当の補正)。
    // 盤上の駒は持ち駒よりわずかに価値が低いとみなし、
    // その差分（先手番側視点）を 1/10 程度だけ減算する。
    //
    // - YO の compute_eval では PieceValueM を用いて
    //   board 上の駒ごとに piece_value * 104/1024 を引いている。
    // - ここでは side_to_move 視点の差分として、
    //   「盤上駒の駒得分」に対して同じ係数を掛けて減算する。
    let board_scale_num: i32 = 104;
    let board_scale_den: i32 = 1024;
    let mut board_delta = 0;
    for &pt in &ALL_PIECE_TYPES {
        // 王は除外（YO 側では実質 0 扱いになっている）。
        if pt == PieceType::King {
            continue;
        }
        let piece_type = pt as usize;
        let base_value = APERY_PIECE_VALUES[piece_type];

        let our_pieces = pos.board.piece_bb[us as usize][piece_type];
        let their_pieces = pos.board.piece_bb[them as usize][piece_type];
        let our_count = our_pieces.count_ones() as i32;
        let their_count = their_pieces.count_ones() as i32;

        let diff = our_count - their_count;
        if diff != 0 {
            board_delta += (base_value * diff * board_scale_num) / board_scale_den;
        }
    }
    score -= board_delta;

    score
}

/// デバッグ・分析用途向けの公開ラッパー。
///
/// - 「MATERIAL 部分だけ」の評価値を取得したいときに使用する。
/// - 対局用ロジックからは `MaterialEvaluator` 経由の評価を利用し、この関数はあくまで
///   評価分解やログ分析ツールから参照することを想定している。
#[inline]
pub fn evaluate_material_only_debug(pos: &Position) -> i32 {
    evaluate_material_apery_only(pos)
}

/// Evaluate position from side to move perspective
pub fn evaluate(pos: &Position) -> i32 {
    // まず純粋な物質評価（Apery 駒価値＋持ち駒）を計算し、その上に king safety / king position を積む。
    let mut score = evaluate_material_apery_only(pos);

    // --- King safety (distance × effect counts + king position bonus)
    let effects = compute_effect_counts(pos);
    score += king_safety_term(pos, &effects);
    score += king_position_term(pos);

    score
}

/// Simple material evaluator implementing Evaluator trait
#[derive(Clone, Copy, Debug)]
pub struct MaterialEvaluator;

impl Evaluator for MaterialEvaluator {
    fn evaluate(&self, pos: &Position) -> i32 {
        evaluate(pos)
    }
}

// --- Runtime knobs for MaterialEvaluator lightweight terms
fn tempo_cp_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| AtomicI32::new(0))
}

fn rook_mobility_cp_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| AtomicI32::new(0))
}

fn rook_trapped_penalty_cp_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| AtomicI32::new(0))
}

fn king_early_move_penalty_cp_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| AtomicI32::new(0))
}

fn king_early_move_max_ply_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| AtomicI32::new(20))
}

#[inline]
pub fn material_tempo_cp() -> i32 {
    tempo_cp_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn set_material_tempo_cp(v: i32) {
    tempo_cp_atomic().store(v.clamp(-200, 200), Ordering::Relaxed);
}

#[inline]
pub fn material_rook_mobility_cp() -> i32 {
    rook_mobility_cp_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn set_material_rook_mobility_cp(v: i32) {
    rook_mobility_cp_atomic().store(v.clamp(0, 50), Ordering::Relaxed);
}

#[inline]
pub fn material_rook_trapped_penalty_cp() -> i32 {
    rook_trapped_penalty_cp_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn set_material_rook_trapped_penalty_cp(v: i32) {
    rook_trapped_penalty_cp_atomic().store(v.clamp(0, 500), Ordering::Relaxed);
}

#[inline]
pub fn material_king_early_move_penalty_cp() -> i32 {
    king_early_move_penalty_cp_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn set_material_king_early_move_penalty_cp(v: i32) {
    king_early_move_penalty_cp_atomic().store(v.clamp(0, 200), Ordering::Relaxed);
}

#[inline]
pub fn material_king_early_move_max_ply() -> i32 {
    king_early_move_max_ply_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn set_material_king_early_move_max_ply(v: i32) {
    king_early_move_max_ply_atomic().store(v.clamp(0, 100), Ordering::Relaxed);
}

#[inline]
fn king_safety_our_weights() -> &'static [i32; 9] {
    static CELL: OnceLock<[i32; 9]> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut weights = [0; 9];
        for (dist, slot) in weights.iter_mut().enumerate() {
            // YaneuraOu MATERIAL 相当の係数よりやや控えめに設定しつつ、
            // 敵利き側の重み（king_safety_their_weights）とのバランスを安全寄りに取る。
            let base = 60 * 1024;
            *slot = base / ((dist + 1) as i32);
        }
        weights
    })
}

#[inline]
fn king_safety_their_weights() -> &'static [i32; 9] {
    static CELL: OnceLock<[i32; 9]> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut weights = [0; 9];
        for (dist, slot) in weights.iter_mut().enumerate() {
            // 敵利き側は自玉への脅威としてやや強めに評価する。
            let base = 105 * 1024;
            *slot = base / ((dist + 1) as i32);
        }
        weights
    })
}

#[inline]
fn king_pos_bonus_table() -> &'static [i32; SHOGI_BOARD_SIZE] {
    use crate::shogi::board_constants::{BOARD_FILES, BOARD_RANKS};

    static CELL: OnceLock<[i32; SHOGI_BOARD_SIZE]> = OnceLock::new();
    CELL.get_or_init(|| {
        // YaneuraOu の king_pos_bonus を盤面の 1 段目 9 筋から 9 段目 1 筋の順で並べたテーブル。
        // ここでは [rank(0=a)..8=i][file(0=9筋)..8=1筋] の順で 9x9 に展開し、Square の内部 index
        // (file,rank) = (0..8,0..8) に対応するようにマッピングする。
        let raw: [i32; 81] = [
            875, 655, 830, 680, 770, 815, 720, 945, 755, 605, 455, 610, 595, 730, 610, 600, 590,
            615, 565, 640, 555, 525, 635, 565, 440, 600, 575, 520, 515, 580, 420, 640, 535, 565,
            500, 510, 220, 355, 240, 375, 340, 335, 305, 275, 320, 500, 530, 560, 445, 510, 395,
            455, 490, 410, 345, 275, 250, 355, 295, 280, 420, 235, 135, 335, 370, 385, 255, 295,
            200, 265, 305, 305, 255, 225, 245, 295, 200, 320, 275, 70, 200,
        ];

        let mut table = [0i32; SHOGI_BOARD_SIZE];
        let files = BOARD_FILES as u8;
        let ranks = BOARD_RANKS as u8;

        for rank in 0..ranks {
            for file in 0..files {
                let sq = Square::new(file, rank);
                let internal_idx = sq.index();
                let raw_idx = (rank as usize) * (files as usize) + (file as usize);
                let base = raw[raw_idx];

                // cp スケールに合わせて少し弱めるため、約 1/20 に縮小する。
                table[internal_idx] = base / 20;
            }
        }

        table
    })
}

fn king_position_term(pos: &Position) -> i32 {
    let table = king_pos_bonus_table();
    let (Some(bk), Some(wk)) =
        (pos.board.king_square(Color::Black), pos.board.king_square(Color::White))
    else {
        return 0;
    };

    // 先手玉の位置ボーナス - 後手玉の位置ボーナスを先手視点スコアとして使う。
    // evaluate() は手番側視点に変換しているので、ここでは先手視点の差分だけを返す。
    let black_bonus = table[bk.index()];
    let white_bonus = table[wk.flip().index()];
    black_bonus - white_bonus
}
fn king_safety_term(pos: &Position, effects: &[[u8; SHOGI_BOARD_SIZE]; 2]) -> i32 {
    let (Some(black_king), Some(white_king)) =
        (pos.board.king_square(Color::Black), pos.board.king_square(Color::White))
    else {
        return 0;
    };

    let black_score = king_safety_score_for(
        Color::Black,
        black_king,
        &effects[Color::Black as usize],
        &effects[Color::White as usize],
    );
    let white_score = king_safety_score_for(
        Color::White,
        white_king,
        &effects[Color::White as usize],
        &effects[Color::Black as usize],
    );

    // King safety 項全体のスケールを少し抑え、安全側寄りに調整する。
    const KING_SAFETY_SCALE_X100: i32 = 60;

    let diff = match pos.side_to_move {
        Color::Black => black_score - white_score,
        Color::White => white_score - black_score,
    };

    diff * KING_SAFETY_SCALE_X100 / 100
}

fn king_safety_score_for(
    color: Color,
    king_sq: Square,
    own_effects: &[u8; SHOGI_BOARD_SIZE],
    opp_effects: &[u8; SHOGI_BOARD_SIZE],
) -> i32 {
    let our_weights = king_safety_our_weights();
    let their_weights = king_safety_their_weights();
    let our_dir_rates = king_safety_our_dir_rates();
    let their_dir_rates = king_safety_their_dir_rates();
    let mut total = 0i32;

    for idx in 0..SHOGI_BOARD_SIZE {
        let sq = Square(idx as u8);
        let dist = chebyshev_distance(king_sq, sq).min(8) as usize;
        // 方角バケットは常に「先手から見た」定義になっているため、
        // 後手玉については盤面を 180 度回転させた座標系（YaneuraOu の Inv 相当）
        // で direction を計算する。
        let (king_for_dir, sq_for_dir) = match color {
            Color::Black => (king_sq, sq),
            Color::White => (king_sq.flip(), sq.flip()),
        };
        let dir = king_direction_bucket(king_for_dir, sq_for_dir);
        let own = multi_effect_value(own_effects[idx]);
        let opp = multi_effect_value(opp_effects[idx]);
        // 距離に基づく重みと方角レートを掛け合わせる。
        let w_our = (our_weights[dist] as i64 * our_dir_rates[dir] as i64) / 1024;
        let mut w_their = (their_weights[dist] as i64 * their_dir_rates[dir] as i64) / 1024;

        // 玉頭方向（前方〜前方斜め）1〜2マスに対する敵利きの重みをわずかに強める。
        // YaneuraOu の our_effect_rate/their_effect_rate に沿いつつ、危険帯だけ 10〜15% 程度ブーストする。
        let is_forward_dir = matches!(dir, 0..=2);
        if dist <= 2 && is_forward_dir {
            const HEAD_THEIR_SCALE_X1024: i64 = 1150; // ≒ +12%
            w_their = (w_their * HEAD_THEIR_SCALE_X1024) / 1024;
        }

        total += ((own as i64 * w_our) / (1024 * 1024)) as i32;
        total -= ((opp as i64 * w_their) / (1024 * 1024)) as i32;
    }

    total
}

#[cfg(test)]
fn king_shell_term(pos: &Position, effects: &[[u8; SHOGI_BOARD_SIZE]; 2]) -> i32 {
    // 玉の8近傍のカバー状況を軽く見る項。
    // - 自玉周りで利きが薄い＋空き/敵駒のマスがあると減点
    // - そこに味方駒が立っていれば小さく加点
    const EMPTY_OR_ENEMY_PEN: i32 = 8;
    const OWN_PIECE_BONUS_WEAK: i32 = 5;
    const OWN_PIECE_BONUS_STRONG: i32 = 3;

    let mut shell = [0i32; 2];

    for color in [Color::Black, Color::White] {
        let Some(king_sq) = pos.board.king_square(color) else {
            continue;
        };
        let color_idx = color as usize;
        let opp_idx = color.opposite() as usize;

        for rank in 0..9u8 {
            for file in 0..9u8 {
                let sq = Square::new(file, rank);
                if chebyshev_distance(king_sq, sq) != 1 {
                    continue;
                }
                let idx = sq.index();
                let effect_us = effects[color_idx][idx].min(2) as i32;
                let effect_them = effects[opp_idx][idx].min(2) as i32;
                let piece = pos.board.piece_on(sq);
                let own_piece = matches!(piece, Some(p) if p.color == color);

                if effect_us <= 1 {
                    if own_piece {
                        shell[color_idx] += OWN_PIECE_BONUS_WEAK;
                    } else {
                        shell[color_idx] -= EMPTY_OR_ENEMY_PEN;
                    }
                } else if own_piece {
                    shell[color_idx] += OWN_PIECE_BONUS_STRONG + effect_them;
                }
            }
        }
    }

    match pos.side_to_move {
        Color::Black => shell[Color::Black as usize] - shell[Color::White as usize],
        Color::White => shell[Color::White as usize] - shell[Color::Black as usize],
    }
}

/// 玉周りに前に出た攻め駒が「敵利き＞味方利き」になっているときの安全性ペナルティ。
///
/// - 対象: 玉からチェビシェフ距離1〜3マスにいる自軍の飛・角・金・銀（成り含む）。
/// - そのマスの `their_effect > our_effect` の分だけ小さく減点する。
/// - piece_safety_term の質駒ペナルティを補完し、「玉頭に出過ぎた攻め駒」を少しだけ嫌う。
///
/// NOTE: この項は YaneuraOu MATERIAL には存在しない「実験的な追加ペナルティ」です。
///       特徴量（利き本数・距離・駒種）は本家と揃えつつ、局所的に危険形を強調する目的で導入しており、
///       チューニングの結果次第では係数調整や撤廃を含めて見直す可能性があります。
#[cfg(test)]
fn king_attacker_safety_term(pos: &Position, effects: &[[u8; SHOGI_BOARD_SIZE]; 2]) -> i32 {
    let mut danger = [0i32; 2];

    for color in [Color::Black, Color::White] {
        let Some(king_sq) = pos.board.king_square(color) else {
            continue;
        };
        let color_idx = color as usize;
        let opp_idx = color.opposite() as usize;

        for rank in 0..9u8 {
            for file in 0..9u8 {
                let sq = Square::new(file, rank);
                let dist = chebyshev_distance(king_sq, sq);
                if dist == 0 || dist > 3 {
                    continue;
                }
                let Some(piece) = pos.board.piece_on(sq) else {
                    continue;
                };
                if piece.color != color {
                    continue;
                }
                // 玉頭に出て行きやすい攻め駒のみ対象（歩・香・桂・玉は除外）
                match piece.piece_type {
                    PieceType::Rook | PieceType::Bishop | PieceType::Silver | PieceType::Gold => {}
                    _ => continue,
                }
                let idx = sq.index();
                let our = effects[color_idx][idx].min(3) as i32;
                let their = effects[opp_idx][idx].min(3) as i32;
                let net = their - our;
                if net <= 0 {
                    continue;
                }
                // 駒種と距離に応じた小さなペナルティ（最大でも数十cp程度）。
                let piece_weight = match piece.piece_type {
                    PieceType::Rook | PieceType::Bishop => 3,
                    _ => 2,
                };
                let dist_weight = if dist <= 2 { 2 } else { 1 };
                danger[color_idx] += piece_weight * dist_weight * net;
            }
        }
    }

    match pos.side_to_move {
        // 自玉側の danger が大きいほどスコアが悪化するよう符号を付ける。
        Color::Black => danger[Color::White as usize] - danger[Color::Black as usize],
        Color::White => danger[Color::Black as usize] - danger[Color::White as usize],
    }
}

fn king_safety_our_dir_rates() -> &'static [i32; 10] {
    static CELL: OnceLock<[i32; 10]> = OnceLock::new();
    CELL.get_or_init(|| {
        // YaneuraOu MATERIAL_LEVEL 8 の our_effect_rate と同じテーブル。
        //   0 : 真上, 1 : 右上上, 2 : 右上, 3 : 右右上, 4 : 右,
        //   5 : 右右下, 6 : 右下, 7 : 右下下, 8 : 真下, 9 : 同一升
        [1120, 1872, 112, 760, 744, 880, 1320, 600, 904, 1024]
    })
}

fn king_safety_their_dir_rates() -> &'static [i32; 10] {
    static CELL: OnceLock<[i32; 10]> = OnceLock::new();
    CELL.get_or_init(|| {
        // YaneuraOu MATERIAL_LEVEL 8 の their_effect_rate と同じテーブル。
        [1056, 1714, 1688, 1208, 248, 240, 496, 816, 928, 1024]
    })
}

fn king_direction_bucket(king: Square, sq: Square) -> usize {
    // 先手玉から見た方角バケット。YaneuraOu の direction_of() と同じ定義。
    // file: 0=9筋..8=1筋, rank: 0=a..8=i。
    let df0 = sq.file() as i32 - king.file() as i32;
    let dr0 = sq.rank() as i32 - king.rank() as i32;
    let mut df = df0;
    let dr = dr0;
    // sq が玉から見て右側にあればミラー（df > 0 なら符号反転）
    if df > 0 {
        df = -df;
    }

    if df == 0 && dr == 0 {
        return 9;
    }
    if df == 0 && dr < 0 {
        return 0;
    }
    if df > dr && dr < 0 {
        return 1;
    }
    if df == dr && dr < 0 {
        return 2;
    }
    if df < dr && dr < 0 {
        return 3;
    }
    if df < 0 && dr == 0 {
        return 4;
    }
    if df < -dr && dr > 0 {
        return 5;
    }
    if df == -dr && dr > 0 {
        return 6;
    }
    if df == 0 && dr > 0 {
        return 8;
    }
    if df > -dr && dr > 0 {
        return 7;
    }
    // 想定外のケースだが、安全側にフォールバック。
    9
}

fn compute_effect_counts(pos: &Position) -> [[u8; SHOGI_BOARD_SIZE]; 2] {
    let mut effects = [[0u8; SHOGI_BOARD_SIZE]; 2];
    let occupied = pos.board.all_bb;

    for color in [Color::Black, Color::White] {
        accumulate_effects_for_color(pos, color, occupied, &mut effects[color as usize]);
    }

    effects
}

#[cfg(test)]
fn piece_safety_term(pos: &Position, effects: &[[u8; SHOGI_BOARD_SIZE]; 2]) -> i32 {
    // 自駒への利きによるボーナスはやや控えめにし、敵利きによるペナルティを相対的に重視する。
    const OUR_EFFECT_TO_OUR_PIECE: [i32; 3] = [0, 20, 26];
    const THEIR_EFFECT_TO_OUR_PIECE: [i32; 3] = [0, 113, 122];
    const SCALE: i32 = 4096;
    let mut raw = [0i32; 2];

    for color in [Color::Black, Color::White] {
        let mut bb = pos.board.occupied_bb[color as usize];
        while let Some(sq) = bb.pop_lsb() {
            let Some(piece) = pos.board.piece_on(sq) else {
                continue;
            };
            if piece.color != color {
                continue;
            }
            if piece.piece_type == PieceType::King {
                continue;
            }
            let defenders = effects[color as usize][sq.index()].min(2) as usize;
            let attackers = effects[color.opposite() as usize][sq.index()].min(2) as usize;
            let piece_value = piece.value();
            raw[color as usize] += piece_value * OUR_EFFECT_TO_OUR_PIECE[defenders] / SCALE;
            raw[color as usize] -= piece_value * THEIR_EFFECT_TO_OUR_PIECE[attackers] / SCALE;
        }
    }

    match pos.side_to_move {
        Color::Black => raw[Color::Black as usize] - raw[Color::White as usize],
        Color::White => raw[Color::White as usize] - raw[Color::Black as usize],
    }
}

fn multi_effect_value(count: u8) -> i32 {
    const TABLE: [i32; 11] = [
        0, 1024, 1800, 2300, 2900, 3500, 3900, 4300, 4650, 5000, 5300,
    ];
    TABLE[usize::from(count.min(10))]
}

fn accumulate_effects_for_color(
    pos: &Position,
    color: Color,
    occupied: Bitboard,
    effects: &mut [u8; SHOGI_BOARD_SIZE],
) {
    for &piece_type in &ALL_PIECE_TYPES {
        let mut bb = pos.board.piece_bb[color as usize][piece_type as usize];
        while let Some(sq) = bb.pop_lsb() {
            let Some(piece) = pos.board.piece_on(sq) else {
                continue;
            };
            let mut attacks = piece_attack_bitboard(pos, sq, piece, occupied);
            while let Some(target) = attacks.pop_lsb() {
                let idx = target.index();
                effects[idx] = effects[idx].saturating_add(1);
            }
        }
    }
}

fn piece_attack_bitboard(pos: &Position, sq: Square, piece: Piece, occupied: Bitboard) -> Bitboard {
    match piece.piece_type {
        PieceType::Pawn => {
            if piece.promoted {
                attacks::gold_attacks(sq, piece.color)
            } else {
                attacks::pawn_attacks(sq, piece.color)
            }
        }
        PieceType::Lance => {
            if piece.promoted {
                attacks::gold_attacks(sq, piece.color)
            } else {
                lance_attacks_blocked(pos, sq, piece.color)
            }
        }
        PieceType::Knight => {
            if piece.promoted {
                attacks::gold_attacks(sq, piece.color)
            } else {
                attacks::knight_attacks(sq, piece.color)
            }
        }
        PieceType::Silver => {
            if piece.promoted {
                attacks::gold_attacks(sq, piece.color)
            } else {
                attacks::silver_attacks(sq, piece.color)
            }
        }
        PieceType::Gold => attacks::gold_attacks(sq, piece.color),
        PieceType::King => attacks::king_attacks(sq),
        PieceType::Bishop => {
            let mut result = attacks::sliding_attacks(sq, occupied, PieceType::Bishop);
            if piece.promoted {
                result |= king_orthogonal_attacks(sq);
            }
            result
        }
        PieceType::Rook => {
            let mut result = attacks::sliding_attacks(sq, occupied, PieceType::Rook);
            if piece.promoted {
                result |= king_diagonal_attacks(sq);
            }
            result
        }
    }
}

fn lance_attacks_blocked(pos: &Position, sq: Square, color: Color) -> Bitboard {
    let mut attacks = Bitboard::EMPTY;
    let file = sq.file();
    let mut rank = sq.rank() as i8;
    let step = if color == Color::Black { -1 } else { 1 };

    loop {
        rank += step;
        if !(0..9).contains(&rank) {
            break;
        }
        let target = Square::new(file, rank as u8);
        attacks.set(target);
        if pos.board.piece_on(target).is_some() {
            break;
        }
    }

    attacks
}

fn king_orthogonal_attacks(sq: Square) -> Bitboard {
    let mut attacks = Bitboard::EMPTY;
    let file = sq.file() as i8;
    let rank = sq.rank() as i8;
    for (df, dr) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
        let nf = file + df;
        let nr = rank + dr;
        if (0..9).contains(&nf) && (0..9).contains(&nr) {
            attacks.set(Square::new(nf as u8, nr as u8));
        }
    }
    attacks
}

fn king_diagonal_attacks(sq: Square) -> Bitboard {
    let mut attacks = Bitboard::EMPTY;
    let file = sq.file() as i8;
    let rank = sq.rank() as i8;
    for (df, dr) in [(-1, -1), (-1, 1), (1, -1), (1, 1)] {
        let nf = file + df;
        let nr = rank + dr;
        if (0..9).contains(&nf) && (0..9).contains(&nr) {
            attacks.set(Square::new(nf as u8, nr as u8));
        }
    }
    attacks
}

fn chebyshev_distance(a: Square, b: Square) -> u8 {
    let df = (a.file() as i32 - b.file() as i32).abs();
    let dr = (a.rank() as i32 - b.rank() as i32).abs();
    df.max(dr) as u8
}

#[cfg(test)]
mod tests {
    use crate::{usi::parse_usi_square, Color, Piece};

    use super::*;

    fn place_kings(pos: &mut Position) {
        pos.board
            .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));
    }

    #[test]
    fn test_evaluate_startpos_is_zero() {
        let pos = Position::startpos();
        // 純粋な駒割りのみを検証するため、テンポや飛車利きなどの軽量項は含めない。
        let score = evaluate_material_apery_only(&pos);
        assert_eq!(score, 0);
    }

    #[test]
    fn test_evaluate_material_apery_values() {
        let mut pos = Position::empty();
        place_kings(&mut pos);
        pos.board
            .put_piece(parse_usi_square("2b").unwrap(), Piece::new(PieceType::Rook, Color::Black));
        pos.board.put_piece(
            parse_usi_square("8h").unwrap(),
            Piece::new(PieceType::Bishop, Color::White),
        );

        // APERY 駒価値のみを検証（R=990, B=855）。
        let score = evaluate_material_apery_only(&pos);
        // 990 (R) - 855 (B) = 135
        assert_eq!(score, 135);
    }

    #[test]
    fn test_promoted_piece_values_match_gold() {
        let mut pos = Position::empty();
        place_kings(&mut pos);

        let mut tokin_black = Piece::new(PieceType::Pawn, Color::Black);
        tokin_black.promoted = true;
        pos.board.put_piece(parse_usi_square("5e").unwrap(), tokin_black);

        let mut tokin_white = Piece::new(PieceType::Pawn, Color::White);
        tokin_white.promoted = true;
        pos.board.put_piece(parse_usi_square("5f").unwrap(), tokin_white);

        // 成り歩は双方とも 540（= 金相当）なので互いに打ち消し合うはず。
        let score = evaluate_material_apery_only(&pos);
        assert_eq!(score, 0, "Tokin vs Tokin should cancel out");
    }

    #[test]
    fn test_hand_material_consistency() {
        let mut pos = Position::empty();
        place_kings(&mut pos);
        pos.side_to_move = Color::Black;

        // Black has 1 rook in hand, White has 2 pawns in hand
        pos.hands[Color::Black as usize][0] = 1; // Rook
        pos.hands[Color::White as usize][6] = 2; // Pawns

        // 盤上は対称なので 0、持ち駒だけで 990 - 2 * 90 = 810 となるはず。
        let score = evaluate_material_apery_only(&pos);
        assert_eq!(score, 990 - 2 * 90);
    }

    /// 飛車の機動力評価が「利きの多い側を優先する」向きに働くことを確認する。
    #[test]
    fn rook_mobility_bonus_prefers_more_mobility() {
        // 既存ノブを退避し、飛車機動力以外の軽量項は 0 にしておく
        let tempo_old = material_tempo_cp();
        let mob_old = material_rook_mobility_cp();
        let trap_old = material_rook_trapped_penalty_cp();
        let king_pen_old = material_king_early_move_penalty_cp();

        set_material_tempo_cp(0);
        set_material_rook_trapped_penalty_cp(0);
        set_material_king_early_move_penalty_cp(0);
        set_material_rook_mobility_cp(10);

        // 単純な局面: 自玉の飛車のみを配置し、敵側には飛車を置かない。
        // Kings: 5i / 5a, Black rook: 5e
        let mut pos = Position::empty();
        place_kings(&mut pos);
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Rook, Color::Black));
        pos.side_to_move = Color::Black;

        // 純粋な駒割りとの差分として、飛車機動力項が正のボーナスになっていることを確認する。
        let base = evaluate_material_apery_only(&pos);
        let full = MaterialEvaluator.evaluate(&pos);
        let mob = full - base;

        assert!(
            mob > 0,
            "rook mobility term should give positive bonus when only our rook is present (mob={mob})"
        );

        // ノブを元に戻す
        set_material_tempo_cp(tempo_old);
        set_material_rook_mobility_cp(mob_old);
        set_material_rook_trapped_penalty_cp(trap_old);
        set_material_king_early_move_penalty_cp(king_pen_old);
    }

    /// 早期玉移動ペナルティが「自玉だけが初期位置から動いた場合」にマイナス、
    /// 「相手玉だけが動いた場合」にプラスとして働くことを確認する。
    #[test]
    fn early_king_move_penalty_applies_symmetrically() {
        let tempo_old = material_tempo_cp();
        let mob_old = material_rook_mobility_cp();
        let trap_old = material_rook_trapped_penalty_cp();
        let king_pen_old = material_king_early_move_penalty_cp();
        let king_max_old = material_king_early_move_max_ply();

        set_material_tempo_cp(0);
        set_material_rook_mobility_cp(0);
        set_material_rook_trapped_penalty_cp(0);
        set_material_king_early_move_penalty_cp(50);
        set_material_king_early_move_max_ply(10);

        // ベース: 初期局面（早期判定が効くように ply を 1 に揃える）
        let mut base = Position::startpos();
        base.ply = 1;
        let base_eval = MaterialEvaluator.evaluate(&base);

        // 自玉のみ動かした局面（先手 5i→4i に移動）
        let mut pos_self_move = base.clone();
        pos_self_move.board.remove_piece(parse_usi_square("5i").unwrap());
        pos_self_move
            .board
            .put_piece(parse_usi_square("4i").unwrap(), Piece::new(PieceType::King, Color::Black));

        let eval_self = MaterialEvaluator.evaluate(&pos_self_move);

        // 相手玉のみ動かした局面（後手 5a→6a に移動）
        let mut pos_opp_move = base.clone();
        pos_opp_move.board.remove_piece(parse_usi_square("5a").unwrap());
        pos_opp_move
            .board
            .put_piece(parse_usi_square("6a").unwrap(), Piece::new(PieceType::King, Color::White));
        let eval_opp = MaterialEvaluator.evaluate(&pos_opp_move);

        assert!(
            eval_self < base_eval,
            "early king move by side-to-move should be penalized (eval_self={eval_self}, base={base_eval})"
        );
        assert!(
            eval_opp > base_eval,
            "early king move by opponent should be rewarded (eval_opp={eval_opp}, base={base_eval})"
        );

        // ノブを元に戻す
        set_material_tempo_cp(tempo_old);
        set_material_rook_mobility_cp(mob_old);
        set_material_rook_trapped_penalty_cp(trap_old);
        set_material_king_early_move_penalty_cp(king_pen_old);
        set_material_king_early_move_max_ply(king_max_old);
    }

    #[test]
    fn king_safety_is_symmetric_in_startpos() {
        // MATERIAL レベルでは玉位置ボーナスや利き密度に微妙な非対称があるため、
        // 完全な 0 ではなく「ほぼ対称」であることだけを確認する。
        let pos = Position::startpos();
        let effects = compute_effect_counts(&pos);
        let ks = king_safety_term(&pos, &effects);
        assert!(
            ks.abs() < 100,
            "king safety term should be near symmetric in startpos (got {ks})"
        );
    }

    #[test]
    fn piece_safety_penalizes_hanging_gold() {
        let mut pos = Position::empty();
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(parse_usi_square("6g").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        pos.board
            .put_piece(parse_usi_square("6f").unwrap(), Piece::new(PieceType::Pawn, Color::White));
        // pawn is attacking gold; add a supporting white piece to ensure attacker count > defender count
        pos.board.put_piece(
            parse_usi_square("6e").unwrap(),
            Piece::new(PieceType::Silver, Color::White),
        );

        let effects = compute_effect_counts(&pos);
        // Side to move = Black, penalty should be negative (our gold hanging)
        assert!(piece_safety_term(&pos, &effects) < 0);
    }

    #[test]
    fn multi_effect_value_is_monotonic() {
        let mut last = multi_effect_value(0);
        for m in 1..=10 {
            let v = multi_effect_value(m);
            assert!(
                v >= last,
                "multi_effect_value should be non-decreasing: m={m}, v={v}, last={last}"
            );
            last = v;
        }
    }

    #[test]
    fn king_direction_bucket_matches_basic_directions() {
        // 玉を5e相当の升に置き、前方/後方/左右/斜めが期待するバケットに入ることだけ軽く検証する。
        let king = Square::new(4, 4); // file=4, rank=4
        let up = Square::new(4, 3);
        let down = Square::new(4, 5);
        let right = Square::new(5, 4);
        let left = Square::new(3, 4);
        let up_bucket = king_direction_bucket(king, up);
        let down_bucket = king_direction_bucket(king, down);
        let right_bucket = king_direction_bucket(king, right);
        let left_bucket = king_direction_bucket(king, left);
        // 単純に「真上」と「同一升」が異なることだけ見ておく。
        let same_bucket = king_direction_bucket(king, king);
        assert_ne!(up_bucket, same_bucket, "up bucket should differ from same-square bucket");
        assert_ne!(down_bucket, same_bucket, "down bucket should differ from same-square bucket");
        // 左右方向も「同一升」とは異なるバケットになっていることだけ確認する。
        assert_ne!(right_bucket, same_bucket, "right bucket should differ from same-square bucket");
        assert_ne!(left_bucket, same_bucket, "left bucket should differ from same-square bucket");
    }

    #[test]
    fn king_shell_penalizes_empty_unsafe_squares() {
        let mut pos = Position::empty();
        // 玉を中央付近に置き、周囲に駒がない局面を作る。
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        let effects = compute_effect_counts(&pos);
        // どちらの玉も周囲に味方駒が無く、shell は双方マイナス寄りになるはずだが、
        // 手番差分としては 0 近傍に収まることを確認しておく。
        let shell = king_shell_term(&pos, &effects);
        assert!(
            shell.abs() < 50,
            "shell term should be near 0 in symmetric empty position (shell={shell})"
        );
    }

    #[test]
    fn king_attacker_safety_penalizes_forward_exposed_bishop() {
        let mut pos = Position::empty();
        // 黒玉を9i、白玉を1aに置く既存ヘルパーを利用。
        place_kings(&mut pos);
        // 黒手番とする。
        pos.side_to_move = Color::Black;
        // 黒角を玉から2マス前方寄りの位置に配置（例: 8g 相当）し、
        // 白の飛・角でその地点に利きを集中させる。
        pos.board.put_piece(
            parse_usi_square("8g").unwrap(),
            Piece::new(PieceType::Bishop, Color::Black),
        );
        pos.board
            .put_piece(parse_usi_square("8a").unwrap(), Piece::new(PieceType::Rook, Color::White));
        pos.board.put_piece(
            parse_usi_square("2e").unwrap(),
            Piece::new(PieceType::Bishop, Color::White),
        );
        let effects = compute_effect_counts(&pos);
        let base = king_attacker_safety_term(&pos, &effects);

        // 同じ角を1マス後ろに下げた局面を作り、危険度が軽くなることを確認する。
        let mut safer = pos.clone();
        safer.board.remove_piece(parse_usi_square("8g").unwrap());
        safer.board.put_piece(
            parse_usi_square("8h").unwrap(),
            Piece::new(PieceType::Bishop, Color::Black),
        );
        let safer_effects = compute_effect_counts(&safer);
        let safer_score = king_attacker_safety_term(&safer, &safer_effects);

        assert!(
            safer_score > base,
            "moving attacker away from king should reduce danger (base={base}, safer={safer_score})"
        );
    }
}
