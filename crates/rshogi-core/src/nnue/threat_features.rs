//! Threat 特徴量
//!
//! 盤上の駒の攻撃関係（threat pair）を NNUE 特徴量として列挙する。
//! 各 pair は (attacker_side, attacker_class, attacked_side, attacked_class, from_sq, to_sq) で一意に決まる。
//!
//! ## 仕様
//!
//! - 仕様固定メモ: `docs/threat_spec.md`
//! - 設計書: `docs/nnue_architecture_research.md` Phase 3

use crate::bitboard::{
    Bitboard, bishop_effect, dragon_effect, gold_effect, horse_effect, knight_effect, lance_effect,
    pawn_effect, rook_effect, silver_effect,
};
use crate::position::Position;
use crate::types::{Color, PieceType, Square};

use super::bona_piece_halfka_hm::is_hm_mirror;

// =============================================================================
// 定数
// =============================================================================

/// Threat の総特徴量次元数
pub const THREAT_DIMENSIONS: usize = 216_720;

/// ThreatClass の数（King 除外）
pub const NUM_THREAT_CLASSES: usize = 9;

/// active threat features の最大数
pub const MAX_ACTIVE_THREAT_FEATURES: usize = 320;

// =============================================================================
// ThreatClass
// =============================================================================

/// Threat 駒種分類（King 除外、9 family）
///
/// 順序は仕様固定（`docs/threat_spec.md`）。変更禁止。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ThreatClass {
    Pawn = 0,
    Lance = 1,
    Knight = 2,
    Silver = 3,
    GoldLike = 4,
    Bishop = 5,
    Rook = 6,
    Horse = 7,
    Dragon = 8,
}

impl ThreatClass {
    /// PieceType から ThreatClass への変換。King は None。
    #[inline]
    pub fn from_piece_type(pt: PieceType) -> Option<Self> {
        match pt {
            PieceType::Pawn => Some(Self::Pawn),
            PieceType::Lance => Some(Self::Lance),
            PieceType::Knight => Some(Self::Knight),
            PieceType::Silver => Some(Self::Silver),
            PieceType::Gold
            | PieceType::ProPawn
            | PieceType::ProLance
            | PieceType::ProKnight
            | PieceType::ProSilver => Some(Self::GoldLike),
            PieceType::Bishop => Some(Self::Bishop),
            PieceType::Rook => Some(Self::Rook),
            PieceType::Horse => Some(Self::Horse),
            PieceType::Dragon => Some(Self::Dragon),
            PieceType::King => None,
        }
    }
}

// =============================================================================
// 各クラスの空盤面利き数 (per color)
// =============================================================================

/// 各 ThreatClass の attacks_per_color
const ATTACKS_PER_COLOR: [usize; NUM_THREAT_CLASSES] = [
    72,   // Pawn
    324,  // Lance
    112,  // Knight
    328,  // Silver
    416,  // GoldLike
    816,  // Bishop
    1296, // Rook
    1104, // Horse
    1552, // Dragon
];

// =============================================================================
// pair_base テーブル
// =============================================================================

/// pair_base[attacker_side][attacker_class][attacked_side][attacked_class]
/// flat index: as * 162 + ac * 18 + ds * 9 + dc
const NUM_PAIRS: usize = 2 * NUM_THREAT_CLASSES * 2 * NUM_THREAT_CLASSES; // 324

/// pair_base テーブルを構築
const fn build_pair_base() -> [usize; NUM_PAIRS] {
    let mut table = [0usize; NUM_PAIRS];
    let mut cumulative = 0usize;
    let mut attacker_side = 0usize;
    while attacker_side < 2 {
        let mut ac = 0usize;
        while ac < NUM_THREAT_CLASSES {
            let mut ds = 0usize;
            while ds < 2 {
                let mut dc = 0usize;
                while dc < NUM_THREAT_CLASSES {
                    let idx = attacker_side * 162 + ac * 18 + ds * 9 + dc;
                    table[idx] = cumulative;
                    cumulative += ATTACKS_PER_COLOR[ac];
                    dc += 1;
                }
                ds += 1;
            }
            ac += 1;
        }
        attacker_side += 1;
    }
    table
}

static PAIR_BASE: [usize; NUM_PAIRS] = build_pair_base();

/// pair_base を取得
#[inline]
fn pair_base(
    attacker_side: usize,
    ac: ThreatClass,
    attacked_side: usize,
    dc: ThreatClass,
) -> usize {
    let idx = attacker_side * 162 + (ac as usize) * 18 + attacked_side * 9 + dc as usize;
    PAIR_BASE[idx]
}

// =============================================================================
// from_offset テーブル + attack_order（色別 LUT、Stockfish 準拠）
// =============================================================================

/// 方向性駒かどうか
fn is_directional(class: ThreatClass) -> bool {
    matches!(
        class,
        ThreatClass::Pawn
            | ThreatClass::Lance
            | ThreatClass::Knight
            | ThreatClass::Silver
            | ThreatClass::GoldLike
    )
}

/// 空盤面上の攻撃先 Bitboard を取得（色指定版）
///
/// 方向性駒は `color` で攻撃方向が変わる。
/// 非方向性駒は `color` に関係なく同じ結果を返す。
fn attacks_bb_colored(class: ThreatClass, color: Color, sq: Square) -> Bitboard {
    let empty = Bitboard::EMPTY;
    match class {
        ThreatClass::Pawn => pawn_effect(color, sq),
        ThreatClass::Lance => lance_effect(color, sq, empty),
        ThreatClass::Knight => knight_effect(color, sq),
        ThreatClass::Silver => silver_effect(color, sq),
        ThreatClass::GoldLike => gold_effect(color, sq),
        ThreatClass::Bishop => bishop_effect(sq, empty),
        ThreatClass::Rook => rook_effect(sq, empty),
        ThreatClass::Horse => horse_effect(sq, empty),
        ThreatClass::Dragon => dragon_effect(sq, empty),
    }
}

/// 空盤面上の攻撃先 Bitboard を取得（先手基準、テスト用）
fn attacks_bb(class: ThreatClass, sq: Square) -> Bitboard {
    attacks_bb_colored(class, Color::Black, sq)
}

/// 各クラスの各マスの空盤面攻撃数
fn attacks_count(class: ThreatClass, sq: Square) -> usize {
    attacks_bb(class, sq).count() as usize
}

/// from_offset[class][sq] = sq=0..sq-1 の攻撃数累積和
fn compute_from_offset_colored(class: ThreatClass, color: Color) -> [usize; 81] {
    let mut offsets = [0usize; 81];
    let mut cumulative = 0usize;
    for sq_raw in 0..81u8 {
        offsets[sq_raw as usize] = cumulative;
        let sq = unsafe { Square::from_u8_unchecked(sq_raw) };
        cumulative += attacks_bb_colored(class, color, sq).count() as usize;
    }
    offsets
}

/// Attack pattern ID: 方向性駒は色別、非方向性駒は色不問
///
/// 0..8: Black (先手) の各 ThreatClass
/// 9..13: White (後手) の方向性駒 (Pawn=9, Lance=10, Knight=11, Silver=12, GoldLike=13)
const NUM_ATTACK_PATTERNS: usize = 14;

fn attack_pattern_id(class: ThreatClass, oriented_color: Color) -> usize {
    if oriented_color == Color::White && is_directional(class) {
        NUM_THREAT_CLASSES + class as usize // 9..13
    } else {
        class as usize // 0..8
    }
}

/// 全 attack pattern の from_offset テーブル
struct FromOffsetTable {
    data: [[usize; 81]; NUM_ATTACK_PATTERNS],
}

impl FromOffsetTable {
    fn new() -> Self {
        let mut data = [[0usize; 81]; NUM_ATTACK_PATTERNS];
        for class_id in 0..NUM_THREAT_CLASSES {
            let class = unsafe { std::mem::transmute::<u8, ThreatClass>(class_id as u8) };
            // Black (先手) の from_offset
            data[class_id] = compute_from_offset_colored(class, Color::Black);
            // White (後手) の方向性駒は別エントリ
            if is_directional(class) {
                data[NUM_THREAT_CLASSES + class_id] =
                    compute_from_offset_colored(class, Color::White);
            }
        }
        Self { data }
    }

    #[inline]
    fn get(&self, pattern: usize, sq_n: Square) -> usize {
        self.data[pattern][sq_n.raw() as usize]
    }
}

/// attack_order: from_sq の攻撃先を raw 昇順で列挙したときの to_sq の順位（色別）
fn compute_attack_order_colored(
    class: ThreatClass,
    color: Color,
    from_sq: Square,
    to_sq: Square,
) -> usize {
    let bb = attacks_bb_colored(class, color, from_sq);
    let to_raw = to_sq.raw();
    let mut order = 0;
    let mut iter = bb;
    while !iter.is_empty() {
        let target = iter.pop();
        if target.raw() == to_raw {
            return order;
        }
        order += 1;
    }
    panic!(
        "attack_order: to_sq {} is not attacked by {:?} ({:?}) at {}",
        to_sq.raw(),
        class,
        color,
        from_sq.raw()
    );
}

// =============================================================================
// Threat index 計算
// =============================================================================

/// Threat index を計算する（Stockfish 準拠: perspective 基準 + 色別 LUT）
///
/// # 引数
/// - `attacker_side`: 0 = perspective side (friend), 1 = opposite side (enemy)
/// - `attacker_class`: 攻撃駒の ThreatClass
/// - `oriented_color`: perspective swap 後の attacker 色
/// - `attacked_side`: 0 = perspective side (friend), 1 = opposite side (enemy)
/// - `attacked_class`: 被攻撃駒の ThreatClass
/// - `from_sq_n`: 正規化後の攻撃駒のマス
/// - `to_sq_n`: 正規化後の被攻撃駒のマス
/// - `from_offset_table`: 事前計算された from_offset テーブル
#[inline]
fn threat_index(
    attacker_side: usize,
    attacker_class: ThreatClass,
    oriented_color: Color,
    attacked_side: usize,
    attacked_class: ThreatClass,
    from_sq_n: Square,
    to_sq_n: Square,
    from_offset_table: &FromOffsetTable,
) -> usize {
    let base = pair_base(attacker_side, attacker_class, attacked_side, attacked_class);
    let pattern = attack_pattern_id(attacker_class, oriented_color);
    let from_off = from_offset_table.get(pattern, from_sq_n);
    let attack_ord =
        compute_attack_order_colored(attacker_class, oriented_color, from_sq_n, to_sq_n);
    base + from_off + attack_ord
}

// =============================================================================
// マス正規化
// =============================================================================

/// マスを perspective 基準 + HM mirror で正規化（Stockfish 準拠）
///
/// HalfKA_hm と同じ perspective 基準。
/// 方向性駒の利き方向の整合は、色別 attack LUT で解決する。
#[inline]
fn normalize_sq(sq: Square, perspective: Color, hm_mirror: bool) -> Square {
    let sq_n = if perspective == Color::Black {
        sq
    } else {
        sq.inverse()
    };
    if hm_mirror { sq_n.mirror() } else { sq_n }
}

// =============================================================================
// append_active_indices
// =============================================================================

/// 現局面の全 threat pair を列挙し、indices に追加する。
///
/// 列挙方法:
/// 1. 盤上の各駒について、その駒が攻撃しているマスを実盤面 occupied で列挙
/// 2. 攻撃先マスに駒がいれば threat pair を生成
/// 3. attacker/attacked とも King は除外
pub fn append_active_threat_indices(
    pos: &Position,
    perspective: Color,
    king_sq: Square,
    indices: &mut Vec<usize>,
) {
    let hm = is_hm_mirror(king_sq, perspective);
    let occupied = pos.occupied();
    let from_offset_table = FromOffsetTable::new();

    // perspective から見た friend/enemy
    let friend_color = perspective;
    let enemy_color = !perspective;

    // 全盤上駒を列挙
    for &attacker_color in &[friend_color, enemy_color] {
        let attacker_side = if attacker_color == friend_color { 0 } else { 1 };

        let mut attacker_bb = pos.pieces_c(attacker_color);
        while !attacker_bb.is_empty() {
            let from_sq = attacker_bb.pop();
            let pc = pos.piece_on(from_sq);
            let pt = pc.piece_type();

            let Some(attacker_class) = ThreatClass::from_piece_type(pt) else {
                continue; // King は除外
            };

            // 実盤面上の攻撃先
            let attack_bb = attacks_from_piece(pt, attacker_color, from_sq, occupied);

            let mut targets = attack_bb & occupied;
            while !targets.is_empty() {
                let to_sq = targets.pop();
                let target_pc = pos.piece_on(to_sq);
                let target_pt = target_pc.piece_type();
                let target_color = target_pc.color();

                let Some(attacked_class) = ThreatClass::from_piece_type(target_pt) else {
                    continue; // King は除外
                };

                let attacked_side = if target_color == friend_color { 0 } else { 1 };

                // Perspective 基準で正規化（Stockfish 準拠）
                let from_sq_n = normalize_sq(from_sq, perspective, hm);
                let to_sq_n = normalize_sq(to_sq, perspective, hm);

                // oriented_color: perspective swap 後の attacker 色
                // perspective=Black なら attacker_color そのまま
                // perspective=White なら Black↔White 反転
                let oriented_color = if perspective == Color::Black {
                    attacker_color
                } else {
                    !attacker_color
                };

                let idx = threat_index(
                    attacker_side,
                    attacker_class,
                    oriented_color,
                    attacked_side,
                    attacked_class,
                    from_sq_n,
                    to_sq_n,
                    &from_offset_table,
                );

                debug_assert!(
                    idx < THREAT_DIMENSIONS,
                    "threat index out of range: {idx} >= {THREAT_DIMENSIONS}"
                );

                indices.push(idx);
            }
        }
    }
}

/// 駒種・色・マス・occupied から実盤面上の攻撃先 Bitboard を取得
fn attacks_from_piece(pt: PieceType, color: Color, sq: Square, occupied: Bitboard) -> Bitboard {
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
        PieceType::King => Bitboard::EMPTY,
    }
}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_threat_class_from_piece_type() {
        assert_eq!(ThreatClass::from_piece_type(PieceType::Pawn), Some(ThreatClass::Pawn));
        assert_eq!(ThreatClass::from_piece_type(PieceType::ProPawn), Some(ThreatClass::GoldLike));
        assert_eq!(ThreatClass::from_piece_type(PieceType::Gold), Some(ThreatClass::GoldLike));
        assert_eq!(ThreatClass::from_piece_type(PieceType::King), None);
        // PieceType には None variant がないため、King のみ None を返すことを確認
    }

    #[test]
    fn test_pair_base_dimensions() {
        // 最後の pair の末尾が THREAT_DIMENSIONS と一致
        let last_idx = 162 + 8 * 18 + 9 + 8; // as=1, ac=Dragon, ds=1, dc=Dragon
        let last_base = PAIR_BASE[last_idx];
        assert_eq!(last_base + ATTACKS_PER_COLOR[ThreatClass::Dragon as usize], THREAT_DIMENSIONS);
    }

    #[test]
    fn test_from_offset_pawn() {
        let offsets = compute_from_offset_colored(ThreatClass::Pawn, Color::Black);
        // Pawn: rank=0 → 0 attacks, rank>0 → 1 attack
        // sq=0 (file=0, rank=0): offset=0, attacks=0
        // sq=1 (file=0, rank=1): offset=0, attacks=1
        assert_eq!(offsets[0], 0);
        assert_eq!(offsets[1], 0); // sq=0 has 0 attacks
        assert_eq!(offsets[2], 1); // sq=1 has 1 attack, cumulative=1
        // Total: 72
        let total: usize = (0..81u8)
            .map(|sq| attacks_count(ThreatClass::Pawn, unsafe { Square::from_u8_unchecked(sq) }))
            .sum();
        assert_eq!(total, 72);
    }

    #[test]
    fn test_from_offset_rook() {
        let offsets = compute_from_offset_colored(ThreatClass::Rook, Color::Black);
        // Rook: 全マスで attacks=16
        for (sq, &ofs) in offsets.iter().enumerate() {
            assert_eq!(ofs, 16 * sq);
        }
        let total: usize = (0..81u8)
            .map(|sq| attacks_count(ThreatClass::Rook, unsafe { Square::from_u8_unchecked(sq) }))
            .sum();
        assert_eq!(total, 1296);
    }

    #[test]
    fn test_attacks_per_color_totals() {
        for (class_id, &expected) in ATTACKS_PER_COLOR.iter().enumerate() {
            let class = unsafe { std::mem::transmute::<u8, ThreatClass>(class_id as u8) };
            let total: usize = (0..81u8)
                .map(|sq| attacks_count(class, unsafe { Square::from_u8_unchecked(sq) }))
                .sum();
            assert_eq!(
                total, expected,
                "ThreatClass {:?}: expected {expected}, got {total}",
                class
            );
        }
    }

    #[test]
    fn test_attack_order_rook_center() {
        // Rook at sq=40 (5五): 攻撃先を raw 昇順で列挙
        let sq = unsafe { Square::from_u8_unchecked(40) };
        let bb = attacks_bb(ThreatClass::Rook, sq);
        assert_eq!(bb.count(), 16);

        // attack_order は 0-indexed
        // 最初の攻撃先（raw 最小）は order=0
        let mut iter = bb;
        let first = iter.pop();
        assert_eq!(compute_attack_order_colored(ThreatClass::Rook, Color::Black, sq, first), 0);
    }

    #[test]
    fn test_threat_index_range() {
        let from_offset_table = FromOffsetTable::new();
        // 全クラスの全マスの全攻撃先について index が範囲内であることを確認
        // 先手基準 (oriented_color=Black) と後手基準 (oriented_color=White) の両方をテスト
        for class_id in 0..NUM_THREAT_CLASSES {
            let class = unsafe { std::mem::transmute::<u8, ThreatClass>(class_id as u8) };
            for &oriented_color in &[Color::Black, Color::White] {
                for sq_raw in 0..81u8 {
                    let sq = unsafe { Square::from_u8_unchecked(sq_raw) };
                    let bb = attacks_bb_colored(class, oriented_color, sq);
                    let mut iter = bb;
                    while !iter.is_empty() {
                        let to = iter.pop();
                        for as_ in 0..2 {
                            for ds in 0..2 {
                                for dc in 0..NUM_THREAT_CLASSES {
                                    let dc_class =
                                        unsafe { std::mem::transmute::<u8, ThreatClass>(dc as u8) };
                                    let idx = threat_index(
                                        as_,
                                        class,
                                        oriented_color,
                                        ds,
                                        dc_class,
                                        sq,
                                        to,
                                        &from_offset_table,
                                    );
                                    assert!(
                                        idx < THREAT_DIMENSIONS,
                                        "index {idx} out of range for class={class:?} color={oriented_color:?} sq={} to={}",
                                        sq.raw(),
                                        to.raw()
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_startpos_active_threats() {
        // 初期局面での threat 列挙が正常に動作することを確認
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("Failed to parse startpos");
        let king_sq_b = pos.king_square(Color::Black);
        let king_sq_w = pos.king_square(Color::White);

        let mut indices_b = Vec::new();
        append_active_threat_indices(&pos, Color::Black, king_sq_b, &mut indices_b);

        let mut indices_w = Vec::new();
        append_active_threat_indices(&pos, Color::White, king_sq_w, &mut indices_w);

        // 初期局面では threat pair は限定的（歩同士の対面等）
        // 具体的な数は仕様依存だが、0 ではないはず
        assert!(!indices_b.is_empty(), "Black perspective should have threats");
        assert!(!indices_w.is_empty(), "White perspective should have threats");

        // 全 index が範囲内
        for &idx in &indices_b {
            assert!(idx < THREAT_DIMENSIONS);
        }
        for &idx in &indices_w {
            assert!(idx < THREAT_DIMENSIONS);
        }
    }

    /// Canonical test vector: 初期局面の sorted threat index を固定値と比較
    /// bullet-shogi 側のテストと一致することを確認するためのテスト
    #[test]
    fn test_canonical_startpos_threat_indices() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("Failed to parse startpos");

        let king_sq_b = pos.king_square(Color::Black);
        let mut indices_b = Vec::new();
        append_active_threat_indices(&pos, Color::Black, king_sq_b, &mut indices_b);
        indices_b.sort();

        let king_sq_w = pos.king_square(Color::White);
        let mut indices_w = Vec::new();
        append_active_threat_indices(&pos, Color::White, king_sq_w, &mut indices_w);
        indices_w.sort();

        // Canonical test vector: この値は bullet-shogi と一致すること
        // 変更がある場合は両方のリポジトリで同時に更新すること
        #[rustfmt::skip]
        let expected: &[usize] = &[
            1330, 1618, 7147, 7148, 7231, 7232, 11047, 11213,
            16475, 16578, 23268, 23270, 24087, 25717, 37487, 40080,
            43974, 112573, 112861, 116503, 116504, 116587, 116588,
            122160, 122650, 128533, 128636, 138321, 138323, 139136,
            140770, 158280, 160871, 164753,
        ];

        assert_eq!(indices_b, expected, "Black perspective canonical mismatch");
        assert_eq!(indices_w, expected, "White perspective canonical mismatch (symmetric pos)");
    }
}
