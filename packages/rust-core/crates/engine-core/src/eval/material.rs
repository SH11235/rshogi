use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::LazyLock;

use crate::position::{BoardEffects, Position};
use crate::types::{Color, Piece, PieceType, Square, Value};

/// Material評価の適用レベル（YaneuraOu MaterialLv に対応）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialLevel {
    Lv1,
    Lv2,
    Lv3,
    Lv4,
    Lv7,
    Lv8,
    Lv9,
}

impl MaterialLevel {
    /// レベル値から MaterialLevel を取得
    ///
    /// 注意: レベル5, 6は未実装（YaneuraOu互換性のため欠番）
    pub fn from_value(v: u8) -> Option<Self> {
        match v {
            1 => Some(MaterialLevel::Lv1),
            2 => Some(MaterialLevel::Lv2),
            3 => Some(MaterialLevel::Lv3),
            4 => Some(MaterialLevel::Lv4),
            7 => Some(MaterialLevel::Lv7),
            8 => Some(MaterialLevel::Lv8),
            9 => Some(MaterialLevel::Lv9),
            _ => None,
        }
    }

    /// レベル値を取得
    pub fn value(self) -> u8 {
        match self {
            MaterialLevel::Lv1 => 1,
            MaterialLevel::Lv2 => 2,
            MaterialLevel::Lv3 => 3,
            MaterialLevel::Lv4 => 4,
            MaterialLevel::Lv7 => 7,
            MaterialLevel::Lv8 => 8,
            MaterialLevel::Lv9 => 9,
        }
    }
}

/// デフォルトのMaterial評価レベル（YaneuraOu MaterialLv9 相当）
pub const DEFAULT_MATERIAL_LEVEL: MaterialLevel = MaterialLevel::Lv9;

/// ランタイムで切り替え可能なMaterial評価レベル
/// 値は MaterialLevel::value() の戻り値 (1, 2, 3, 4, 7, 8, 9)
///
/// 注意: Ordering::Relaxed を使用しているが、MaterialLevel は探索開始前
/// （USI isready / ベンチマーク開始時）に設定される想定のため問題ない。
/// 探索中に変更されることは想定していない。
static MATERIAL_LEVEL: AtomicU8 = AtomicU8::new(9);

/// 現在のMaterial評価レベルを取得
pub fn get_material_level() -> MaterialLevel {
    let v = MATERIAL_LEVEL.load(Ordering::Relaxed);
    debug_assert!(
        MaterialLevel::from_value(v).is_some(),
        "Invalid MaterialLevel value in AtomicU8: {v}"
    );
    MaterialLevel::from_value(v).unwrap_or(DEFAULT_MATERIAL_LEVEL)
}

/// Material評価レベルを設定
pub fn set_material_level(level: MaterialLevel) {
    MATERIAL_LEVEL.store(level.value(), Ordering::Relaxed);
}

/// Apery(WCSC26)準拠の駒価値
pub(crate) fn base_piece_value(pt: PieceType) -> i32 {
    match pt {
        PieceType::Pawn => 90,
        PieceType::Lance => 315,
        PieceType::Knight => 405,
        PieceType::Silver => 495,
        PieceType::Bishop => 855,
        PieceType::Rook => 990,
        PieceType::Gold => 540,
        PieceType::King => 15000,
        PieceType::ProPawn => 540,
        PieceType::ProLance => 540,
        PieceType::ProKnight => 540,
        PieceType::ProSilver => 540,
        PieceType::Horse => 945,
        PieceType::Dragon => 1395,
    }
}

#[inline]
pub(crate) fn signed_piece_value(pc: Piece) -> i32 {
    if pc.is_none() {
        return 0;
    }
    let sign = if pc.color() == Color::Black { 1 } else { -1 };
    sign * base_piece_value(pc.piece_type())
}

#[inline]
pub(crate) fn hand_piece_value(color: Color, pt: PieceType) -> i32 {
    let sign = if color == Color::Black { 1 } else { -1 };
    sign * base_piece_value(pt)
}

/// material_valueをフル再計算（StateInfoの再初期化用）
pub fn compute_material_value(pos: &Position) -> Value {
    let mut score = 0i32;

    for sq in pos.occupied().iter() {
        score += signed_piece_value(pos.piece_on(sq));
    }

    for color in [Color::Black, Color::White] {
        let hand = pos.hand(color);
        for pt in PieceType::HAND_PIECES {
            score += hand.count(pt) as i32 * hand_piece_value(color, pt);
        }
    }

    Value::new(score)
}

#[inline]
fn dist(a: Square, b: Square) -> usize {
    let df = (a.file().index() as i32 - b.file().index() as i32).unsigned_abs() as usize;
    let dr = (a.rank().index() as i32 - b.rank().index() as i32).unsigned_abs() as usize;
    df.max(dr)
}

/// 距離に応じた利き評価値テーブルを生成（base * 1024 / (distance+1)）
const fn make_effect_values(base: i32) -> [i32; 9] {
    let mut arr = [0; 9];
    let mut i = 0;
    while i < 9 {
        arr[i] = base * 1024 / (i as i32 + 1);
        i += 1;
    }
    arr
}

const LV3_OUR_EFFECT_VALUE: [i32; 9] = make_effect_values(68);
const LV3_THEIR_EFFECT_VALUE: [i32; 9] = make_effect_values(96);
const LV4_OUR_EFFECT_VALUE: [i32; 9] = make_effect_values(85);
const LV4_THEIR_EFFECT_VALUE: [i32; 9] = make_effect_values(98);
const LV7_OUR_EFFECT_VALUE: [i32; 9] = make_effect_values(83);
const LV7_THEIR_EFFECT_VALUE: [i32; 9] = make_effect_values(92);

/// 利きの多重度に応じた評価値テーブル（LazyLockによる遅延初期化）
static MULTI_EFFECT_VALUE: LazyLock<[i32; 11]> = LazyLock::new(|| {
    let mut arr = [0i32; 11];
    // YaneuraOu の optimizer が出力した近似式
    // 6365 - pow(0.8525, m-1) * 5341 (m=1..10)
    // 利きが増えるほど逓減しつつ上限に漸近する特性を再現するための定数
    for (m, value) in arr.iter_mut().enumerate().skip(1) {
        *value = (6365.0 - 0.8525f64.powi((m as i32) - 1) * 5341.0) as i32;
    }
    arr
});

struct Lv7Tables {
    our_effect_table: [[[i32; 3]; Square::NUM]; Square::NUM],
    their_effect_table: [[[i32; 3]; Square::NUM]; Square::NUM],
}

/// Lv7評価用のテーブル（LazyLockによる遅延初期化）
static LV7_TABLES: LazyLock<Lv7Tables> = LazyLock::new(|| {
    let mv = &*MULTI_EFFECT_VALUE;
    let mut our_effect_table = [[[0i32; 3]; Square::NUM]; Square::NUM];
    let mut their_effect_table = [[[0i32; 3]; Square::NUM]; Square::NUM];

    for king_sq in Square::all() {
        for sq in Square::all() {
            let d = dist(sq, king_sq);
            for m in 0..3 {
                our_effect_table[king_sq.index()][sq.index()][m] =
                    mv[m] * LV7_OUR_EFFECT_VALUE[d] / (1024 * 1024);
                their_effect_table[king_sq.index()][sq.index()][m] =
                    mv[m] * LV7_THEIR_EFFECT_VALUE[d] / (1024 * 1024);
            }
        }
    }

    Lv7Tables {
        our_effect_table,
        their_effect_table,
    }
});

// 自駒への味方/敵の利きが 0/1/2 のときの補正係数（MaterialLv7-9）
// 値は YaneuraOu から移植。後で 4096 で割って駒価値に掛ける。
const OUR_EFFECT_TO_OUR_PIECE: [i32; 3] = [0, 33, 43];
const THEIR_EFFECT_TO_OUR_PIECE: [i32; 3] = [0, 113, 122];

// 玉の位置ボーナス（先手視点の81升テーブル）
// インデックス: (8 - file) + rank * 9 （先手1段1筋が先頭、左から9筋→1筋の順）
// 高い値ほど玉にとって安全とみなす。後手は Inv(sq) でミラーし、符号を反転して用いる。
// 値は YaneuraOu MaterialLv8/9 から移植。
const KING_POS_BONUS: [i32; 81] = [
    875, 655, 830, 680, 770, 815, 720, 945, 755, 605, 455, 610, 595, 730, 610, 600, 590, 615, 565,
    640, 555, 525, 635, 565, 440, 600, 575, 520, 515, 580, 420, 640, 535, 565, 500, 510, 220, 355,
    240, 375, 340, 335, 305, 275, 320, 500, 530, 560, 445, 510, 395, 455, 490, 410, 345, 275, 250,
    355, 295, 280, 420, 235, 135, 335, 370, 385, 255, 295, 200, 265, 305, 305, 255, 225, 245, 295,
    200, 320, 275, 70, 200,
];

// 利き方向ごとの重み（direction_of の戻り値 0..9 に対応）
// 0:真上,1:右上上,2:右上,3:右右上,4:右,5:右右下,6:右下,7:右下下,8:真下,9:同じ升
const OUR_EFFECT_RATE: [i32; 10] = [1120, 1872, 112, 760, 744, 880, 1320, 600, 904, 1024];
const THEIR_EFFECT_RATE: [i32; 10] = [1056, 1714, 1688, 1208, 248, 240, 496, 816, 928, 1024];

fn king_pos_bonus(color: Color, sq: Square) -> i32 {
    // 後手側をミラーしてから参照する
    let target_sq = if color == Color::Black {
        sq
    } else {
        sq.inverse()
    };
    let idx = (8 - target_sq.file().index()) + target_sq.rank().index() * 9;
    let bonus = KING_POS_BONUS[idx];
    if color == Color::Black {
        bonus
    } else {
        -bonus
    }
}

fn direction_of(king: Square, sq: Square) -> usize {
    let mut df = sq.file().index() as i32 - king.file().index() as i32;
    let dr = sq.rank().index() as i32 - king.rank().index() as i32;

    if df > 0 {
        df = -df;
    }

    // 返り値の意味（YaneuraOuのバケット順）
    // 0: 真上, 1: 右上上, 2: 右上, 3: 右右上, 4: 右
    // 5: 右右下, 6: 右下, 7: 右下下, 8: 真下, 9: 同じ升
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

    unreachable!("Unexpected direction calculation: df={df}, dr={dr}");
}

#[inline]
fn clamp_effect(count: u8, max: usize) -> usize {
    usize::min(count as usize, max)
}

fn eval_lv1(pos: &Position) -> i32 {
    pos.state().material_value.raw()
}

fn eval_lv2(pos: &Position) -> i32 {
    let mut score = pos.state().material_value.raw();
    for sq in pos.occupied().iter() {
        let pc = pos.piece_on(sq);
        score -= signed_piece_value(pc) * 104 / 1024;
    }
    score
}

fn eval_lv3(pos: &Position, effects: &BoardEffects) -> i32 {
    let mut score = pos.state().material_value.raw();
    let king_b = pos.king_square(Color::Black);
    let king_w = pos.king_square(Color::White);

    for sq in Square::all() {
        let e_b = effects.effect(Color::Black, sq) as i32;
        let e_w = effects.effect(Color::White, sq) as i32;

        let d_b = dist(sq, king_b);
        let d_w = dist(sq, king_w);

        let s_b = e_b * LV3_OUR_EFFECT_VALUE[d_b] / 1024 - e_w * LV3_THEIR_EFFECT_VALUE[d_b] / 1024;
        let s_w = e_w * LV3_OUR_EFFECT_VALUE[d_w] / 1024 - e_b * LV3_THEIR_EFFECT_VALUE[d_w] / 1024;

        score += s_b;
        score -= s_w;

        let pc = pos.piece_on(sq);
        if pc.is_some() {
            score -= signed_piece_value(pc) * 104 / 1024;
        }
    }

    score
}

fn eval_lv4(pos: &Position, effects: &BoardEffects) -> i32 {
    let mut score = pos.state().material_value.raw();
    let king_b = pos.king_square(Color::Black);
    let king_w = pos.king_square(Color::White);
    let mv = &*MULTI_EFFECT_VALUE;

    for sq in Square::all() {
        let e_b = clamp_effect(effects.effect(Color::Black, sq), 10);
        let e_w = clamp_effect(effects.effect(Color::White, sq), 10);

        let d_b = dist(sq, king_b);
        let d_w = dist(sq, king_w);

        let s_b = mv[e_b] * LV4_OUR_EFFECT_VALUE[d_b] / (1024 * 1024)
            - mv[e_w] * LV4_THEIR_EFFECT_VALUE[d_b] / (1024 * 1024);
        let s_w = mv[e_w] * LV4_OUR_EFFECT_VALUE[d_w] / (1024 * 1024)
            - mv[e_b] * LV4_THEIR_EFFECT_VALUE[d_w] / (1024 * 1024);

        score += s_b;
        score -= s_w;

        let pc = pos.piece_on(sq);
        if pc.is_some() {
            score -= signed_piece_value(pc) * 104 / 1024;
        }
    }

    score
}

fn eval_lv7(pos: &Position, effects: &BoardEffects) -> i32 {
    eval_lv7_like(pos, effects, false, false)
}

fn eval_lv8(pos: &Position, effects: &BoardEffects) -> i32 {
    eval_lv7_like(pos, effects, true, false)
}

fn eval_lv9(pos: &Position, effects: &BoardEffects) -> i32 {
    eval_lv7_like(pos, effects, true, true)
}

fn eval_lv7_like(
    pos: &Position,
    effects: &BoardEffects,
    use_king_bonus: bool,
    use_direction: bool,
) -> i32 {
    let mut score = pos.state().material_value.raw();
    let king_b = pos.king_square(Color::Black);
    let king_w = pos.king_square(Color::White);
    let inv_king_w = king_w.inverse();
    let tables = &*LV7_TABLES;

    for sq in Square::all() {
        let m1 = clamp_effect(effects.effect(Color::Black, sq), 2);
        let m2 = clamp_effect(effects.effect(Color::White, sq), 2);
        let pc = pos.piece_on(sq);
        let mut local = 0i32;

        if use_direction {
            let dir_b = direction_of(king_b, sq);
            let inv_sq = sq.inverse();
            let dir_w = direction_of(inv_king_w, inv_sq);

            local += tables.our_effect_table[king_b.index()][sq.index()][m1]
                * OUR_EFFECT_RATE[dir_b]
                / 1024;
            local -= tables.their_effect_table[king_b.index()][sq.index()][m2]
                * THEIR_EFFECT_RATE[dir_b]
                / 1024;
            local -= tables.our_effect_table[inv_king_w.index()][inv_sq.index()][m2]
                * OUR_EFFECT_RATE[dir_w]
                / 1024;
            local += tables.their_effect_table[inv_king_w.index()][inv_sq.index()][m1]
                * THEIR_EFFECT_RATE[dir_w]
                / 1024;
        } else {
            local += tables.our_effect_table[king_b.index()][sq.index()][m1];
            local -= tables.their_effect_table[king_b.index()][sq.index()][m2];

            let inv_sq = sq.inverse();
            local -= tables.our_effect_table[inv_king_w.index()][inv_sq.index()][m2];
            local += tables.their_effect_table[inv_king_w.index()][inv_sq.index()][m1];
        }

        // 玉の8近傍判定
        for color in [Color::Black, Color::White] {
            let king_sq = if color == Color::Black {
                king_b
            } else {
                king_w
            };
            if dist(sq, king_sq) == 1 {
                let effect_us = if color == Color::Black { m1 } else { m2 };
                let delta = if effect_us <= 1 {
                    if pc.is_none() || pc.color() != color {
                        11
                    } else {
                        -20
                    }
                } else if pc.is_none() || pc.color() != color {
                    0
                } else {
                    -11
                };
                local -= delta * if color == Color::Black { 1 } else { -1 };
            }
        }

        if pc.is_none() {
            // 何もない升
        } else if pc.piece_type() == PieceType::King {
            if use_king_bonus {
                local += king_pos_bonus(pc.color(), sq);
            }
        } else {
            let pv = signed_piece_value(pc);
            local -= pv * 104 / 1024;

            let effect_us = if pc.color() == Color::Black { m1 } else { m2 };
            let effect_them = if pc.color() == Color::Black { m2 } else { m1 };

            local += pv * OUR_EFFECT_TO_OUR_PIECE[effect_us] / 4096;
            local -= pv * THEIR_EFFECT_TO_OUR_PIECE[effect_them] / 4096;
        }

        score += local;
    }

    score
}

/// Material評価（NNUE未初期化時のフォールバック）
///
/// # Tournament Build
///
/// `tournament` フィーチャーが有効な場合、この関数は完全に削除される。
/// - バイナリサイズの削減
/// - NNUEが必須となり、未初期化の場合はパニック
///
/// # 通常ビルド
///
/// NNUEが初期化されていない場合の代替評価関数として使用される。
/// MaterialLevelに応じた評価を実行する。
///
/// # パフォーマンス特性
///
/// - Level 1-2: 駒の価値のみ（高速）
/// - Level 3-4: 利きの計算を含む（中速）
/// - Level 7-9: より複雑な評価（低速だがNNUEより高速）
#[cfg(not(feature = "tournament"))]
pub fn evaluate_material(pos: &Position) -> Value {
    let level = get_material_level();
    let raw = match level {
        MaterialLevel::Lv1 => eval_lv1(pos),
        MaterialLevel::Lv2 => eval_lv2(pos),
        MaterialLevel::Lv3 => {
            let effects = pos.board_effects();
            eval_lv3(pos, effects)
        }
        MaterialLevel::Lv4 => {
            let effects = pos.board_effects();
            eval_lv4(pos, effects)
        }
        MaterialLevel::Lv7 => {
            let effects = pos.board_effects();
            eval_lv7(pos, effects)
        }
        MaterialLevel::Lv8 => {
            let effects = pos.board_effects();
            eval_lv8(pos, effects)
        }
        MaterialLevel::Lv9 => {
            let effects = pos.board_effects();
            eval_lv9(pos, effects)
        }
    };

    if pos.side_to_move() == Color::Black {
        Value::new(raw)
    } else {
        Value::new(-raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::SFEN_HIRATE;

    #[test]
    #[cfg(not(feature = "tournament"))]
    fn test_material_eval_hirate() {
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        let value = evaluate_material(&pos);

        // 初期局面はほぼ互角（MaterialLvにより0から僅かにずれる場合がある）
        assert!(value.raw().abs() < 200);
    }

    #[test]
    fn test_material_level_value_roundtrip() {
        let levels = [
            MaterialLevel::Lv1,
            MaterialLevel::Lv2,
            MaterialLevel::Lv3,
            MaterialLevel::Lv4,
            MaterialLevel::Lv7,
            MaterialLevel::Lv8,
            MaterialLevel::Lv9,
        ];

        for level in levels {
            let value = level.value();
            let restored = MaterialLevel::from_value(value).unwrap();
            assert_eq!(level, restored);
        }
    }

    #[test]
    fn test_material_level_invalid_values() {
        // 欠番と範囲外の値
        assert!(MaterialLevel::from_value(0).is_none());
        assert!(MaterialLevel::from_value(5).is_none()); // 欠番
        assert!(MaterialLevel::from_value(6).is_none()); // 欠番
        assert!(MaterialLevel::from_value(10).is_none());
    }

    #[test]
    fn test_get_set_material_level() {
        let original = get_material_level();

        set_material_level(MaterialLevel::Lv1);
        assert_eq!(get_material_level(), MaterialLevel::Lv1);

        set_material_level(MaterialLevel::Lv9);
        assert_eq!(get_material_level(), MaterialLevel::Lv9);

        // 元に戻す
        set_material_level(original);
    }
}
