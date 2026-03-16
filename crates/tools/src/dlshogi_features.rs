//! 標準 dlshogi 用の入力特徴量構築
//!
//! DL水匠等の標準 dlshogi モデルに対応。
//! AobaZero (dlshogi_aoba) とは以下の点で異なる:
//! - 歩の手駒上限: 8 (AobaZero: 18)
//! - 手番プレーンなし
//! - 手数カテゴリプレーンなし
//! - features2 合計: 57ch (AobaZero: 86ch)
//!
//! 参照: YaneuraOu/source/eval/deep/nn_types.h, nn_types.cpp

use rshogi_core::bitboard::{
    Bitboard, bishop_effect, dragon_effect, gold_effect, horse_effect, king_effect, knight_effect,
    lance_effect, pawn_effect, rook_effect, silver_effect,
};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, PieceType, Square};

// ============================================================
// Constants (標準 dlshogi)
// ============================================================

const MAX_HPAWN_NUM: usize = 8;
const MAX_HLANCE_NUM: usize = 4;
const MAX_HKNIGHT_NUM: usize = 4;
const MAX_HSILVER_NUM: usize = 4;
const MAX_HGOLD_NUM: usize = 4;
const MAX_HBISHOP_NUM: usize = 2;
const MAX_HROOK_NUM: usize = 2;

/// 手駒の枚数合計 (片方の色): 8+4+4+4+4+2+2 = 28
pub const MAX_PIECES_IN_HAND_SUM: usize = MAX_HPAWN_NUM
    + MAX_HLANCE_NUM
    + MAX_HKNIGHT_NUM
    + MAX_HSILVER_NUM
    + MAX_HGOLD_NUM
    + MAX_HBISHOP_NUM
    + MAX_HROOK_NUM;

/// 先後含めた手駒プレーン数: 28 * 2 = 56
const MAX_FEATURES2_HAND_NUM: usize = 2 * MAX_PIECES_IN_HAND_SUM;

const PIECETYPE_NUM: usize = 14; // Pawn(1) .. Dragon(14)
const MAX_ATTACK_NUM: usize = 3;

/// features1 チャンネル数 (片方の色): 14(配置) + 14(利き) + 3(利き数) = 31
pub const MAX_FEATURES1_NUM: usize = PIECETYPE_NUM + PIECETYPE_NUM + MAX_ATTACK_NUM;

/// features2 チャンネル数: 56(手駒) + 1(王手) = 57
pub const MAX_FEATURES2_NUM: usize = MAX_FEATURES2_HAND_NUM + 1;

const SQUARE_NUM: usize = 81;

/// features1 の 1 バッチ分のサイズ (2色 × 31ch × 81sq)
pub const FEATURES1_SIZE: usize = 2 * MAX_FEATURES1_NUM * SQUARE_NUM; // 5022
/// features2 の 1 バッチ分のサイズ (57ch × 81sq)
pub const FEATURES2_SIZE: usize = MAX_FEATURES2_NUM * SQUARE_NUM; // 4617

/// ONNX input1 チャンネル数: 2 × 31 = 62
pub const INPUT1_CHANNELS: usize = 2 * MAX_FEATURES1_NUM;
/// ONNX input2 チャンネル数: 57
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

/// 標準 dlshogi 形式の入力特徴量を構築する
///
/// - `features1`: [2 * 31 * 81] = 5022 要素、事前にゼロクリアされていること
/// - `features2`: [57 * 81] = 4617 要素、事前にゼロクリアされていること
pub fn make_input_features(pos: &Position, features1: &mut [f32], features2: &mut [f32]) {
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

        // 手駒
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
        set_f2_plane(features2, MAX_FEATURES2_HAND_NUM); // plane 56
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
        assert_eq!(FEATURES2_SIZE, 57 * 81);
        assert_eq!(MAX_PIECES_IN_HAND_SUM, 28);
        assert_eq!(MAX_FEATURES2_NUM, 57);
        assert_eq!(INPUT1_CHANNELS, 62);
        assert_eq!(INPUT2_CHANNELS, 57);
    }

    #[test]
    fn test_hirate_features() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();

        let mut f1 = vec![0.0f32; FEATURES1_SIZE];
        let mut f2 = vec![0.0f32; FEATURES2_SIZE];
        make_input_features(&pos, &mut f1, &mut f2);

        // 初期局面では手駒なし → features2 の手駒プレーンは全部 0
        let hand_planes: f32 = f2[..MAX_FEATURES2_HAND_NUM * SQUARE_NUM].iter().sum();
        assert_eq!(hand_planes, 0.0);

        // 王手でない → check plane = 0
        let check_plane: f32 = f2
            [MAX_FEATURES2_HAND_NUM * SQUARE_NUM..(MAX_FEATURES2_HAND_NUM + 1) * SQUARE_NUM]
            .iter()
            .sum();
        assert_eq!(check_plane, 0.0);

        // features1: 先手側に駒が 20 個配置されているはず
        let f1_black_placement: f32 = f1[..PIECETYPE_NUM * SQUARE_NUM].iter().sum();
        assert_eq!(f1_black_placement, 20.0, "Black should have 20 pieces in hirate");

        // 後手も同様
        let f1_white_start = MAX_FEATURES1_NUM * SQUARE_NUM;
        let f1_white_placement: f32 =
            f1[f1_white_start..f1_white_start + PIECETYPE_NUM * SQUARE_NUM].iter().sum();
        assert_eq!(f1_white_placement, 20.0, "White should have 20 pieces in hirate");
    }

    #[test]
    fn test_hand_pieces() {
        // 手駒がある局面で features2 が正しくセットされるか
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b 2P 1")
            .unwrap();

        let mut f1 = vec![0.0f32; FEATURES1_SIZE];
        let mut f2 = vec![0.0f32; FEATURES2_SIZE];
        make_input_features(&pos, &mut f1, &mut f2);

        // 先手番なので logical_c=0 が先手、board_color=Black(0)
        // 先手が歩2枚持ち → board_c_idx=0, offset=0(Pawn), 2枚分のプレーンが埋まる
        let pawn_plane0: f32 = f2[..SQUARE_NUM].iter().sum();
        let pawn_plane1: f32 = f2[SQUARE_NUM..2 * SQUARE_NUM].iter().sum();
        let pawn_plane2: f32 = f2[2 * SQUARE_NUM..3 * SQUARE_NUM].iter().sum();
        assert_eq!(pawn_plane0, 81.0, "Hand pawn plane 0 should be all 1s");
        assert_eq!(pawn_plane1, 81.0, "Hand pawn plane 1 should be all 1s");
        assert_eq!(pawn_plane2, 0.0, "Hand pawn plane 2 should be all 0s");
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
