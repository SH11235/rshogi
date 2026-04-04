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

use super::accumulator::{DirtyPiece, IndexList};
use super::bona_piece::BonaPiece;
use super::bona_piece_halfka_hm::is_hm_mirror;

use std::sync::LazyLock;

// =============================================================================
// 定数
// =============================================================================

/// Threat の総特徴量次元数
pub const THREAT_DIMENSIONS: usize = 216_720;

/// ThreatClass の数（King 除外）
pub const NUM_THREAT_CLASSES: usize = 9;

/// active threat features の最大数
pub const MAX_ACTIVE_THREAT_FEATURES: usize = 320;

/// changed threat features の最大数（差分更新用）
pub const MAX_CHANGED_THREAT_FEATURES: usize = 192;

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

/// 全 ThreatClass を index 順に列挙した配列
const ALL_THREAT_CLASSES: [ThreatClass; NUM_THREAT_CLASSES] = [
    ThreatClass::Pawn,
    ThreatClass::Lance,
    ThreatClass::Knight,
    ThreatClass::Silver,
    ThreatClass::GoldLike,
    ThreatClass::Bishop,
    ThreatClass::Rook,
    ThreatClass::Horse,
    ThreatClass::Dragon,
];

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
        let sq = Square::from_u8(sq_raw).expect("sq_raw is in 0..81");
        cumulative += attacks_bb_colored(class, color, sq).count() as usize;
    }
    offsets
}

/// Attack pattern ID: 方向性駒は色別、非方向性駒は色不問
///
/// 0..8: Black (先手) の各 ThreatClass
/// 9..13: White (後手) の方向性駒 (Pawn=9, Lance=10, Knight=11, Silver=12, GoldLike=13)
const NUM_ATTACK_PATTERNS: usize = 14;

/// FromOffsetTable の LazyLock キャッシュ
static FROM_OFFSET_TABLE: LazyLock<FromOffsetTable> = LazyLock::new(FromOffsetTable::new);

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
        for (class_id, &class) in ALL_THREAT_CLASSES.iter().enumerate() {
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
///
/// テスト用。ホットパスでは `AttackOrderTable` の LUT を使用する。
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

/// attack_order の事前計算 LUT
///
/// `data[attack_pattern][from_sq][to_sq]` = 空盤面上で from_sq の駒が to_sq を攻撃する際の
/// 0-indexed 順位。攻撃しない (from, to) ペアは `INVALID` (u8::MAX)。
///
/// サイズ: 14 × 81 × 81 = 91,854 bytes ≈ 90 KiB
struct AttackOrderTable {
    data: [[[u8; 81]; 81]; NUM_ATTACK_PATTERNS],
}

impl AttackOrderTable {
    const INVALID: u8 = u8::MAX;

    fn new() -> Self {
        let mut data = [[[Self::INVALID; 81]; 81]; NUM_ATTACK_PATTERNS];
        for (class_id, &class) in ALL_THREAT_CLASSES.iter().enumerate() {
            // Black (先手) pattern
            Self::fill_pattern(&mut data[class_id], class, Color::Black);
            // White (後手) 方向性駒は別エントリ
            if is_directional(class) {
                Self::fill_pattern(&mut data[NUM_THREAT_CLASSES + class_id], class, Color::White);
            }
        }
        Self { data }
    }

    fn fill_pattern(table: &mut [[u8; 81]; 81], class: ThreatClass, color: Color) {
        for from_raw in 0..81u8 {
            let from_sq = Square::from_u8(from_raw).unwrap();
            let bb = attacks_bb_colored(class, color, from_sq);
            let mut order: u8 = 0;
            let mut iter = bb;
            while !iter.is_empty() {
                let to_sq = iter.pop();
                table[from_raw as usize][to_sq.raw() as usize] = order;
                order += 1;
            }
        }
    }

    #[inline]
    fn get(&self, pattern: usize, from_sq: Square, to_sq: Square) -> u8 {
        self.data[pattern][from_sq.raw() as usize][to_sq.raw() as usize]
    }
}

/// AttackOrderTable の LazyLock キャッシュ
static ATTACK_ORDER_TABLE: LazyLock<AttackOrderTable> = LazyLock::new(AttackOrderTable::new);

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
    let attack_ord = ATTACK_ORDER_TABLE.get(pattern, from_sq_n, to_sq_n);
    debug_assert_ne!(
        attack_ord,
        AttackOrderTable::INVALID,
        "attack_order: to_sq {} is not attacked by pattern {pattern} at {}",
        to_sq_n.raw(),
        from_sq_n.raw()
    );
    base + from_off + attack_ord as usize
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
    let from_offset_table = &*FROM_OFFSET_TABLE;

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
                    from_offset_table,
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
// BonaPiece デコード（差分更新用）
// =============================================================================

/// BonaPiece (fb perspective) から盤上駒のマスを抽出する。
/// 手駒・ZERO は None。King も含む（占有ビット再構成用）。
fn decode_board_square_fb(bp: BonaPiece) -> Option<Square> {
    use super::bona_piece::{FE_END, FE_HAND_END};
    use super::bona_piece_halfka_hm::{E_KING, F_KING};

    let v = bp.value() as usize;
    if v == 0 {
        return None; // ZERO
    }
    // 通常盤上駒: FE_HAND_END (90) .. FE_END (1548)
    if (FE_HAND_END..FE_END).contains(&v) {
        let sq_raw = ((v - FE_HAND_END) % 81) as u8;
        return Square::from_u8(sq_raw);
    }
    // King: F_KING (1548) .. F_KING+81, E_KING (1629) .. E_KING+81
    if (F_KING..F_KING + 81).contains(&v) {
        return Square::from_u8((v - F_KING) as u8);
    }
    if (E_KING..E_KING + 81).contains(&v) {
        return Square::from_u8((v - E_KING) as u8);
    }
    // 手駒 (1..89)
    None
}

/// BonaPiece (fb perspective) から Threat 駒情報をデコードする。
/// King・手駒・ZERO は None。
fn decode_board_threat_info_fb(bp: BonaPiece) -> Option<(Color, ThreatClass, PieceType, Square)> {
    use super::bona_piece::{FE_END, FE_HAND_END};

    let v = bp.value() as usize;
    if !(FE_HAND_END..FE_END).contains(&v) {
        return None; // 手駒・ZERO・King
    }

    let offset = v - FE_HAND_END;
    let pair_index = offset / 81;
    let sq_raw = (offset % 81) as u8;
    let piece_type_idx = pair_index / 2;
    let is_enemy = pair_index % 2 == 1;

    /// BonaPiece pair index → ThreatClass のマッピング
    /// 順序: Pawn, Lance, Knight, Silver, Gold(Like), Bishop, Horse, Rook, Dragon
    const BP_TO_CLASS: [ThreatClass; 9] = [
        ThreatClass::Pawn,
        ThreatClass::Lance,
        ThreatClass::Knight,
        ThreatClass::Silver,
        ThreatClass::GoldLike,
        ThreatClass::Bishop,
        ThreatClass::Horse,
        ThreatClass::Rook,
        ThreatClass::Dragon,
    ];

    /// BonaPiece pair index → PieceType のマッピング（attack 計算用）
    /// Gold は ProPawn/ProLance 等と同じ gold_effect を使うので PieceType::Gold で統一
    const BP_TO_PT: [PieceType; 9] = [
        PieceType::Pawn,
        PieceType::Lance,
        PieceType::Knight,
        PieceType::Silver,
        PieceType::Gold,
        PieceType::Bishop,
        PieceType::Horse,
        PieceType::Rook,
        PieceType::Dragon,
    ];

    let class = BP_TO_CLASS[piece_type_idx];
    let pt = BP_TO_PT[piece_type_idx];
    // fb perspective: even pair = Black, odd pair = White
    let color = if is_enemy { Color::White } else { Color::Black };
    let sq = Square::from_u8(sq_raw)?;
    Some((color, class, pt, sq))
}

// =============================================================================
// append_changed_indices（Threat 差分生成）
// =============================================================================

/// 差分更新の dirty piece エントリ（before 状態の駒情報）
struct ThreatEntry {
    sq: Square,
    color: Color,
    class: ThreatClass,
    pt: PieceType,
}

/// Threat 差分の中間バッファサイズ
///
/// source squares (典型 10-30) × targets per source (典型 2-8) の上限。
/// debug build で実測し、オーバーフローが無いことを確認する。
const MAX_INTERMEDIATE_THREATS: usize = 512;

/// DirtyPiece から Threat 特徴量の差分（removed / added）を計算する。
///
/// ## 戻り値
///
/// - `true`: 差分計算成功
/// - `false`: 中間バッファまたは出力リストがオーバーフロー → 呼び出し元で full refresh が必要
///
/// ## アルゴリズム概要
///
/// 1. DirtyPiece から changed squares を抽出し、before_occ を再構成
/// 2. changed squares の attackers_to_occ で source squares を収集
///    （開き利きで変化した遠方駒も捕捉。
///    注意: `pos.attackers_to_occ` は after-state の piece bitboards を参照するが、
///    moved/captured 駒は changed_bb に含まれるため実害なし。）
/// 3. 各 source square の before/after threat pair を列挙
/// 4. ソート + マージで set difference → removed / added
pub fn append_changed_threat_indices(
    pos: &Position,
    dirty_piece: &DirtyPiece,
    perspective: Color,
    king_sq: Square,
    removed: &mut IndexList<MAX_CHANGED_THREAT_FEATURES>,
    added: &mut IndexList<MAX_CHANGED_THREAT_FEATURES>,
) -> bool {
    if dirty_piece.dirty_num == 0 {
        return true;
    }

    let hm = is_hm_mirror(king_sq, perspective);
    let from_offset_table = &*FROM_OFFSET_TABLE;
    let friend_color = perspective;
    let after_occ = pos.occupied();

    // ---------------------------------------------------------------
    // Step 1: DirtyPiece デコード → changed_bb, before_occ, 駒情報
    // ---------------------------------------------------------------

    // old/new の threat 駒情報（最大 dirty_num=2 なので固定長配列）
    let mut old_entries: [Option<ThreatEntry>; 2] = [None, None];
    let mut new_entries: [Option<ThreatEntry>; 2] = [None, None];
    let mut old_count = 0usize;
    let mut new_count = 0usize;

    // before_occ 再構成:
    // 捕獲手では to マスが old と new の両方に現れるため、
    // ループ内の |= / &= の順序で結果が変わる。
    // → old/new を Bitboard で一括収集し、順序非依存に再構成する。
    //
    //   before_occ = (after_occ | old_bb) & !(new_bb & !old_bb)
    //
    // new_bb & !old_bb = 「new にあるが old にない」マス = move 前は空だった。
    let mut old_bb = Bitboard::EMPTY;
    let mut new_bb = Bitboard::EMPTY;

    for i in 0..dirty_piece.dirty_num as usize {
        let cp = &dirty_piece.changed_piece[i];

        // old square: King を含めて占有復元、Threat 情報は非 King のみ
        if let Some(old_sq) = decode_board_square_fb(cp.old_piece.fb) {
            old_bb |= Bitboard::from_square(old_sq);
        }
        if let Some((color, class, pt, sq)) = decode_board_threat_info_fb(cp.old_piece.fb) {
            old_entries[old_count] = Some(ThreatEntry {
                sq,
                color,
                class,
                pt,
            });
            old_count += 1;
        }

        // new square: King を含めて占有復元、Threat 情報は非 King のみ
        if let Some(new_sq) = decode_board_square_fb(cp.new_piece.fb) {
            new_bb |= Bitboard::from_square(new_sq);
        }
        if let Some((color, class, pt, sq)) = decode_board_threat_info_fb(cp.new_piece.fb) {
            new_entries[new_count] = Some(ThreatEntry {
                sq,
                color,
                class,
                pt,
            });
            new_count += 1;
        }
    }

    let changed_bb = old_bb | new_bb;
    // new_only = new にあるが old にないマス（move 前は空だった）
    let new_only = new_bb & !old_bb;
    let before_occ = (after_occ | old_bb) & !new_only;

    // ---------------------------------------------------------------
    // Step 2: Source squares 収集
    // ---------------------------------------------------------------

    let mut source_bb = changed_bb;
    let mut ch_iter = changed_bb;
    while !ch_iter.is_empty() {
        let sq = ch_iter.pop();
        source_bb |= pos.attackers_to_occ(sq, before_occ);
        source_bb |= pos.attackers_to_occ(sq, after_occ);
    }

    // ---------------------------------------------------------------
    // Step 3: Before / After の threat index 列挙
    // ---------------------------------------------------------------

    let mut before_buf = [0u32; MAX_INTERMEDIATE_THREATS];
    let mut after_buf = [0u32; MAX_INTERMEDIATE_THREATS];
    let mut before_len = 0usize;
    let mut after_len = 0usize;
    let mut overflow = false;

    let mut src_iter = source_bb;
    while !src_iter.is_empty() {
        let sq_s = src_iter.pop();

        // --- Before 状態の threat 列挙 ---
        if let Some(info) =
            lookup_piece_before(sq_s, &old_entries, old_count, &new_entries, new_count, pos)
        {
            let attacker_side = if info.color == friend_color { 0 } else { 1 };
            let oriented_color = if perspective == Color::Black {
                info.color
            } else {
                !info.color
            };
            let attacks = attacks_from_piece(info.pt, info.color, sq_s, before_occ);
            let mut targets = attacks & before_occ;
            while !targets.is_empty() {
                let to_sq = targets.pop();
                if let Some(t) = lookup_piece_before(
                    to_sq,
                    &old_entries,
                    old_count,
                    &new_entries,
                    new_count,
                    pos,
                ) {
                    let attacked_side = if t.color == friend_color { 0 } else { 1 };
                    let from_sq_n = normalize_sq(sq_s, perspective, hm);
                    let to_sq_n = normalize_sq(to_sq, perspective, hm);
                    let idx = threat_index(
                        attacker_side,
                        info.class,
                        oriented_color,
                        attacked_side,
                        t.class,
                        from_sq_n,
                        to_sq_n,
                        from_offset_table,
                    );
                    if before_len >= MAX_INTERMEDIATE_THREATS {
                        overflow = true;
                        break;
                    }
                    before_buf[before_len] = idx as u32;
                    before_len += 1;
                }
            }
        }
        if overflow {
            break;
        }

        // --- After 状態の threat 列挙 ---
        let pc = pos.piece_on(sq_s);
        if !pc.is_none() {
            let pt = pc.piece_type();
            if let Some(class) = ThreatClass::from_piece_type(pt) {
                let color = pc.color();
                let attacker_side = if color == friend_color { 0 } else { 1 };
                let oriented_color = if perspective == Color::Black {
                    color
                } else {
                    !color
                };
                let attacks = attacks_from_piece(pt, color, sq_s, after_occ);
                let mut targets = attacks & after_occ;
                while !targets.is_empty() {
                    let to_sq = targets.pop();
                    let target_pc = pos.piece_on(to_sq);
                    if target_pc.is_none() {
                        continue;
                    }
                    let target_pt = target_pc.piece_type();
                    if let Some(target_class) = ThreatClass::from_piece_type(target_pt) {
                        let target_color = target_pc.color();
                        let attacked_side = if target_color == friend_color { 0 } else { 1 };
                        let from_sq_n = normalize_sq(sq_s, perspective, hm);
                        let to_sq_n = normalize_sq(to_sq, perspective, hm);
                        let idx = threat_index(
                            attacker_side,
                            class,
                            oriented_color,
                            attacked_side,
                            target_class,
                            from_sq_n,
                            to_sq_n,
                            from_offset_table,
                        );
                        if after_len >= MAX_INTERMEDIATE_THREATS {
                            overflow = true;
                            break;
                        }
                        after_buf[after_len] = idx as u32;
                        after_len += 1;
                    }
                }
            }
        }
        if overflow {
            break;
        }
    }

    if overflow {
        return false; // 呼び出し元で full refresh が必要
    }

    // ---------------------------------------------------------------
    // Step 4: ソート + マージで set difference → removed / added
    // ---------------------------------------------------------------

    before_buf[..before_len].sort_unstable();
    after_buf[..after_len].sort_unstable();

    let mut bi = 0;
    let mut ai = 0;
    while bi < before_len && ai < after_len {
        let bv = before_buf[bi];
        let av = after_buf[ai];
        if bv < av {
            if !removed.push(bv as usize) {
                return false;
            }
            bi += 1;
        } else if bv > av {
            if !added.push(av as usize) {
                return false;
            }
            ai += 1;
        } else {
            // 同一 index: 変化なし
            bi += 1;
            ai += 1;
        }
    }
    while bi < before_len {
        if !removed.push(before_buf[bi] as usize) {
            return false;
        }
        bi += 1;
    }
    while ai < after_len {
        if !added.push(after_buf[ai] as usize) {
            return false;
        }
        ai += 1;
    }
    true
}

/// before 状態でのマス sq の駒情報を返す。
///
/// King は Threat 対象外のため、King のマスでは常に None を返す。
///
/// 優先順位:
/// 1. old_entries に sq がある → before 状態の駒情報（捕獲された駒等）
/// 2. new_entries に sq がある → before 状態では空（駒が移動してきた場所）
/// 3. いずれでもない → current Position から取得（変化していない駒）
struct PieceInfoBefore {
    color: Color,
    class: ThreatClass,
    pt: PieceType,
}

#[inline]
fn lookup_piece_before(
    sq: Square,
    old_entries: &[Option<ThreatEntry>; 2],
    old_count: usize,
    new_entries: &[Option<ThreatEntry>; 2],
    new_count: usize,
    pos: &Position,
) -> Option<PieceInfoBefore> {
    // old_entries に sq がある → before 状態の駒
    for i in 0..old_count {
        if let Some(ref e) = old_entries[i]
            && e.sq == sq
        {
            return Some(PieceInfoBefore {
                color: e.color,
                class: e.class,
                pt: e.pt,
            });
        }
    }
    // new_entries に sq がある → before 状態では空
    for i in 0..new_count {
        if let Some(ref e) = new_entries[i]
            && e.sq == sq
        {
            return None;
        }
    }
    // 変化なし → current Position から取得
    let pc = pos.piece_on(sq);
    if pc.is_none() {
        return None;
    }
    let pt = pc.piece_type();
    ThreatClass::from_piece_type(pt).map(|class| PieceInfoBefore {
        color: pc.color(),
        class,
        pt,
    })
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
            .map(|sq| attacks_count(ThreatClass::Pawn, Square::from_u8(sq).unwrap()))
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
            .map(|sq| attacks_count(ThreatClass::Rook, Square::from_u8(sq).unwrap()))
            .sum();
        assert_eq!(total, 1296);
    }

    #[test]
    fn test_attacks_per_color_totals() {
        for (class_id, &expected) in ATTACKS_PER_COLOR.iter().enumerate() {
            let class = ALL_THREAT_CLASSES[class_id];
            let total: usize =
                (0..81u8).map(|sq| attacks_count(class, Square::from_u8(sq).unwrap())).sum();
            assert_eq!(
                total, expected,
                "ThreatClass {:?}: expected {expected}, got {total}",
                class
            );
        }
    }

    /// White 方向性駒の from_offset が Black と異なることを検証
    #[test]
    fn test_white_directional_lut_differs_from_black() {
        for class in &ALL_THREAT_CLASSES {
            if !is_directional(*class) {
                // 非方向性駒は色不問で同一
                let b = compute_from_offset_colored(*class, Color::Black);
                let w = compute_from_offset_colored(*class, Color::White);
                assert_eq!(b, w, "{class:?} is non-directional but offsets differ");
                continue;
            }
            let b = compute_from_offset_colored(*class, Color::Black);
            let w = compute_from_offset_colored(*class, Color::White);
            assert_ne!(b, w, "{class:?} is directional but Black == White offsets");

            // 合計攻撃数は同じ（対称性）
            let total_b: usize = (0..81u8)
                .map(|sq| {
                    attacks_bb_colored(*class, Color::Black, Square::from_u8(sq).unwrap()).count()
                        as usize
                })
                .sum();
            let total_w: usize = (0..81u8)
                .map(|sq| {
                    attacks_bb_colored(*class, Color::White, Square::from_u8(sq).unwrap()).count()
                        as usize
                })
                .sum();
            assert_eq!(
                total_b, total_w,
                "{class:?}: Black total {total_b} != White total {total_w}"
            );
        }
    }

    /// White Pawn の from_offset 固定値テスト
    #[test]
    fn test_from_offset_white_pawn() {
        let offsets = compute_from_offset_colored(ThreatClass::Pawn, Color::White);
        // White Pawn: rank=8 → 0 attacks, rank<8 → 1 attack（後手歩は rank 増加方向）
        // sq=0 (file=0, rank=0): attacks=1 (rank 0→1 に攻撃)
        // sq=8 (file=0, rank=8): attacks=0 (最奥段)
        assert_eq!(offsets[0], 0);
        assert_eq!(offsets[1], 1); // sq=0 has 1 attack
        assert_eq!(offsets[8], 8); // sq=0..7 で 8 attacks 累積 (各 rank 0..7 が 1 attack)
        // sq=8 (rank=8) has 0 attacks
        assert_eq!(offsets[9], 8); // sq=8 は 0 attack なので累積変わらず
    }

    #[test]
    fn test_attack_order_rook_center() {
        // Rook at sq=40 (5五): 攻撃先を raw 昇順で列挙
        let sq = Square::from_u8(40).unwrap();
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
        for &class in &ALL_THREAT_CLASSES {
            for &oriented_color in &[Color::Black, Color::White] {
                for sq_raw in 0..81u8 {
                    let sq = Square::from_u8(sq_raw).unwrap();
                    let bb = attacks_bb_colored(class, oriented_color, sq);
                    let mut iter = bb;
                    while !iter.is_empty() {
                        let to = iter.pop();
                        for as_ in 0..2 {
                            for ds in 0..2 {
                                for &dc_class in &ALL_THREAT_CLASSES {
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

    // =========================================================================
    // 差分更新テスト
    // =========================================================================

    use super::super::accumulator::IndexList;
    use crate::types::Move;

    /// 差分更新の結果が full refresh と一致することを検証するヘルパー。
    ///
    /// 1. before の局面で active indices を取得
    /// 2. 指し手を実行し、DirtyPiece を取得
    /// 3. append_changed_threat_indices で removed/added を取得
    /// 4. before_set - removed + added = after_set であることを検証
    fn verify_incremental(pos: &mut Position, m: Move) {
        for &perspective in &[Color::Black, Color::White] {
            let king_sq_before = pos.king_square(perspective);

            // HM mirror 境界を跨ぐ場合は full refresh が必要なのでスキップ
            let hm_before = is_hm_mirror(king_sq_before, perspective);

            let mut before_indices = Vec::new();
            append_active_threat_indices(pos, perspective, king_sq_before, &mut before_indices);
            before_indices.sort();

            let gc = pos.gives_check(m);
            let dirty = pos.do_move(m, gc);

            let king_sq_after = pos.king_square(perspective);
            let hm_after = is_hm_mirror(king_sq_after, perspective);

            if hm_before != hm_after {
                // HM boundary crossed → full refresh needed, skip incremental test
                pos.undo_move(m);
                return;
            }

            let mut after_indices = Vec::new();
            append_active_threat_indices(pos, perspective, king_sq_after, &mut after_indices);
            after_indices.sort();

            let mut removed = IndexList::<MAX_CHANGED_THREAT_FEATURES>::new();
            let mut added = IndexList::<MAX_CHANGED_THREAT_FEATURES>::new();
            let ok = append_changed_threat_indices(
                pos,
                &dirty,
                perspective,
                king_sq_after,
                &mut removed,
                &mut added,
            );
            assert!(
                ok,
                "append_changed_threat_indices overflow (perspective={perspective:?}, move={m:?})"
            );

            // before_set - removed + added = after_set
            let removed_set: Vec<usize> = removed.iter().collect();
            let added_set: Vec<usize> = added.iter().collect();

            let mut computed = before_indices.clone();
            for &r in &removed_set {
                let pos_found = computed.iter().position(|&x| x == r);
                assert!(
                    pos_found.is_some(),
                    "removed index {r} not found in before_set (perspective={perspective:?}, move={m:?})"
                );
                computed.remove(pos_found.unwrap());
            }
            for &a in &added_set {
                computed.push(a);
            }
            computed.sort();

            assert_eq!(
                computed, after_indices,
                "Incremental mismatch for perspective={perspective:?}, move={m:?}\n\
                 before={before_indices:?}\n\
                 removed={removed_set:?}\n\
                 added={added_set:?}\n\
                 computed={computed:?}\n\
                 expected={after_indices:?}"
            );

            pos.undo_move(m);
        }
    }

    /// 初手 7六歩（通常手）の差分更新テスト
    #[test]
    fn test_changed_indices_pawn_push() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");
        let m = Move::from_usi("7g7f").expect("7g7f");
        verify_incremental(&mut pos, m);
    }

    /// 角道を開けた後の角交換（取る手）の差分更新テスト
    #[test]
    fn test_changed_indices_capture() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");
        // 7六歩 → 3四歩 → 8八角から2二角成（角交換）
        for mv_str in &["7g7f", "3c3d"] {
            let m = Move::from_usi(mv_str).unwrap();
            let gc = pos.gives_check(m);
            pos.do_move(m, gc);
        }
        let m = Move::from_usi("8h2b+").expect("8h2b+");
        verify_incremental(&mut pos, m);
    }

    /// 駒打ち（手駒から打つ手）の差分更新テスト
    #[test]
    fn test_changed_indices_drop() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");
        // 角交換で手駒を作る: 7六歩 → 3四歩 → 8八角から2二角成 → 同銀
        for mv_str in &["7g7f", "3c3d", "8h2b+", "3a2b"] {
            let m = Move::from_usi(mv_str).unwrap();
            let gc = pos.gives_check(m);
            pos.do_move(m, gc);
        }
        // 先手の手駒に角がある状態。角を打つ。
        let m = Move::from_usi("B*5e").expect("B*5e");
        verify_incremental(&mut pos, m);
    }

    /// 玉が動く手（開き利き変化）の差分更新テスト
    #[test]
    fn test_changed_indices_king_move() {
        let mut pos = Position::new();
        // 玉が動ける局面（5九の王が 4八 or 6八 に移動可能）
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1BK4R1/LNSG1GSNL b - 1")
            .expect("sfen");
        let m = Move::from_usi("7h6h").expect("7h6h");
        verify_incremental(&mut pos, m);
    }

    /// 複数手を連続で差分更新して正当性を検証
    #[test]
    fn test_changed_indices_sequence() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");

        let move_sequence = ["7g7f", "3c3d", "2g2f", "8c8d", "2f2e", "8d8e"];
        for mv_str in &move_sequence {
            let m = Move::from_usi(mv_str).expect(mv_str);
            verify_incremental(&mut pos, m);
            let gc = pos.gives_check(m);
            pos.do_move(m, gc);
        }
    }

    /// BonaPiece デコードのテスト
    #[test]
    fn test_decode_board_square_fb() {
        use super::super::bona_piece::{E_PAWN, F_PAWN};
        use super::super::bona_piece_halfka_hm::{E_KING, F_KING};

        // ZERO → None
        assert!(decode_board_square_fb(BonaPiece::ZERO).is_none());

        // 手駒 → None
        assert!(decode_board_square_fb(BonaPiece::new(1)).is_none());

        // F_PAWN + 0 = sq=0
        let bp = BonaPiece::new(F_PAWN);
        assert_eq!(decode_board_square_fb(bp), Some(Square::from_u8(0).unwrap()));

        // E_PAWN + 40 = sq=40
        let bp = BonaPiece::new(E_PAWN + 40);
        assert_eq!(decode_board_square_fb(bp), Some(Square::from_u8(40).unwrap()));

        // F_KING + 10
        let bp = BonaPiece::new(F_KING as u16 + 10);
        assert_eq!(decode_board_square_fb(bp), Some(Square::from_u8(10).unwrap()));

        // E_KING + 40
        let bp = BonaPiece::new(E_KING as u16 + 40);
        assert_eq!(decode_board_square_fb(bp), Some(Square::from_u8(40).unwrap()));
    }

    /// BonaPiece から Threat 情報デコードのテスト
    #[test]
    fn test_decode_board_threat_info_fb() {
        use super::super::bona_piece::{E_HORSE, E_ROOK, F_HORSE, F_PAWN, F_ROOK};
        use super::super::bona_piece_halfka_hm::F_KING;

        // F_PAWN + 40: Black Pawn at sq=40
        let bp = BonaPiece::new(F_PAWN + 40);
        let (color, class, pt, sq) = decode_board_threat_info_fb(bp).unwrap();
        assert_eq!(color, Color::Black);
        assert_eq!(class, ThreatClass::Pawn);
        assert_eq!(pt, PieceType::Pawn);
        assert_eq!(sq.raw(), 40);

        // Horse/Rook の並び順確認（BonaPiece では Horse が Rook より前）
        // F_HORSE: Black Horse
        let bp = BonaPiece::new(F_HORSE + 20);
        let (color, class, pt, sq) = decode_board_threat_info_fb(bp).unwrap();
        assert_eq!(color, Color::Black);
        assert_eq!(class, ThreatClass::Horse);
        assert_eq!(pt, PieceType::Horse);
        assert_eq!(sq.raw(), 20);

        // E_HORSE: White Horse
        let bp = BonaPiece::new(E_HORSE + 5);
        let (color, class, pt, sq) = decode_board_threat_info_fb(bp).unwrap();
        assert_eq!(color, Color::White);
        assert_eq!(class, ThreatClass::Horse);
        assert_eq!(pt, PieceType::Horse);
        assert_eq!(sq.raw(), 5);

        // F_ROOK: Black Rook
        let bp = BonaPiece::new(F_ROOK + 10);
        let (color, class, pt, sq) = decode_board_threat_info_fb(bp).unwrap();
        assert_eq!(color, Color::Black);
        assert_eq!(class, ThreatClass::Rook);
        assert_eq!(pt, PieceType::Rook);
        assert_eq!(sq.raw(), 10);

        // E_ROOK: White Rook
        let bp = BonaPiece::new(E_ROOK);
        let (color, class, pt, sq) = decode_board_threat_info_fb(bp).unwrap();
        assert_eq!(color, Color::White);
        assert_eq!(class, ThreatClass::Rook);
        assert_eq!(pt, PieceType::Rook);
        assert_eq!(sq.raw(), 0);

        // King → None
        let bp = BonaPiece::new(F_KING as u16);
        assert!(decode_board_threat_info_fb(bp).is_none());

        // ZERO → None
        assert!(decode_board_threat_info_fb(BonaPiece::ZERO).is_none());
    }

    /// 非取り成り手の差分更新テスト
    #[test]
    fn test_changed_indices_promotion_no_capture() {
        let mut pos = Position::new();
        // 歩が 3 段目にいて成れる局面
        pos.set_sfen("lnsgkgsnl/1r5b1/pppppp1pp/9/9/6P2/PPPPPP1PP/1B5R1/LNSGKGSNL b - 1")
            .expect("sfen");
        // 3四歩 → 3三歩成
        for mv_str in &["3f3e", "1c1d", "3e3d", "1d1e"] {
            let m = Move::from_usi(mv_str).unwrap();
            let gc = pos.gives_check(m);
            pos.do_move(m, gc);
        }
        let m = Move::from_usi("3d3c+").expect("3d3c+");
        verify_incremental(&mut pos, m);
    }

    /// 取り成り手の差分更新テスト
    #[test]
    fn test_changed_indices_promotion_with_capture() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");
        // 角交換: 7六歩 → 3四歩 → 2二角成（成り + 取り）
        for mv_str in &["7g7f", "3c3d"] {
            let m = Move::from_usi(mv_str).unwrap();
            let gc = pos.gives_check(m);
            pos.do_move(m, gc);
        }
        // 8八角 → 2二角成 = 成りかつ取り
        let m = Move::from_usi("8h2b+").expect("8h2b+");
        verify_incremental(&mut pos, m);
    }

    /// 後手番の指し手に対する差分更新テスト
    #[test]
    fn test_changed_indices_white_to_move() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");
        // 先手 7六歩
        let m = Move::from_usi("7g7f").unwrap();
        let gc = pos.gives_check(m);
        pos.do_move(m, gc);
        // 後手 3四歩
        let m = Move::from_usi("3c3d").expect("3c3d");
        verify_incremental(&mut pos, m);
    }

    /// 開き利き（スライダーの blocker が外れるケース）の差分更新テスト
    ///
    /// 飛車の前に歩がいて、歩が横に動くと飛車の利きが通る局面。
    #[test]
    fn test_changed_indices_discovered_attack() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");
        // 2六歩 → 8四歩 → 2五歩 → 8五歩: 飛車の前の歩が進んで利きが延びる
        let moves = ["2g2f", "8c8d", "2f2e", "8d8e"];
        for mv_str in &moves {
            let m = Move::from_usi(mv_str).expect(mv_str);
            verify_incremental(&mut pos, m);
            let gc = pos.gives_check(m);
            pos.do_move(m, gc);
        }
    }

    /// 取り返しの連続手の差分更新テスト
    #[test]
    fn test_changed_indices_recapture_sequence() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");
        // 角交換: 7六歩 → 3四歩 → 2二角成 → 同銀 → B*6e
        let moves = ["7g7f", "3c3d", "8h2b+", "3a2b"];
        for mv_str in &moves {
            let m = Move::from_usi(mv_str).unwrap();
            verify_incremental(&mut pos, m);
            let gc = pos.gives_check(m);
            pos.do_move(m, gc);
        }
    }

    /// AttackOrderTable と compute_attack_order_colored の一致テスト
    #[test]
    fn test_attack_order_table_matches_compute() {
        let table = &*ATTACK_ORDER_TABLE;
        for &class in &ALL_THREAT_CLASSES {
            for &color in &[Color::Black, Color::White] {
                if !is_directional(class) && color == Color::White {
                    continue; // 非方向性駒は Black パターンのみ
                }
                let pattern = attack_pattern_id(class, color);
                for from_raw in 0..81u8 {
                    let from_sq = Square::from_u8(from_raw).unwrap();
                    let bb = attacks_bb_colored(class, color, from_sq);
                    let mut iter = bb;
                    while !iter.is_empty() {
                        let to_sq = iter.pop();
                        let expected = compute_attack_order_colored(class, color, from_sq, to_sq);
                        let got = table.get(pattern, from_sq, to_sq);
                        assert_eq!(
                            got,
                            expected as u8,
                            "Mismatch: {class:?} {color:?} from={} to={}",
                            from_raw,
                            to_sq.raw()
                        );
                    }
                }
            }
        }
    }
}
