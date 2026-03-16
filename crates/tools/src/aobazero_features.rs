//! AobaZero (dlshogi_aoba) 用の入力特徴量構築
//!
//! dlshogi_aoba のカスタム特徴量フォーマットに対応。
//! 標準 dlshogi とは以下の点で異なる:
//! - 歩の手駒上限: 18 (標準: 8)
//! - 手番プレーン追加
//! - 手数カテゴリ 8 プレーン追加
//! - features2 合計: 86ch (標準: 57ch)
//!
//! 参照: https://github.com/yssaya/dlshogi_aoba
//! 元コード: dlshogi_aoba/cppshogi/cppshogi.cpp `make_input_features()`

use rshogi_core::bitboard::{
    Bitboard, bishop_effect, dragon_effect, gold_effect, horse_effect, king_effect, knight_effect,
    lance_effect, pawn_effect, rook_effect, silver_effect,
};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, PieceType, Square};

// ============================================================
// Constants (dlshogi_aoba custom)
// ============================================================

const MAX_HPAWN_NUM: usize = 18; // dlshogi_aoba: 18 (標準 dlshogi: 8)
const MAX_HLANCE_NUM: usize = 4;
const MAX_HKNIGHT_NUM: usize = 4;
const MAX_HSILVER_NUM: usize = 4;
const MAX_HGOLD_NUM: usize = 4;
const MAX_HBISHOP_NUM: usize = 2;
const MAX_HROOK_NUM: usize = 2;

/// 手駒の枚数合計 (片方の色): 18+4+4+4+4+2+2 = 38
pub const MAX_PIECES_IN_HAND_SUM: usize = MAX_HPAWN_NUM
    + MAX_HLANCE_NUM
    + MAX_HKNIGHT_NUM
    + MAX_HSILVER_NUM
    + MAX_HGOLD_NUM
    + MAX_HBISHOP_NUM
    + MAX_HROOK_NUM;

/// 先後含めた手駒プレーン数: 38 * 2 = 76
const MAX_FEATURES2_HAND_NUM: usize = 2 * MAX_PIECES_IN_HAND_SUM;

const PIECETYPE_NUM: usize = 14; // Pawn(1) .. Dragon(14)
const MAX_ATTACK_NUM: usize = 3;

/// features1 チャンネル数 (片方の色): 14(配置) + 14(利き) + 3(利き数) = 31
pub const MAX_FEATURES1_NUM: usize = PIECETYPE_NUM + PIECETYPE_NUM + MAX_ATTACK_NUM;

/// features2 チャンネル数: 76(手駒) + 1(王手) + 1(手番) + 8(手数) = 86
pub const MAX_FEATURES2_NUM: usize = MAX_FEATURES2_HAND_NUM + 1 + 1 + 8;

const SQUARE_NUM: usize = 81;

/// features1 の 1 バッチ分のサイズ (2色 × 31ch × 81sq)
pub const FEATURES1_SIZE: usize = 2 * MAX_FEATURES1_NUM * SQUARE_NUM; // 5022
/// features2 の 1 バッチ分のサイズ (86ch × 81sq)
pub const FEATURES2_SIZE: usize = MAX_FEATURES2_NUM * SQUARE_NUM; // 6966

/// ONNX input1 チャンネル数: 2 × 31 = 62
pub const INPUT1_CHANNELS: usize = 2 * MAX_FEATURES1_NUM;
/// ONNX input2 チャンネル数: 86
pub const INPUT2_CHANNELS: usize = MAX_FEATURES2_NUM;

/// 手駒種別ごとのオフセット (features2 内)
const HAND_OFFSETS: [usize; 7] = [
    0,                                                                                  // Pawn
    MAX_HPAWN_NUM,                                                                      // Lance
    MAX_HPAWN_NUM + MAX_HLANCE_NUM,                                                     // Knight
    MAX_HPAWN_NUM + MAX_HLANCE_NUM + MAX_HKNIGHT_NUM,                                   // Silver
    MAX_HPAWN_NUM + MAX_HLANCE_NUM + MAX_HKNIGHT_NUM + MAX_HSILVER_NUM,                 // Gold
    MAX_HPAWN_NUM + MAX_HLANCE_NUM + MAX_HKNIGHT_NUM + MAX_HSILVER_NUM + MAX_HGOLD_NUM, // Bishop
    MAX_HPAWN_NUM
        + MAX_HLANCE_NUM
        + MAX_HKNIGHT_NUM
        + MAX_HSILVER_NUM
        + MAX_HGOLD_NUM
        + MAX_HBISHOP_NUM, // Rook
];

const HAND_MAXES: [usize; 7] = [
    MAX_HPAWN_NUM,
    MAX_HLANCE_NUM,
    MAX_HKNIGHT_NUM,
    MAX_HSILVER_NUM,
    MAX_HGOLD_NUM,
    MAX_HBISHOP_NUM,
    MAX_HROOK_NUM,
];

// ============================================================
// Feature extraction
// ============================================================

/// features1 に値をセット
/// layout: [color][feature][square] = flat index: c * 31 * 81 + f * 81 + sq
#[inline]
fn set_f1(features1: &mut [f32], c: usize, f: usize, sq: usize) {
    features1[c * MAX_FEATURES1_NUM * SQUARE_NUM + f * SQUARE_NUM + sq] = 1.0;
}

/// features2 の手駒プレーンをセット (num 枚分の連続プレーンを全マス 1.0 で埋める)
#[inline]
fn set_f2_hand(features2: &mut [f32], board_color: usize, offset: usize, num: usize) {
    let base = (MAX_PIECES_IN_HAND_SUM * board_color + offset) * SQUARE_NUM;
    for i in 0..num {
        let start = base + i * SQUARE_NUM;
        features2[start..start + SQUARE_NUM].fill(1.0);
    }
}

/// features2 の単一プレーンを全マス 1.0 で埋める
#[inline]
fn set_f2_plane(features2: &mut [f32], plane: usize) {
    let start = plane * SQUARE_NUM;
    features2[start..start + SQUARE_NUM].fill(1.0);
}

/// 駒種に応じた利きを計算する
fn piece_attacks(pt: PieceType, color: Color, sq: Square, occupied: Bitboard) -> Bitboard {
    match pt {
        PieceType::Pawn => pawn_effect(color, sq),
        PieceType::Lance => lance_effect(color, sq, occupied),
        PieceType::Knight => knight_effect(color, sq),
        PieceType::Silver => silver_effect(color, sq),
        PieceType::Gold
        | PieceType::ProPawn
        | PieceType::ProLance
        | PieceType::ProKnight
        | PieceType::ProSilver => gold_effect(color, sq),
        PieceType::Bishop => bishop_effect(sq, occupied),
        PieceType::Rook => rook_effect(sq, occupied),
        PieceType::Horse => horse_effect(sq, occupied),
        PieceType::Dragon => dragon_effect(sq, occupied),
        PieceType::King => king_effect(sq),
    }
}

/// AobaZero (dlshogi_aoba) 形式の入力特徴量を構築する
///
/// - `features1`: [2 * 31 * 81] = 5022 要素、事前にゼロクリアされていること
/// - `features2`: [86 * 81] = 6966 要素、事前にゼロクリアされていること
/// - `game_ply`: 局面の手数 (PackedSfenValue.game_ply)
/// - `draw_ply`: 引き分け手数 (0 = 調整なし)
pub fn make_input_features(
    pos: &Position,
    features1: &mut [f32],
    features2: &mut [f32],
    game_ply: i32,
    draw_ply: i32,
) {
    debug_assert!(features1.len() >= FEATURES1_SIZE);
    debug_assert!(features2.len() >= FEATURES2_SIZE);

    let turn = pos.side_to_move();
    let is_white_turn = turn == Color::White;
    let occupied = pos.occupied();

    // 利き数集計用 [color][square]
    let mut attack_num = [[0u8; SQUARE_NUM]; 2];

    // --- 歩以外の駒: 配置 + 利き + 利き数 ---
    let pawns_bb = pos.pieces_pt(PieceType::Pawn);
    let without_pawns = occupied & !pawns_bb;

    for sq in without_pawns.iter() {
        let pc = pos.piece_on(sq);
        let pt = pc.piece_type();
        let orig_color = pc.color();
        let attacks = piece_attacks(pt, orig_color, sq, occupied);

        // 出力用の色・マス (後手番なら反転)
        let (c, out_sq) = if is_white_turn {
            (orig_color.opponent() as usize, sq.inverse().index())
        } else {
            (orig_color as usize, sq.index())
        };

        // 駒の配置 (pt as usize - 1 で 0-indexed)
        let pt_idx = pt as usize - 1;
        set_f1(features1, c, pt_idx, out_sq);

        // 利き
        for to in attacks.iter() {
            let out_to = if is_white_turn {
                to.inverse().index()
            } else {
                to.index()
            };
            set_f1(features1, c, PIECETYPE_NUM + pt_idx, out_to);

            let num = &mut attack_num[c][out_to];
            if (*num as usize) < MAX_ATTACK_NUM {
                set_f1(features1, c, PIECETYPE_NUM + PIECETYPE_NUM + *num as usize, out_to);
                *num += 1;
            }
        }
    }

    // --- 歩 + 手駒 (色ごとに処理) ---
    let colors = [Color::Black, Color::White];
    for logical_c in 0..2usize {
        // board_color: 実際の盤面上の色
        let board_color = if is_white_turn {
            colors[1 - logical_c]
        } else {
            colors[logical_c]
        };

        // 歩の配置と利き
        let my_pawns = pawns_bb & pos.pieces_c(board_color);
        for sq in my_pawns.iter() {
            let out_sq = if is_white_turn {
                sq.inverse().index()
            } else {
                sq.index()
            };

            // 配置 (Pawn = 1, index = 0)
            set_f1(features1, logical_c, 0, out_sq);

            // 歩の利き: logical_c==0 は北方向(先手歩)、logical_c==1 は南方向(後手歩)
            // 反転後のマスに対して pawn_effect を適用
            let pawn_color = if logical_c == 0 {
                Color::Black
            } else {
                Color::White
            };
            if let Some(out_sq_sq) = Square::from_u8(out_sq as u8) {
                let effect = pawn_effect(pawn_color, out_sq_sq);
                for to in effect.iter() {
                    let out_to = to.index();
                    set_f1(features1, logical_c, PIECETYPE_NUM, out_to); // Pawn attack plane

                    let num = &mut attack_num[logical_c][out_to];
                    if (*num as usize) < MAX_ATTACK_NUM {
                        set_f1(
                            features1,
                            logical_c,
                            PIECETYPE_NUM + PIECETYPE_NUM + *num as usize,
                            out_to,
                        );
                        *num += 1;
                    }
                }
            }
        }

        // 手駒: C++では position.hand(c) で論理色の手駒を読み、
        // set_features2(features2, c2, ...) で盤面色のスロットに書く
        let hand = pos.hand(colors[logical_c]); // 論理色で読む
        let board_c_idx = board_color as usize; // 盤面色で書く
        let hand_piece_types = [
            PieceType::Pawn,
            PieceType::Lance,
            PieceType::Knight,
            PieceType::Silver,
            PieceType::Gold,
            PieceType::Bishop,
            PieceType::Rook,
        ];
        for (i, &hpt) in hand_piece_types.iter().enumerate() {
            let num = hand.count(hpt) as usize;
            let num = num.min(HAND_MAXES[i]);
            if num > 0 {
                set_f2_hand(features2, board_c_idx, HAND_OFFSETS[i], num);
            }
        }
    }

    // --- 王手フラグ ---
    if pos.in_check() {
        set_f2_plane(features2, MAX_FEATURES2_HAND_NUM); // plane 76
    }

    // --- 手番フラグ ---
    if is_white_turn {
        set_f2_plane(features2, MAX_FEATURES2_HAND_NUM + 1); // plane 77
    }

    // --- 手数カテゴリ ---
    let max_draw_moves: i32 = 513;
    let div = max_draw_moves - 1;
    let mut tt = game_ply.min(div);
    if draw_ply > 0 && draw_ply < max_draw_moves {
        let draw = draw_ply;
        let w = 160.min(draw);
        let d = draw - w;
        if tt > d {
            let x = (tt - d) as f64 * 2.0 / w as f64 - 1.0;
            tt = (1.0 / (1.0 + (-5.0 * x).exp()) * (513 - d) as f64 + d as f64) as i32;
        }
    }
    if tt > 190 {
        let g = ((tt - 190) / 40 + 1).min(8) as usize;
        set_f2_plane(features2, MAX_FEATURES2_HAND_NUM + 2 + g - 1); // planes 78-85
    }
}

/// 勝率 (0..1) をセンチポーン値に変換
///
/// dlshogi の Eval_Coef に相当する逆変換。
/// `scale` で bullet-shogi の --scale と整合させる。
/// winrate = sigmoid(cp / scale) の逆関数: cp = scale * ln(p / (1-p))
pub fn winrate_to_cp(winrate: f32, scale: f32) -> i32 {
    let clamped = winrate.clamp(0.001, 0.999);
    let logit = (clamped / (1.0 - clamped)).ln();
    (logit * scale) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_sizes() {
        assert_eq!(FEATURES1_SIZE, 2 * 31 * 81);
        assert_eq!(FEATURES2_SIZE, 86 * 81);
        assert_eq!(MAX_PIECES_IN_HAND_SUM, 38);
    }

    #[test]
    fn test_hirate_features() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();

        let mut f1 = vec![0.0f32; FEATURES1_SIZE];
        let mut f2 = vec![0.0f32; FEATURES2_SIZE];
        make_input_features(&pos, &mut f1, &mut f2, 1, 0);

        // 初期局面では手駒なし → features2 の手駒プレーンは全部 0
        let hand_planes: f32 = f2[..MAX_FEATURES2_HAND_NUM * SQUARE_NUM].iter().sum();
        assert_eq!(hand_planes, 0.0);

        // 王手でない → check plane = 0
        let check_plane: f32 = f2
            [MAX_FEATURES2_HAND_NUM * SQUARE_NUM..(MAX_FEATURES2_HAND_NUM + 1) * SQUARE_NUM]
            .iter()
            .sum();
        assert_eq!(check_plane, 0.0);

        // 先手番 → 手番プレーンは 0
        let turn_plane: f32 = f2
            [(MAX_FEATURES2_HAND_NUM + 1) * SQUARE_NUM..(MAX_FEATURES2_HAND_NUM + 2) * SQUARE_NUM]
            .iter()
            .sum();
        assert_eq!(turn_plane, 0.0);

        // features1: 先手側に駒が 20 個配置されているはず
        // 配置プレーン (最初の 14ch) だけをカウント
        let f1_black_placement: f32 = f1[..PIECETYPE_NUM * SQUARE_NUM].iter().sum();
        assert_eq!(f1_black_placement, 20.0, "Black should have 20 pieces in hirate");

        // 後手も同様
        let f1_white_start = MAX_FEATURES1_NUM * SQUARE_NUM;
        let f1_white_placement: f32 =
            f1[f1_white_start..f1_white_start + PIECETYPE_NUM * SQUARE_NUM].iter().sum();
        assert_eq!(f1_white_placement, 20.0, "White should have 20 pieces in hirate");
    }

    #[test]
    fn test_winrate_to_cp() {
        // 50% → 0 cp
        assert_eq!(winrate_to_cp(0.5, 600.0), 0);
        // 勝率が高いほど正のスコア
        assert!(winrate_to_cp(0.7, 600.0) > 0);
        assert!(winrate_to_cp(0.3, 600.0) < 0);
        // 対称性
        let cp_high = winrate_to_cp(0.7, 600.0);
        let cp_low = winrate_to_cp(0.3, 600.0);
        assert_eq!(cp_high, -cp_low);
    }
}
