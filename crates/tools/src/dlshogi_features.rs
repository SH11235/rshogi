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
use rshogi_core::types::{Color, Move, PieceType, Square};

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

// ============================================================
// Policy move label (cshogi の make_move_label 相当)
// ============================================================

/// ポリシーヘッドの移動方向ラベル
///
/// 10 方向 × 2 (不成り/成り) = 20 盤上方向 + 7 持ち駒 = 合計 27 カテゴリ。
/// ポリシー出力サイズ: 27 × 81 = 2187。
///
/// cshogi の `MOVE_DIRECTION` enum と同一順序。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveDirection {
    Up = 0,
    UpLeft = 1,
    UpRight = 2,
    Left = 3,
    Right = 4,
    Down = 5,
    DownLeft = 6,
    DownRight = 7,
    Up2Left = 8,
    Up2Right = 9,
    // 成り (上記 + 10)
    UpPromote = 10,
    UpLeftPromote = 11,
    UpRightPromote = 12,
    LeftPromote = 13,
    RightPromote = 14,
    DownPromote = 15,
    DownLeftPromote = 16,
    DownRightPromote = 17,
    Up2LeftPromote = 18,
    Up2RightPromote = 19,
}

/// 盤上移動方向の数 (不成り10 + 成り10 = 20)
const MOVE_DIRECTION_NUM: u32 = 20;

/// ポリシーラベルの総数: (20 盤上方向 + 7 持ち駒) × 81 マス
pub const MAX_MOVE_LABEL_NUM: usize = 27 * SQUARE_NUM; // 2187

/// from→to の差分から移動方向を判定する
///
/// `dir_x = from_file - to_file`, `dir_y = to_rank - from_rank`
/// (cshogi と同一符号: 上方向が負、右方向が正)
fn get_move_direction(dir_x: i32, dir_y: i32) -> MoveDirection {
    if dir_y < 0 && dir_x == 0 {
        MoveDirection::Up
    } else if dir_y == -2 && dir_x == -1 {
        MoveDirection::Up2Left
    } else if dir_y == -2 && dir_x == 1 {
        MoveDirection::Up2Right
    } else if dir_y < 0 && dir_x < 0 {
        MoveDirection::UpLeft
    } else if dir_y < 0 && dir_x > 0 {
        MoveDirection::UpRight
    } else if dir_y == 0 && dir_x < 0 {
        MoveDirection::Left
    } else if dir_y == 0 && dir_x > 0 {
        MoveDirection::Right
    } else if dir_y > 0 && dir_x == 0 {
        MoveDirection::Down
    } else if dir_y > 0 && dir_x < 0 {
        MoveDirection::DownLeft
    } else {
        // dir_y > 0 && dir_x > 0
        MoveDirection::DownRight
    }
}

/// 指し手をポリシー出力のラベルインデックスに変換する
///
/// cshogi の `__dlshogi_make_move_label` と同一ロジック。
/// 後手番では盤面を 180 度回転して先手視点に正規化する。
///
/// 戻り値は `0..2187` のインデックス。
pub fn make_move_label(mv: Move, color: Color) -> usize {
    let is_white = color == Color::White;

    if !mv.is_drop() {
        let (to_sq, from_sq) = if is_white {
            (mv.to().inverse().index(), mv.from().inverse().index())
        } else {
            (mv.to().index(), mv.from().index())
        };

        let to_file = to_sq / 9;
        let to_rank = to_sq % 9;
        let from_file = from_sq / 9;
        let from_rank = from_sq % 9;

        let dir_x = from_file as i32 - to_file as i32;
        let dir_y = to_rank as i32 - from_rank as i32;

        let mut direction = get_move_direction(dir_x, dir_y) as u32;

        if mv.is_promote() {
            direction += 10;
        }

        (SQUARE_NUM as u32 * direction + to_sq as u32) as usize
    } else {
        let to_sq = if is_white {
            mv.to().inverse().index()
        } else {
            mv.to().index()
        };

        // 手駒種別: Pawn=1..Rook=6 → 0..5, Gold=7 → 6
        let pt = mv.drop_piece_type() as u32;
        let hand_piece = if pt <= 6 { pt - 1 } else { 6 }; // Gold(7) → 6

        let direction = MOVE_DIRECTION_NUM + hand_piece;

        (SQUARE_NUM as u32 * direction + to_sq as u32) as usize
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
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b 2P 1").unwrap();

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

    // ============================================================
    // make_move_label テスト
    // ============================================================

    #[test]
    fn test_max_move_label_num() {
        assert_eq!(MAX_MOVE_LABEL_NUM, 2187);
    }

    #[test]
    fn test_make_move_label_range() {
        // 初期局面の全合法手がラベル範囲内に収まること
        use rshogi_core::movegen::{MoveList, generate_legal};

        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();
        let mut list = MoveList::new();
        generate_legal(&pos, &mut list);

        for mv in list.iter() {
            let label = make_move_label(*mv, Color::Black);
            assert!(label < MAX_MOVE_LABEL_NUM, "label {label} out of range for {}", mv.to_usi());
        }
    }

    #[test]
    fn test_make_move_label_up_move_black() {
        // 7g7f (先手): file=6→6, rank=6→5 → dir_x=0, dir_y=-1 → UP
        // to_sq = 6*9+5 = 59, label = 81*0 + 59 = 59
        let mv = Move::from_usi("7g7f").unwrap();
        let label = make_move_label(mv, Color::Black);
        assert_eq!(label, 59);
    }

    #[test]
    fn test_make_move_label_promote() {
        // 2d2c+ (先手): file=1→1, rank=3→2 → dir_x=0, dir_y=-1 → UP_PROMOTE=10
        // to_sq = 1*9+2 = 11, label = 81*10 + 11 = 821
        let mv = Move::from_usi("2d2c+").unwrap();
        let label = make_move_label(mv, Color::Black);
        assert_eq!(label, 821);
    }

    #[test]
    fn test_make_move_label_drop_pawn() {
        // P*5e (先手): hand_piece=0(Pawn), direction=20
        // to_sq = 4*9+4 = 40, label = 81*20 + 40 = 1660
        let mv = Move::from_usi("P*5e").unwrap();
        let label = make_move_label(mv, Color::Black);
        assert_eq!(label, 1660);
    }

    #[test]
    fn test_make_move_label_drop_gold() {
        // G*5e (先手): hand_piece=6(Gold), direction=26
        // to_sq = 4*9+4 = 40, label = 81*26 + 40 = 2146
        let mv = Move::from_usi("G*5e").unwrap();
        let label = make_move_label(mv, Color::Black);
        assert_eq!(label, 2146);
    }

    #[test]
    fn test_make_move_label_white_rotation() {
        // 後手: 7g7f → 盤面反転 → to=80-59=21, from=80-60=20
        // to=(file=2,rank=3), from=(file=2,rank=2) → dir_x=0, dir_y=1 → DOWN=5
        // label = 81*5 + 21 = 426
        let mv = Move::from_usi("7g7f").unwrap();
        let label = make_move_label(mv, Color::White);
        assert_eq!(label, 426);
    }

    #[test]
    fn test_make_move_label_all_legal_in_range() {
        // 中盤局面で全合法手のラベルが範囲内
        use rshogi_core::movegen::{MoveList, generate_legal};

        let mut pos = Position::new();
        pos.set_sfen("l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w GR5p 1")
            .unwrap();
        let color = pos.side_to_move();
        let mut list = MoveList::new();
        generate_legal(&pos, &mut list);

        for mv in list.iter() {
            let label = make_move_label(*mv, color);
            assert!(label < MAX_MOVE_LABEL_NUM, "label {label} out of range for {}", mv.to_usi());
        }
    }
}
