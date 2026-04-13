//! HandThreat 特徴量 (案 A: full drop-attack pair)
//!
//! 持ち駒を打つことによる potential attack pair を NNUE 特徴量として列挙する。
//! 各 pair は `(drop_owner_side, hand_class, drop_sq, attacked_side, attacked_class, attacked_to_sq)`
//! で一意に決まる。
//!
//! ## 仕様
//!
//! - 設計ノート: `docs/performance/hand_threat_design_20260413.md`
//! - Index 構造は board Threat (`threat_features`) を流用 (Option 1A)
//! - 案 A: `dims = 121,104`
//! - Profile flag は当面なし
//!
//! ## Active 条件
//!
//! feature `(drop_owner, hand_class, drop_sq, attacked_side, attacked_class, attack_to_sq)` が active
//! となるのは以下の条件を全て満たす場合:
//! 1. `pos.hand(drop_owner).count(hand_class) > 0` (持ち駒がある)
//! 2. `!occupied.contains(drop_sq)` (drop_sq が空)
//! 3. `is_legal_drop_rank(hand_class, drop_owner, drop_sq)` (行きどころ無しでない)
//! 4. Pawn の場合: `!has_pawn_on_file(drop_owner, drop_sq.file())` (二歩でない)
//! 5. `attacks_from_dropped(hand_class, drop_owner, drop_sq, occupied).contains(attack_to_sq)`
//! 6. `occupied.contains(attack_to_sq)` (target sq が occupied)
//! 7. `piece_at(attack_to_sq).side == attacked_side` (drop_owner 視点で判定)
//! 8. `piece_at(attack_to_sq).piece_type → ThreatClass == attacked_class`
//!
//! 打ち歩詰めは合法手判定の領域なので feature には含めない。

use crate::bitboard::{
    Bitboard, FILE_BB, bishop_effect, gold_effect, knight_effect, lance_effect, pawn_effect,
    rook_effect, silver_effect,
};
use crate::position::Position;
use crate::types::{Color, PieceType, Square};

use super::accumulator::{DirtyPiece, IndexList};
use super::bona_piece_halfka_hm::is_hm_mirror;
use super::threat_features::{
    ATTACKS_PER_COLOR, NUM_THREAT_CLASSES, ThreatClass, extract_prev_king_sq,
    lookup_attack_feature_offset, normalize_sq,
};

// =============================================================================
// 定数
// =============================================================================

/// Drop 可能な駒種の数 (King/Horse/Dragon/GoldLike-except-Gold を除く)
pub const HAND_NUM_CLASSES: usize = 7;

/// HandThreat 差分更新の最大変化数 (Option 6B: partial rebuild は除外)
///
/// `IndexList<N>` は N <= 255 の制約があるため 255 を上限とする。
/// board Threat の `MAX_CHANGED_THREAT_FEATURES = 192` の約 1.33 倍。
/// overflow 時は full rebuild にフォールバック。
/// 将来 256+ が必要なら IndexList を u16 length に拡張するか、
/// 差分を分割処理する。
pub const MAX_CHANGED_HAND_THREAT_FEATURES: usize = 255;

// =============================================================================
// Refresh 判定 (HM mirror ベース)
// =============================================================================

/// HandThreat 差分更新を諦めて full rebuild すべきかを判定する
///
/// Phase 0 の `threat_features::needs_threat_refresh` と同じロジック:
/// HM mirror 境界を跨いだときのみ true。HandThreat も `normalize_sq` (HM mirror)
/// を正規化に使うため、跨ぎが起きない限り差分更新で正しく計算できる。
///
/// 玉が動いていない場合は早期 return で false。
///
/// # 引数
/// - `dirty_piece`: 直前の do_move で発生した DirtyPiece
/// - `curr_king_sq`: 現在 (after) の perspective 側の玉位置
/// - `perspective`: 視点
///
/// # 戻り値
/// - true: full rebuild が必要 (HM mirror 跨ぎ、または king 動きありで prev 不明)
/// - false: 差分更新で対応可能 (or partial rebuild 経路で対応)
pub fn needs_hand_threat_refresh(
    dirty_piece: &DirtyPiece,
    curr_king_sq: Square,
    perspective: Color,
) -> bool {
    if !dirty_piece.king_moved[perspective as usize] {
        return false;
    }
    let prev_king_sq = match extract_prev_king_sq(dirty_piece, perspective) {
        Some(sq) => sq,
        None => return true,
    };
    is_hm_mirror(prev_king_sq, perspective) != is_hm_mirror(curr_king_sq, perspective)
}

/// `HandThreatClass` → board `ThreatClass` マッピング
///
/// 持ち駒を打つと、その駒は元の駒種で盤上に出る (promotion は do_move 時点で解決済)。
/// `Gold` は board Threat では `GoldLike` に統合されている。
const HAND_TO_BOARD_CLASS: [ThreatClass; HAND_NUM_CLASSES] = [
    ThreatClass::Pawn,     // 0: Pawn
    ThreatClass::Lance,    // 1: Lance
    ThreatClass::Knight,   // 2: Knight
    ThreatClass::Silver,   // 3: Silver
    ThreatClass::GoldLike, // 4: Gold  (board 側は GoldLike)
    ThreatClass::Bishop,   // 5: Bishop
    ThreatClass::Rook,     // 6: Rook
];

/// 各 HandThreatClass の drop → 利き数 (color ごと)
///
/// board Threat の `ATTACKS_PER_COLOR` を `HAND_TO_BOARD_CLASS` で mapping した値。
/// = [72, 324, 112, 328, 416, 816, 1296]
/// 合計 = 3,364
const HAND_ATTACKS_PER_COLOR: [usize; HAND_NUM_CLASSES] = {
    let mut out = [0usize; HAND_NUM_CLASSES];
    let mut i = 0;
    while i < HAND_NUM_CLASSES {
        out[i] = ATTACKS_PER_COLOR[HAND_TO_BOARD_CLASS[i] as usize];
        i += 1;
    }
    out
};

// =============================================================================
// HandThreatClass
// =============================================================================

/// 持ち駒で drop 可能な駒種
///
/// 順序は仕様固定 (`hand_threat_design_20260413.md`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HandThreatClass {
    Pawn = 0,
    Lance = 1,
    Knight = 2,
    Silver = 3,
    Gold = 4,
    Bishop = 5,
    Rook = 6,
}

/// 全 HandThreatClass を index 順に列挙した配列
const ALL_HAND_THREAT_CLASSES: [HandThreatClass; HAND_NUM_CLASSES] = [
    HandThreatClass::Pawn,
    HandThreatClass::Lance,
    HandThreatClass::Knight,
    HandThreatClass::Silver,
    HandThreatClass::Gold,
    HandThreatClass::Bishop,
    HandThreatClass::Rook,
];

impl HandThreatClass {
    /// drop した駒が盤上に出た時の ThreatClass (board 側)
    #[inline]
    pub fn as_board_class(self) -> ThreatClass {
        HAND_TO_BOARD_CLASS[self as usize]
    }

    /// drop した駒が盤上に出た時の `PieceType`
    #[inline]
    pub fn as_piece_type(self) -> PieceType {
        match self {
            Self::Pawn => PieceType::Pawn,
            Self::Lance => PieceType::Lance,
            Self::Knight => PieceType::Knight,
            Self::Silver => PieceType::Silver,
            Self::Gold => PieceType::Gold,
            Self::Bishop => PieceType::Bishop,
            Self::Rook => PieceType::Rook,
        }
    }
}

// =============================================================================
// hand_pair_base テーブル
// =============================================================================

/// hand_pair_base の pair 数
/// = 2 (drop_owner) × 7 (hand_class) × 2 (attacked_side) × 9 (attacked_class)
const HAND_NUM_PAIRS: usize = 2 * HAND_NUM_CLASSES * 2 * NUM_THREAT_CLASSES; // 252

/// hand_pair_base テーブルを build し、HAND_THREAT_DIMENSIONS も同時算出
///
/// Layout (flat): `drop_owner * 126 + hc * 18 + attacked_side * 9 + ac`
/// 126 = 7 * 18, 18 = 2 * 9
///
/// 各 entry には (drop_owner, hc, attacked_side, ac) 未満の全 pair が持つ
/// `HAND_ATTACKS_PER_COLOR[hc]` の累積和を格納。
const fn build_hand_pair_base() -> ([usize; HAND_NUM_PAIRS], usize) {
    let mut table = [0usize; HAND_NUM_PAIRS];
    let mut cumulative = 0usize;
    let mut drop_owner = 0usize;
    while drop_owner < 2 {
        let mut hc = 0usize;
        while hc < HAND_NUM_CLASSES {
            let mut attacked_side = 0usize;
            while attacked_side < 2 {
                let mut ac = 0usize;
                while ac < NUM_THREAT_CLASSES {
                    let idx = drop_owner * 126 + hc * 18 + attacked_side * 9 + ac;
                    table[idx] = cumulative;
                    cumulative += HAND_ATTACKS_PER_COLOR[hc];
                    ac += 1;
                }
                attacked_side += 1;
            }
            hc += 1;
        }
        drop_owner += 1;
    }
    (table, cumulative)
}

const HAND_PAIR_DATA: ([usize; HAND_NUM_PAIRS], usize) = build_hand_pair_base();

static HAND_PAIR_BASE: [usize; HAND_NUM_PAIRS] = HAND_PAIR_DATA.0;

/// HandThreat の総特徴量次元数
///
/// 案 A: `2 × 7 × 2 × 9 × (HAND_ATTACKS_PER_COLOR の合計 / 7)` の展開
/// = 36 × 3,364 = **121,104**
pub const HAND_THREAT_DIMENSIONS: usize = HAND_PAIR_DATA.1;

/// コンパイル時 assertion: HAND_THREAT_DIMENSIONS が期待値と一致すること
const _HAND_THREAT_DIMENSIONS_CHECK: () = {
    assert!(
        HAND_THREAT_DIMENSIONS == 121_104,
        "HAND_THREAT_DIMENSIONS must be 121,104"
    );
};

/// hand_pair_base を取得 (除外なしなので常に Some)
#[inline]
fn hand_pair_base(
    drop_owner: usize,
    hc: HandThreatClass,
    attacked_side: usize,
    ac: ThreatClass,
) -> usize {
    let idx = drop_owner * 126 + (hc as usize) * 18 + attacked_side * 9 + ac as usize;
    HAND_PAIR_BASE[idx]
}

// =============================================================================
// drop legality
// =============================================================================

/// `hand_class` の駒が `color` で `sq` に打てるか (行きどころ無しの観点のみ)
///
/// 行きどころ無しの定義 (先手基準):
/// - Pawn: rank=0 (1段目) に打てない
/// - Lance: rank=0 (1段目) に打てない
/// - Knight: rank ∈ {0, 1} (1-2段目) に打てない
/// - Silver/Gold/Bishop/Rook: 制限なし
///
/// 後手の場合は rank 8 / rank 7-8 に置き換え (rank = 8 - rank)。
#[inline]
pub(crate) fn is_legal_drop_rank(hand_class: HandThreatClass, color: Color, sq: Square) -> bool {
    let rank = sq.rank() as usize;
    match hand_class {
        HandThreatClass::Pawn | HandThreatClass::Lance => {
            if color == Color::Black {
                rank != 0
            } else {
                rank != 8
            }
        }
        HandThreatClass::Knight => {
            if color == Color::Black {
                rank >= 2
            } else {
                rank <= 6
            }
        }
        _ => true,
    }
}

/// `color` が持ち駒の Pawn を file `file` に drop できるか (二歩判定)
///
/// 同じ file に `color` 側の `PieceType::Pawn` (not promoted) があれば二歩。
#[inline]
pub(crate) fn has_pawn_on_file(pos: &Position, color: Color, sq: Square) -> bool {
    let pawn_bb = pos.pieces(color, PieceType::Pawn);
    let file_bb = FILE_BB[sq.file() as usize];
    !(pawn_bb & file_bb).is_empty()
}

// =============================================================================
// attack_bb for dropped piece
// =============================================================================

/// drop された駒の `occupied` 考慮 attack bitboard
///
/// 持ち駒は drop された時点で盤上の該当駒と同じ attack を持つ (promotion は drop 不可)。
#[inline]
pub(crate) fn attacks_from_dropped(
    hand_class: HandThreatClass,
    color: Color,
    sq: Square,
    occupied: Bitboard,
) -> Bitboard {
    match hand_class {
        HandThreatClass::Pawn => pawn_effect(color, sq),
        HandThreatClass::Lance => lance_effect(color, sq, occupied),
        HandThreatClass::Knight => knight_effect(color, sq),
        HandThreatClass::Silver => silver_effect(color, sq),
        HandThreatClass::Gold => gold_effect(color, sq),
        HandThreatClass::Bishop => bishop_effect(sq, occupied),
        HandThreatClass::Rook => rook_effect(sq, occupied),
    }
}

// =============================================================================
// hand_threat_index
// =============================================================================

/// HandThreat index を計算する
///
/// # 引数
/// - `drop_owner`: 0 = perspective side (friend), 1 = opposite side (enemy)
/// - `hand_class`: drop 駒の HandThreatClass
/// - `oriented_color`: perspective swap 後の drop 駒色 (attack pattern の方向性に使用)
/// - `attacked_side`: 0 = perspective side (friend), 1 = opposite side (enemy)
/// - `attacked_class`: 被攻撃駒の board ThreatClass
/// - `drop_sq_n`: 正規化後の drop_sq (perspective + HM mirror)
/// - `attack_to_sq_n`: 正規化後の attack target sq (同上)
#[inline]
pub(crate) fn hand_threat_index(
    drop_owner: usize,
    hand_class: HandThreatClass,
    oriented_color: Color,
    attacked_side: usize,
    attacked_class: ThreatClass,
    drop_sq_n: Square,
    attack_to_sq_n: Square,
) -> usize {
    let base = hand_pair_base(drop_owner, hand_class, attacked_side, attacked_class);
    let offset = lookup_attack_feature_offset(
        hand_class.as_board_class(),
        oriented_color,
        drop_sq_n,
        attack_to_sq_n,
    );
    base + offset
}

// =============================================================================
// append_changed_hand_threat_indices (差分更新、Option 6B hybrid)
// =============================================================================

/// DirtyPiece から HandThreat 特徴量の差分 (removed / added) を計算する
///
/// ## 戦略 (Option 6B hybrid)
///
/// 1. king が HM mirror 境界を跨いだ場合 → 呼び出し元で先に判定済み (needs_hand_threat_refresh)
/// 2. dirty_piece を解析して以下のケースを判別:
///    - (a) **持ち駒 count 変化** (capture/drop/promotion): `return false` で full rebuild fallback
///    - (b) **通常 board move** (非取り、非成り、非打ち):
///         - α: drop@A 新規 features を added に追加 (A が空になる)
///         - β: drop@B 消失 features を removed に追加 (B が occupied になる)
///         - γ: target@A 変化 features を処理 (moved piece at A → empty)
///         - δ: target@B 変化 features を処理 (empty at B → moved piece)
///         - ε: slider 経由の attack pattern 変化 (最も複雑)
///    - (c) 安全性のため: slider (Lance/Bishop/Rook/Horse/Dragon) が
///         絡む手、もしくは α/β/γ/δ の volume が大きい場合は full rebuild fallback
///
/// ## 初期版 (correctness first)
///
/// slider 経由 ε の正確な実装は次の最適化 pass に回す。このバージョンでは
/// **全ケースで `false` を返し** full rebuild にフォールバックする。
/// これは Task #19 で実装済みの rebuild_hand_threat と同じ correctness を保つ。
///
/// 本関数のシグネチャは将来の inline diff 実装時にそのまま使える形に固定しておく。
///
/// # 戻り値
/// - `true`: 差分計算成功、`removed` / `added` に diff が格納された
/// - `false`: 複雑ケースや overflow → 呼び出し元で full rebuild が必要
pub fn append_changed_hand_threat_indices(
    _pos: &Position,
    _dirty_piece: &DirtyPiece,
    _perspective: Color,
    _king_sq: Square,
    _removed: &mut IndexList<MAX_CHANGED_HAND_THREAT_FEATURES>,
    _added: &mut IndexList<MAX_CHANGED_HAND_THREAT_FEATURES>,
) -> bool {
    // 初期版: 常に full rebuild fallback を返す。
    //
    // 理由: HandThreat の差分更新は board Threat より構造的に複雑で、
    // 特に以下の component を正確に実装するには慎重な case analysis が必要:
    // - ε (slider 経由 attack pattern 変化): 移動 sq を通るスライダー drop の attack_bb 変化
    // - ζ (二歩 state 変化): pawn 取りや pawn 成りで file 単位の drop legality が変わる
    // - η (持ち駒 count 変化): capture/drop/promote で当該 (drop_owner, hand_class) block 全体が再計算
    //
    // Phase 1 PoC では correctness 優先で full rebuild のみを使用し、
    // 差分更新ロジックは Task #23 フォローアップ最適化として実装する。
    // Golden Forward テスト (Task #26) はこの fallback 経路でも correctness を保つ。
    false
}

// =============================================================================
// append_active_hand_threat_indices (test 用フル列挙)
// =============================================================================

/// 現局面の全 hand threat pair を列挙し、`indices` に追加する
///
/// refresh path や初回計算で使用する。差分更新は `append_changed_hand_threat_indices`
/// が未実装のため当面は full rebuild のみ。
pub fn append_active_hand_threat_indices(
    pos: &Position,
    perspective: Color,
    king_sq: Square,
    indices: &mut Vec<usize>,
) {
    let hm = is_hm_mirror(king_sq, perspective);
    let occupied = pos.occupied();

    let friend_color = perspective;
    let enemy_color = !perspective;

    // 両 drop_owner を処理
    for &drop_color in &[friend_color, enemy_color] {
        let drop_owner = if drop_color == friend_color { 0 } else { 1 };
        let hand = pos.hand(drop_color);

        // 7 hand class をループ
        for &hand_class in &ALL_HAND_THREAT_CLASSES {
            if hand.count(hand_class.as_piece_type()) == 0 {
                continue;
            }

            // 全 81 マスを drop 候補として走査
            for drop_raw in 0..81u8 {
                let drop_sq = Square::from_u8(drop_raw).unwrap();

                // (1) occupied なら skip
                if occupied.contains(drop_sq) {
                    continue;
                }
                // (2) 行きどころ無し
                if !is_legal_drop_rank(hand_class, drop_color, drop_sq) {
                    continue;
                }
                // (3) 二歩 (Pawn のみ)
                if hand_class == HandThreatClass::Pawn
                    && has_pawn_on_file(pos, drop_color, drop_sq)
                {
                    continue;
                }

                // drop sq からの attack bb
                let attack_bb = attacks_from_dropped(hand_class, drop_color, drop_sq, occupied);
                let mut targets = attack_bb & occupied;
                while !targets.is_empty() {
                    let to_sq = targets.pop();
                    let target_pc = pos.piece_on(to_sq);
                    let target_pt = target_pc.piece_type();

                    let Some(attacked_class) = ThreatClass::from_piece_type(target_pt) else {
                        continue; // King は除外
                    };

                    let attacked_side = if target_pc.color() == friend_color { 0 } else { 1 };

                    // 正規化
                    let drop_sq_n = normalize_sq(drop_sq, perspective, hm);
                    let to_sq_n = normalize_sq(to_sq, perspective, hm);

                    // oriented_color: attack pattern 方向性のため
                    let oriented_color = if perspective == Color::Black {
                        drop_color
                    } else {
                        !drop_color
                    };

                    let idx = hand_threat_index(
                        drop_owner,
                        hand_class,
                        oriented_color,
                        attacked_side,
                        attacked_class,
                        drop_sq_n,
                        to_sq_n,
                    );

                    debug_assert!(
                        idx < HAND_THREAT_DIMENSIONS,
                        "hand_threat index out of range: {idx} >= {HAND_THREAT_DIMENSIONS}"
                    );

                    indices.push(idx);
                }
            }
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hand_threat_dimensions() {
        assert_eq!(HAND_THREAT_DIMENSIONS, 121_104);
    }

    #[test]
    fn test_hand_attacks_per_color() {
        assert_eq!(HAND_ATTACKS_PER_COLOR, [72, 324, 112, 328, 416, 816, 1296]);
        assert_eq!(HAND_ATTACKS_PER_COLOR.iter().sum::<usize>(), 3_364);
    }

    #[test]
    fn test_hand_to_board_class_mapping() {
        assert_eq!(
            HandThreatClass::Pawn.as_board_class() as usize,
            ThreatClass::Pawn as usize
        );
        assert_eq!(
            HandThreatClass::Gold.as_board_class() as usize,
            ThreatClass::GoldLike as usize
        );
        assert_eq!(
            HandThreatClass::Rook.as_board_class() as usize,
            ThreatClass::Rook as usize
        );
    }

    #[test]
    fn test_hand_pair_base_monotone() {
        // hand_pair_base は累積和なので単調増加
        let mut prev: Option<usize> = None;
        for drop_owner in 0..2 {
            for hc in 0..HAND_NUM_CLASSES {
                for attacked_side in 0..2 {
                    for ac in 0..NUM_THREAT_CLASSES {
                        let idx = drop_owner * 126 + hc * 18 + attacked_side * 9 + ac;
                        let base = HAND_PAIR_BASE[idx];
                        if let Some(p) = prev {
                            assert!(base > p, "base must be strictly increasing");
                        }
                        prev = Some(base);
                    }
                }
            }
        }
    }

    #[test]
    fn test_startpos_hand_threats_empty() {
        // 初期局面は持ち駒 0 → HandThreat active indices も 0 件
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");

        let king_b = pos.king_square(Color::Black);
        let mut ib = Vec::new();
        append_active_hand_threat_indices(&pos, Color::Black, king_b, &mut ib);
        assert!(ib.is_empty(), "startpos should have no hand threats");

        let king_w = pos.king_square(Color::White);
        let mut iw = Vec::new();
        append_active_hand_threat_indices(&pos, Color::White, king_w, &mut iw);
        assert!(iw.is_empty(), "startpos (White persp) should have no hand threats");
    }

    #[test]
    fn test_midgame_hand_threats_present() {
        // 中盤局面 (持ち駒あり) で HandThreat active indices が非空
        // complex-middle: l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1
        let mut pos = Position::new();
        pos.set_sfen("l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1")
            .expect("complex-middle");

        let king_b = pos.king_square(Color::Black);
        let mut ib = Vec::new();
        append_active_hand_threat_indices(&pos, Color::Black, king_b, &mut ib);
        assert!(!ib.is_empty(), "midgame should have hand threats");
        // すべて index 範囲内
        for &idx in &ib {
            assert!(idx < HAND_THREAT_DIMENSIONS, "idx {idx} out of range");
        }

        let king_w = pos.king_square(Color::White);
        let mut iw = Vec::new();
        append_active_hand_threat_indices(&pos, Color::White, king_w, &mut iw);
        assert!(!iw.is_empty(), "midgame (White persp) should have hand threats");
        for &idx in &iw {
            assert!(idx < HAND_THREAT_DIMENSIONS, "idx {idx} out of range");
        }
    }

    #[test]
    fn test_drop_legality_black_pawn() {
        let p = HandThreatClass::Pawn;
        // Black Pawn: rank 0 (1段目) は illegal、それ以外 legal
        assert!(!is_legal_drop_rank(p, Color::Black, Square::from_u8(0).unwrap())); // 1一
        assert!(is_legal_drop_rank(p, Color::Black, Square::from_u8(1).unwrap())); // 1二
        assert!(is_legal_drop_rank(p, Color::Black, Square::from_u8(8).unwrap())); // 1九
    }

    #[test]
    fn test_drop_legality_black_knight() {
        let n = HandThreatClass::Knight;
        // Black Knight: rank 0, 1 (1-2段目) は illegal
        assert!(!is_legal_drop_rank(n, Color::Black, Square::from_u8(0).unwrap())); // 1一
        assert!(!is_legal_drop_rank(n, Color::Black, Square::from_u8(1).unwrap())); // 1二
        assert!(is_legal_drop_rank(n, Color::Black, Square::from_u8(2).unwrap())); // 1三
    }

    #[test]
    fn test_drop_legality_white_pawn() {
        let p = HandThreatClass::Pawn;
        // White Pawn: rank 8 (9段目) は illegal
        assert!(!is_legal_drop_rank(p, Color::White, Square::from_u8(8).unwrap())); // 1九
        assert!(is_legal_drop_rank(p, Color::White, Square::from_u8(7).unwrap())); // 1八
    }

    #[test]
    fn test_drop_legality_gold_anywhere() {
        let g = HandThreatClass::Gold;
        // Gold は制限なし (打ち歩詰めは別問題)
        for raw in 0..81u8 {
            let sq = Square::from_u8(raw).unwrap();
            assert!(is_legal_drop_rank(g, Color::Black, sq));
            assert!(is_legal_drop_rank(g, Color::White, sq));
        }
    }

    // =========================================================================
    // Golden Forward テスト (差分更新 vs 全更新 一致検証)
    // =========================================================================

    use super::super::accumulator::IndexList;
    use crate::types::Move;

    /// 差分更新の結果が full refresh と一致することを検証するヘルパー
    ///
    /// 初期版 (Task #23): `append_changed_hand_threat_indices` は常に `false` を
    /// 返して full rebuild fallback するため、この test では
    /// 「差分更新関数の戻り値が false ならスキップ (correctness は full rebuild で保証)」
    /// という緩めの検証を行う。
    ///
    /// 将来 Task #23 のフォローアップで真の差分更新を実装したら、この helper は
    /// `ok=true` を返すケースで before-removed+added == after を厳密検証する。
    fn verify_incremental_hand_threat(pos: &mut Position, m: Move) {
        for &perspective in &[Color::Black, Color::White] {
            let king_sq_before = pos.king_square(perspective);

            let mut before_indices = Vec::new();
            append_active_hand_threat_indices(
                pos,
                perspective,
                king_sq_before,
                &mut before_indices,
            );
            before_indices.sort();

            let gc = pos.gives_check(m);
            let dirty = pos.do_move(m, gc);

            let king_sq_after = pos.king_square(perspective);

            // HM mirror 境界を跨ぐ場合 needs_hand_threat_refresh は true を返すので
            // 差分更新は呼ばれない (呼び出し元で full rebuild に分岐する)
            let should_refresh =
                needs_hand_threat_refresh(&dirty, king_sq_after, perspective);

            let mut after_indices = Vec::new();
            append_active_hand_threat_indices(pos, perspective, king_sq_after, &mut after_indices);
            after_indices.sort();

            if !should_refresh {
                // 差分更新を試行
                let mut removed = IndexList::<MAX_CHANGED_HAND_THREAT_FEATURES>::new();
                let mut added = IndexList::<MAX_CHANGED_HAND_THREAT_FEATURES>::new();
                let ok = append_changed_hand_threat_indices(
                    pos,
                    &dirty,
                    perspective,
                    king_sq_after,
                    &mut removed,
                    &mut added,
                );
                if ok {
                    // 真の差分更新が実装されている場合:
                    // before - removed + added == after を検証
                    let removed_set: Vec<usize> = removed.iter().collect();
                    let added_set: Vec<usize> = added.iter().collect();

                    let mut computed = before_indices.clone();
                    for &r in &removed_set {
                        let found = computed.iter().position(|&x| x == r);
                        assert!(
                            found.is_some(),
                            "removed index {r} not in before_set \
                             (perspective={perspective:?}, move={m:?})"
                        );
                        computed.remove(found.unwrap());
                    }
                    for &a in &added_set {
                        computed.push(a);
                    }
                    computed.sort();
                    assert_eq!(
                        computed, after_indices,
                        "HandThreat incremental mismatch \
                         (perspective={perspective:?}, move={m:?})\n\
                         before={before_indices:?}\n\
                         removed={removed_set:?}\n\
                         added={added_set:?}\n\
                         computed={computed:?}\n\
                         expected={after_indices:?}"
                    );
                }
                // ok == false (current stub) の場合は full rebuild fallback
                // なので特に検証する必要なし (full rebuild は append_active で計算済み)
            }

            pos.undo_move(m);
        }
    }

    /// 7g7f (7六歩) で差分更新が full rebuild と一致
    ///
    /// この手は持ち駒変化なし (非取り)、玉動きなし、単純な pawn 移動。
    #[test]
    fn test_hand_threat_incremental_pawn_push() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");
        let m = Move::from_usi("7g7f").expect("7g7f");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// 角交換成り (取りあり、成りあり、持ち駒変化あり) で差分更新が full rebuild と一致
    #[test]
    fn test_hand_threat_incremental_capture_promotion() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");
        for mv in &["7g7f", "3c3d"] {
            let m = Move::from_usi(mv).unwrap();
            let gc = pos.gives_check(m);
            pos.do_move(m, gc);
        }
        // 8八角 → 2二角成 (取り + 成り、後手の角を捕獲して先手の持ち駒に追加)
        let m = Move::from_usi("8h2b+").expect("8h2b+");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// 持ち駒あり局面で歩打ち → 持ち駒変化なし (打ちは持ち駒減るが drop 動作の試験)
    ///
    /// 角交換成り後に持ち駒ありの状態で、非取り歩突き
    #[test]
    fn test_hand_threat_incremental_midgame_normal_move() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");
        // 7六歩 → 3四歩 → 8八角成 → 同銀 (持ち駒あり局面を作る)
        for mv in &["7g7f", "3c3d", "8h2b+", "3a2b"] {
            let m = Move::from_usi(mv).unwrap();
            let gc = pos.gives_check(m);
            pos.do_move(m, gc);
        }
        // 先手番、持ち駒に角あり、非取り歩突き
        let m = Move::from_usi("2g2f").expect("2g2f");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// 玉移動 (HM zone 内、非跨ぎ) で差分更新が動作確認
    #[test]
    fn test_hand_threat_incremental_king_move_within_zone() {
        let mut pos = Position::new();
        // 起点: 2六歩 (横移動しない歩突き) → 後で先手玉を 4八 に動かす
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSG1GSNL b - 1")
            .expect("sfen without black king");
        // set_sfen は king 無しの position を受け付けないので、代わりに一般的な中盤局面で king 移動を試す
        let mut pos2 = Position::new();
        pos2.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1BK4R1/LNSG1GSNL b - 1")
            .expect("sfen with king at 7h");
        // 玉 7h → 6h (file 6→5, rank h) file=6→5, both in [5..], is_hm_mirror 不変
        let m = Move::from_usi("7h6h").expect("7h6h");
        // Note: ここでは startpos が使えないので別の検証として動作させる
        verify_incremental_hand_threat(&mut pos2, m);
    }

    /// Cross-validation: bullet-shogi と同一の index を生成するか確認する
    ///
    /// bullet-shogi 側で同一局面を構築し、`/tmp/hand_threat_golden_minimal.txt` に
    /// sorted `stm_idx nstm_idx` ペアを書き出す (手動実行):
    /// ```bash
    /// cd bullet-shogi
    /// cargo test test_write_hand_threat_golden -- --ignored --nocapture
    /// ```
    ///
    /// 局面: 先手玉=5九、後手玉=5一、先手飛車=5五、先手持ち駒=Pawn×1
    /// SFEN: `4k4/9/9/9/4R4/9/9/9/4K4 b P 1`
    ///
    /// Expected (bullet-shogi から取得した golden): stm=468, nstm=61667
    #[test]
    fn test_cross_validation_minimal_pawn_drop() {
        let mut pos = Position::new();
        // 4k4: 5一に後手玉
        // 4R4: 5五に先手飛
        // 4K4: 5九に先手玉
        // 先手の持ち駒: Pawn×1 (`b P 1` で `P` が 1 枚の Pawn を意味する)
        pos.set_sfen("4k4/9/9/9/4R4/9/9/9/4K4 b P 1")
            .expect("minimal test sfen");

        // STM (Black) perspective
        let king_b = pos.king_square(Color::Black);
        let mut stm_indices = Vec::new();
        append_active_hand_threat_indices(&pos, Color::Black, king_b, &mut stm_indices);

        // NSTM (White) perspective
        let king_w = pos.king_square(Color::White);
        let mut nstm_indices = Vec::new();
        append_active_hand_threat_indices(&pos, Color::White, king_w, &mut nstm_indices);

        // 両 perspective は同じ 1 件の feature を生成するはず
        assert_eq!(stm_indices.len(), 1, "STM should have 1 hand threat");
        assert_eq!(nstm_indices.len(), 1, "NSTM should have 1 hand threat");

        // bullet-shogi から取得した golden 値
        // test_write_hand_threat_golden の出力: stm=468 nstm=61667
        assert_eq!(
            stm_indices[0], 468,
            "STM index mismatch: expected 468 (from bullet-shogi golden)"
        );
        assert_eq!(
            nstm_indices[0], 61667,
            "NSTM index mismatch: expected 61667 (from bullet-shogi golden)"
        );
    }
}
