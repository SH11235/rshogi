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
    ATTACKS_PER_COLOR, NUM_THREAT_CLASSES, ThreatClass, decode_board_threat_info_fb,
    extract_prev_king_sq, lookup_attack_feature_offset, normalize_sq,
};

// =============================================================================
// Fallback reason counters (hand-threat-stats feature)
// =============================================================================

#[cfg(feature = "hand-threat-stats")]
pub mod stats {
    //! HandThreat 差分更新の fallback 内訳集計
    //!
    //! `hand-threat-stats` feature 有効時のみ存在する。
    //! `append_changed_hand_threat_indices` の各 return path で AtomicU64 を increment し、
    //! `dump()` で stderr に内訳を出力する。
    use std::sync::atomic::{AtomicU64, Ordering};

    macro_rules! define_counters {
        ($($name:ident),* $(,)?) => {
            $(pub static $name: AtomicU64 = AtomicU64::new(0);)*

            pub fn reset() {
                $($name.store(0, Ordering::Relaxed);)*
            }

            pub fn snapshot() -> Vec<(&'static str, u64)> {
                vec![
                    $((stringify!($name), $name.load(Ordering::Relaxed)),)*
                ]
            }
        };
    }

    define_counters! {
        // --- diff 関数 (append_changed_hand_threat_indices) の return path ---
        INCREMENTAL_OK,
        FALLBACK_KING_MOVE,
        FALLBACK_DROP,
        FALLBACK_PROMOTION_NONCAP_NONPAWN,
        FALLBACK_PROMOTION_NONCAP_PAWN,
        FALLBACK_PROMOTION_CAPTURE_NONPAWN,
        FALLBACK_PROMOTION_CAPTURE_PAWN_OR_CAPTURED_PAWN,
        FALLBACK_PROMOTION_CAPTURE_HAND_TRANSITION,
        FALLBACK_PAWN_INVOLVED,
        // PAWN_INVOLVED 分解
        FALLBACK_PAWN_CAP_OLD_PAWN,
        FALLBACK_PAWN_CAP_BOARD_PAWN,
        FALLBACK_PAWN_CAP_HAND_PAWN,
        FALLBACK_PAWN_DROP,
        FALLBACK_CAPTURE_0_1_TRANSITION,
        FALLBACK_CAPTURE_OTHER,
        FALLBACK_BUFFER_OVERFLOW,
        FALLBACK_OTHER,
        // --- rebuild_hand_threat の呼び出し元 call site ---
        // refresh path (diff が根本的に使えない):
        REBUILD_FROM_REFRESH_ACCUMULATOR,
        REBUILD_FROM_REFRESH_ACCUMULATOR_WITH_CACHE,
        // update path の reset (HM mirror 跨ぎなど):
        REBUILD_FROM_UPDATE_RESET,
        REBUILD_FROM_UPDATE_CACHE_RESET,
        // update path の diff fallback (上の FALLBACK_* counter と対応):
        REBUILD_FROM_UPDATE_DIFF_FALLBACK,
        REBUILD_FROM_UPDATE_CACHE_DIFF_FALLBACK,
        // forward_update_incremental:
        REBUILD_FROM_FORWARD_UPDATE_DIFF_FALLBACK,
        REBUILD_FROM_FORWARD_UPDATE_PATH_LONG,
        // --- Refresh diagnostic (find_usable_accumulator が None を返した時の内訳) ---
        REFRESH_DIAG_DEPTH_1_4,
        REFRESH_DIAG_DEPTH_5_8,
        REFRESH_DIAG_DEPTH_9_16,
        REFRESH_DIAG_DEPTH_17_32,
        REFRESH_DIAG_DEPTH_33_PLUS,
        REFRESH_DIAG_CURRENT_KING_MOVED,
        REFRESH_DIAG_ANCESTOR_KING_MOVED,
        REFRESH_DIAG_CHAIN_ENDED,
        // --- King move を許容して walk した時の depth 分布 ---
        // (Fix B の path length 見積もり; 現状の find_usable では届かない祖先の深さ)
        REFRESH_DIAG_KOK_DEPTH_1,
        REFRESH_DIAG_KOK_DEPTH_2,
        REFRESH_DIAG_KOK_DEPTH_3_4,
        REFRESH_DIAG_KOK_DEPTH_5_8,
        REFRESH_DIAG_KOK_DEPTH_9_PLUS,
        REFRESH_DIAG_KOK_NO_ANCESTOR,
        // --- Fix B (HandThreat-specific Tier 2) fallback reason counters ---
        // find_usable_for_hand_threat per perspective:
        FIXB_FIND_HIT_DEPTH_1,
        FIXB_FIND_HIT_DEPTH_2,
        FIXB_FIND_HIT_DEPTH_3_4,
        FIXB_FIND_HIT_DEPTH_5_8,
        FIXB_FIND_HIT_DEPTH_9_16,
        FIXB_FIND_HIT_DEPTH_17_PLUS,
        FIXB_FIND_MISS_MAX_DEPTH,
        FIXB_FIND_MISS_MIRROR_MISMATCH,
        FIXB_FIND_MISS_CHAIN_END,
        // try_apply_hand_threat_tier2 at call site:
        FIXB_APPLY_SUCCESS,
        FIXB_APPLY_SKIP_ONE_PERSPECTIVE_MISS,
        FIXB_APPLY_SKIP_SRC_IDX_MISMATCH,
        FIXB_APPLY_SKIP_DEPTH_TOO_DEEP,
        FIXB_APPLY_SKIP_PATH_COLLECT_FAIL,
        FIXB_APPLY_SKIP_PATH_LEN_MISMATCH,
        FIXB_APPLY_FAIL_DIFF_FALLBACK,
        // --- Tier D (forward_update_incremental) path length 分布 ---
        // PATH_LEN_1 は既存 fast path (incremental)。2+ は現状 rebuild fallback。
        TIERD_PATH_LEN_1,
        TIERD_PATH_LEN_2,
        TIERD_PATH_LEN_3,
        TIERD_PATH_LEN_4_PLUS,
    }

    /// diff カウンタのみの合計 (update 呼び出し総数)
    fn diff_total() -> u64 {
        INCREMENTAL_OK.load(Ordering::Relaxed)
            + FALLBACK_KING_MOVE.load(Ordering::Relaxed)
            + FALLBACK_DROP.load(Ordering::Relaxed)
            + FALLBACK_PROMOTION_NONCAP_NONPAWN.load(Ordering::Relaxed)
            + FALLBACK_PROMOTION_NONCAP_PAWN.load(Ordering::Relaxed)
            + FALLBACK_PROMOTION_CAPTURE_NONPAWN.load(Ordering::Relaxed)
            + FALLBACK_PROMOTION_CAPTURE_PAWN_OR_CAPTURED_PAWN.load(Ordering::Relaxed)
            + FALLBACK_PROMOTION_CAPTURE_HAND_TRANSITION.load(Ordering::Relaxed)
            + FALLBACK_PAWN_INVOLVED.load(Ordering::Relaxed)
            + FALLBACK_CAPTURE_0_1_TRANSITION.load(Ordering::Relaxed)
            + FALLBACK_CAPTURE_OTHER.load(Ordering::Relaxed)
            + FALLBACK_BUFFER_OVERFLOW.load(Ordering::Relaxed)
            + FALLBACK_OTHER.load(Ordering::Relaxed)
    }

    /// stderr に内訳を出力する
    pub fn dump() {
        let entries = snapshot();
        let diff_total = diff_total();
        eprintln!("=== HandThreat update stats ===");
        eprintln!("  diff call total: {diff_total}");
        for (name, count) in &entries {
            if name.starts_with("REBUILD_FROM_") || name.starts_with("REFRESH_DIAG_") {
                continue;
            }
            if diff_total == 0 {
                continue;
            }
            let pct = (*count as f64) * 100.0 / (diff_total as f64);
            eprintln!("  {name:50}  {count:>12}  {pct:6.2}%");
        }
        eprintln!();
        // REBUILD 内訳
        let rebuild_total: u64 = entries
            .iter()
            .filter(|(n, _)| n.starts_with("REBUILD_FROM_"))
            .map(|(_, v)| *v)
            .sum();
        eprintln!("  rebuild call total: {rebuild_total}");
        for (name, count) in &entries {
            if !name.starts_with("REBUILD_FROM_") {
                continue;
            }
            if rebuild_total == 0 {
                continue;
            }
            let pct = (*count as f64) * 100.0 / (rebuild_total as f64);
            eprintln!("  {name:50}  {count:>12}  {pct:6.2}%");
        }
        eprintln!();
        // REFRESH 診断内訳 (find_usable が None の理由)
        let diag_total: u64 = entries
            .iter()
            .filter(|(n, _)| n.starts_with("REFRESH_DIAG_"))
            .map(|(_, v)| *v)
            .sum();
        eprintln!("  refresh diag total (find_usable=None case): {diag_total}");
        for (name, count) in &entries {
            if !name.starts_with("REFRESH_DIAG_") {
                continue;
            }
            if diag_total == 0 {
                continue;
            }
            let pct = (*count as f64) * 100.0 / (diag_total as f64);
            eprintln!("  {name:50}  {count:>12}  {pct:6.2}%");
        }
        eprintln!();
        // Fix B find_usable_for_hand_threat 内訳 (per perspective 呼び出し)
        let find_total: u64 = entries
            .iter()
            .filter(|(n, _)| n.starts_with("FIXB_FIND_"))
            .map(|(_, v)| *v)
            .sum();
        eprintln!("  fixB find_usable total: {find_total}");
        for (name, count) in &entries {
            if !name.starts_with("FIXB_FIND_") {
                continue;
            }
            if find_total == 0 {
                continue;
            }
            let pct = (*count as f64) * 100.0 / (find_total as f64);
            eprintln!("  {name:50}  {count:>12}  {pct:6.2}%");
        }
        eprintln!();
        // Fix B apply (try_apply_hand_threat_tier2) 呼び出し内訳
        let apply_total: u64 = entries
            .iter()
            .filter(|(n, _)| n.starts_with("FIXB_APPLY_"))
            .map(|(_, v)| *v)
            .sum();
        eprintln!("  fixB apply total: {apply_total}");
        for (name, count) in &entries {
            if !name.starts_with("FIXB_APPLY_") {
                continue;
            }
            if apply_total == 0 {
                continue;
            }
            let pct = (*count as f64) * 100.0 / (apply_total as f64);
            eprintln!("  {name:50}  {count:>12}  {pct:6.2}%");
        }
        eprintln!();
        // Tier D (forward_update_incremental) path length 分布
        let tierd_total: u64 = entries
            .iter()
            .filter(|(n, _)| n.starts_with("TIERD_PATH_LEN_"))
            .map(|(_, v)| *v)
            .sum();
        eprintln!("  Tier D path length total: {tierd_total}");
        for (name, count) in &entries {
            if !name.starts_with("TIERD_PATH_LEN_") {
                continue;
            }
            if tierd_total == 0 {
                continue;
            }
            let pct = (*count as f64) * 100.0 / (tierd_total as f64);
            eprintln!("  {name:50}  {count:>12}  {pct:6.2}%");
        }
    }
}

/// Counter increment ヘルパー (feature 無効時は no-op)
#[cfg(feature = "hand-threat-stats")]
macro_rules! bump {
    ($counter:ident) => {
        stats::$counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    };
}
#[cfg(not(feature = "hand-threat-stats"))]
macro_rules! bump {
    ($counter:ident) => {
        ()
    };
}

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
/// `threat_features::needs_threat_refresh` と同じロジック:
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

/// `PieceType` から `HandThreatClass` に変換する (手駒にできる駒のみ)
#[inline]
pub(crate) fn piece_type_to_hand_threat_class(pt: PieceType) -> Option<HandThreatClass> {
    match pt {
        PieceType::Pawn => Some(HandThreatClass::Pawn),
        PieceType::Lance => Some(HandThreatClass::Lance),
        PieceType::Knight => Some(HandThreatClass::Knight),
        PieceType::Silver => Some(HandThreatClass::Silver),
        PieceType::Gold => Some(HandThreatClass::Gold),
        PieceType::Bishop => Some(HandThreatClass::Bishop),
        PieceType::Rook => Some(HandThreatClass::Rook),
        _ => None,
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
    assert!(HAND_THREAT_DIMENSIONS == 121_104, "HAND_THREAT_DIMENSIONS must be 121,104");
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

/// `[hand_class][color]` で「行きどころ無し制限を満たす」マスの bitboard を保持する static。
///
/// `for_each_active_hand_threat_index` の hot path で per-square check を bitboard
/// 1 回の AND に置換するために使用。
static LEGAL_DROP_RANK_BB: std::sync::LazyLock<[[Bitboard; 2]; HAND_NUM_CLASSES]> =
    std::sync::LazyLock::new(|| {
        let mut tbl = [[Bitboard::EMPTY; 2]; HAND_NUM_CLASSES];
        for &hc in &ALL_HAND_THREAT_CLASSES {
            for &color in &[Color::Black, Color::White] {
                let mut bb = Bitboard::EMPTY;
                for raw in 0..81u8 {
                    if let Some(sq) = Square::from_u8(raw)
                        && is_legal_drop_rank(hc, color, sq)
                    {
                        bb |= Bitboard::from_square(sq);
                    }
                }
                tbl[hc as usize][color as usize] = bb;
            }
        }
        tbl
    });

#[inline]
pub(crate) fn legal_drop_rank_bb(hand_class: HandThreatClass, color: Color) -> Bitboard {
    LEGAL_DROP_RANK_BB[hand_class as usize][color as usize]
}

/// `pawn_bb` (ある color の生歩 bitboard) から、その駒が存在する file 全体を表す
/// bitboard を返す。Pawn drop の二歩判定で「drop が禁止される file」をまとめて
/// 1 つの bitboard で扱うために使う。
#[inline]
pub(crate) fn pawn_files_bb(pawn_bb: Bitboard) -> Bitboard {
    let mut result = Bitboard::EMPTY;
    let mut iter = pawn_bb;
    while !iter.is_empty() {
        let sq = iter.pop();
        result |= FILE_BB[sq.file() as usize];
    }
    result
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

/// before 状態の has_pawn_on_file を返す (pawn file flip 対応)
///
/// `flip == Some((flipped_color, flipped_file))` なら、その (color, file) の
/// ペアに対しては `pos` の返す値を反転 (二歩 state が flip しているため)。
/// それ以外は `pos` の値と同じ。
///
/// `flips` は最大 2 つまでの同時 flip に対応する。
/// 通常の 1 flip 用途 (drop or capture of pawn) は `[Some(...), None]`。
/// 2 flip 用途 (Pawn promotion + cap of pawn、両方が同じ file に flip) は
/// `[Some(attacker_flip), Some(cap_flip)]`。
#[inline]
pub(crate) fn has_pawn_on_file_before<P: HandThreatPosLike>(
    pos: &P,
    color: Color,
    sq: Square,
    flips: [Option<(Color, u8)>; 2],
) -> bool {
    let after = pos.has_pawn_on_file(color, sq);
    for flip in flips {
        if let Some((fc, ff)) = flip
            && color == fc
            && sq.file() as u8 == ff
        {
            return !after;
        }
    }
    after
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
// HandThreatPosLike trait (Fix B: Position と Snapshot を統一的に扱う)
// =============================================================================

/// `append_changed_hand_threat_indices` から見える Position のサブセット。
/// `Position` 本体と HandThreat-specific Tier 2 用 `HandThreatPosSnapshot`
/// の両方で実装する。
pub trait HandThreatPosLike {
    fn occupied(&self) -> Bitboard;
    fn piece_at(&self, sq: Square) -> crate::types::Piece;
    fn hand_count(&self, color: Color, pt: PieceType) -> u32;
    fn has_pawn_on_file(&self, color: Color, sq: Square) -> bool;
}

impl HandThreatPosLike for Position {
    #[inline]
    fn occupied(&self) -> Bitboard {
        Position::occupied(self)
    }
    #[inline]
    fn piece_at(&self, sq: Square) -> crate::types::Piece {
        self.piece_on(sq)
    }
    #[inline]
    fn hand_count(&self, color: Color, pt: PieceType) -> u32 {
        self.hand(color).count(pt)
    }
    #[inline]
    fn has_pawn_on_file(&self, color: Color, sq: Square) -> bool {
        has_pawn_on_file(self, color, sq)
    }
}

// =============================================================================
// HandThreatPosSnapshot (Fix B: HandThreat-specific multi-ply forward 用)
// =============================================================================

/// HandThreat 差分計算に必要な最小限の状態を保持するスナップショット。
///
/// 用途: HandThreat-specific Tier 2 経路で、Tier 3 refresh path から
/// king move chain を walk して computed 祖先まで戻り、そこから forward 方向に
/// 各 step ごとに HandThreat diff を適用するため。
///
/// 通常の `Position` の代わりに `HandThreatPosLike` trait 経由で
/// `append_changed_hand_threat_indices` に渡される。
#[derive(Clone)]
pub struct HandThreatPosSnapshot {
    pub occupied: Bitboard,
    /// 各マスの駒 (King 含む)
    pub piece_at: [crate::types::Piece; 81],
    /// `[color][pt_idx]` の手駒カウント。pt_idx は `pt_to_hand_idx` で計算
    pub hand_count: [[u32; 8]; 2],
    /// 通常 Pawn (非成駒) の bitboard、二歩判定用
    pub pawn_bb: [Bitboard; 2],
    /// 各色の玉位置 (king move を含む multi-ply walk で king_sq を追跡するため)
    pub king_sq: [Square; 2],
}

#[inline]
fn pt_to_hand_idx(pt: PieceType) -> usize {
    match pt {
        PieceType::Pawn => 0,
        PieceType::Lance => 1,
        PieceType::Knight => 2,
        PieceType::Silver => 3,
        PieceType::Gold => 4,
        PieceType::Bishop => 5,
        PieceType::Rook => 6,
        _ => 7, // dummy
    }
}

impl HandThreatPosSnapshot {
    /// `Position` から初期スナップショットを構築する
    pub fn from_pos(pos: &Position) -> Self {
        let mut piece_at = [crate::types::Piece::NONE; 81];
        for raw in 0..81u8 {
            if let Some(sq) = Square::from_u8(raw) {
                piece_at[raw as usize] = pos.piece_on(sq);
            }
        }
        let mut hand_count = [[0u32; 8]; 2];
        for color in [Color::Black, Color::White] {
            let h = pos.hand(color);
            for pt in [
                PieceType::Pawn,
                PieceType::Lance,
                PieceType::Knight,
                PieceType::Silver,
                PieceType::Gold,
                PieceType::Bishop,
                PieceType::Rook,
            ] {
                hand_count[color as usize][pt_to_hand_idx(pt)] = h.count(pt);
            }
        }
        let pawn_bb = [
            pos.pieces(Color::Black, PieceType::Pawn),
            pos.pieces(Color::White, PieceType::Pawn),
        ];
        let king_sq = [pos.king_square(Color::Black), pos.king_square(Color::White)];
        Self {
            occupied: pos.occupied(),
            piece_at,
            hand_count,
            pawn_bb,
            king_sq,
        }
    }

    #[inline]
    pub fn king_square(&self, color: Color) -> Square {
        self.king_sq[color as usize]
    }

    /// `dirty_piece` を順方向に適用してスナップショットを次の状態に進める。
    ///
    /// 2-pass: まず全 changed_piece の old_piece を remove し、その後で new_piece を add する。
    /// 1-pass だと capture 時に cp[0]=attacker (from→to) を処理後、cp[1]=captured
    /// (old=to, new=hand) の old_remove で to を消してしまう。
    pub fn apply_dirty_forward(&mut self, dirty: &DirtyPiece) {
        for i in 0..dirty.dirty_num as usize {
            let cp = &dirty.changed_piece[i];
            self.apply_old_remove(&cp.old_piece, dirty.king_moved, i);
        }
        for i in 0..dirty.dirty_num as usize {
            let cp = &dirty.changed_piece[i];
            self.apply_new_add(&cp.new_piece, dirty.king_moved, i);
        }
    }

    /// `dirty_piece` を逆方向に適用してスナップショットを前の状態に戻す。
    ///
    /// 2-pass: まず全 changed_piece の new_piece を remove し、その後で old_piece を add する。
    pub fn apply_dirty_reverse(&mut self, dirty: &DirtyPiece) {
        for i in 0..dirty.dirty_num as usize {
            let cp = &dirty.changed_piece[i];
            self.apply_old_remove(&cp.new_piece, dirty.king_moved, i);
        }
        for i in 0..dirty.dirty_num as usize {
            let cp = &dirty.changed_piece[i];
            self.apply_new_add(&cp.old_piece, dirty.king_moved, i);
        }
    }

    /// `bp` が表す駒・手駒を snapshot から除去する
    ///
    /// `king_moved`/`cp_index` は king 判定用 (decode_board_threat_info_fb は King を
    /// None にするため、king move の場合は decode_board_square_fb + king_moved 情報で
    /// king 駒を識別)。
    fn apply_old_remove(
        &mut self,
        bp: &super::bona_piece::ExtBonaPiece,
        king_moved: [bool; 2],
        cp_index: usize,
    ) {
        // まず通常駒として decode を試みる
        if let Some((color, _, pt, sq)) = decode_board_threat_info_fb(bp.fb) {
            self.occupied &= !Bitboard::from_square(sq);
            self.piece_at[sq.index()] = crate::types::Piece::NONE;
            if pt == PieceType::Pawn {
                self.pawn_bb[color as usize] &= !Bitboard::from_square(sq);
            }
            return;
        }
        // 手駒 BonaPiece として decode
        if let Some((color, pt)) = decode_hand_piece_fb(bp.fb) {
            self.hand_count[color as usize][pt_to_hand_idx(pt)] -= 1;
            return;
        }
        // どちらでもない → King の可能性 (king_moved && cp_index == 0 のとき)
        // King は board_threat_info_fb で None になるので decode_board_square_fb で sq を取る
        if cp_index == 0 && (king_moved[0] || king_moved[1]) {
            use super::threat_features::decode_board_square_fb;
            if let Some(sq) = decode_board_square_fb(bp.fb) {
                self.occupied &= !Bitboard::from_square(sq);
                self.piece_at[sq.index()] = crate::types::Piece::NONE;
            }
        }
    }

    #[inline]
    fn apply_new_add(
        &mut self,
        bp: &super::bona_piece::ExtBonaPiece,
        king_moved: [bool; 2],
        cp_index: usize,
    ) {
        if let Some((color, _, pt, sq)) = decode_board_threat_info_fb(bp.fb) {
            self.occupied |= Bitboard::from_square(sq);
            self.piece_at[sq.index()] = crate::types::Piece::new(color, pt);
            if pt == PieceType::Pawn {
                self.pawn_bb[color as usize] |= Bitboard::from_square(sq);
            }
            return;
        }
        if let Some((color, pt)) = decode_hand_piece_fb(bp.fb) {
            self.hand_count[color as usize][pt_to_hand_idx(pt)] += 1;
            return;
        }
        // King の可能性
        if cp_index == 0 && (king_moved[0] || king_moved[1]) {
            use super::threat_features::decode_board_square_fb;
            if let Some(sq) = decode_board_square_fb(bp.fb) {
                let king_color = if king_moved[0] {
                    Color::Black
                } else {
                    Color::White
                };
                self.occupied |= Bitboard::from_square(sq);
                self.piece_at[sq.index()] = crate::types::Piece::new(king_color, PieceType::King);
                self.king_sq[king_color as usize] = sq;
            }
        }
    }
}

impl HandThreatPosLike for HandThreatPosSnapshot {
    #[inline]
    fn occupied(&self) -> Bitboard {
        self.occupied
    }
    #[inline]
    fn piece_at(&self, sq: Square) -> crate::types::Piece {
        self.piece_at[sq.index()]
    }
    #[inline]
    fn hand_count(&self, color: Color, pt: PieceType) -> u32 {
        self.hand_count[color as usize][pt_to_hand_idx(pt)]
    }
    #[inline]
    fn has_pawn_on_file(&self, color: Color, sq: Square) -> bool {
        let file_bb = FILE_BB[sq.file() as usize];
        !(self.pawn_bb[color as usize] & file_bb).is_empty()
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
// 手駒 BonaPiece decode (差分更新 helper)
// =============================================================================

/// 手駒 BonaPiece (`fb` perspective) から `(owner_color, piece_type)` を復元する。
///
/// 盤上駒・ZERO の場合は `None`。返される `PieceType` は Pawn/Lance/Knight/Silver/
/// Gold/Bishop/Rook のいずれかで、成駒は含まれない (手駒に成駒は存在しない)。
///
/// ## 目的
///
/// `dirty_piece[1].new_piece.fb` から実際に手駒側に増減した駒種を取得するために使う。
/// `decode_board_threat_info_fb` は盤上駒の Threat 情報を返すが、成駒を GoldLike
/// クラスに正規化 (Gold/ProPawn/ProLance/ProKnight/ProSilver → `PieceType::Gold`)
/// してしまうため、手駒側 transition の判定には使えない。
///
/// 本 helper は `bona_piece.rs` の手駒レイアウトに従って range 判定で decode する:
///
/// ```text
/// Black Pawn   : 1..=18   (F_HAND_PAWN=1,   max count 18)
/// White Pawn   : 20..=37  (E_HAND_PAWN=20)
/// Black Lance  : 39..=42  (F_HAND_LANCE=39, max count 4)
/// White Lance  : 44..=47
/// Black Knight : 49..=52  (F_HAND_KNIGHT=49)
/// White Knight : 54..=57
/// Black Silver : 59..=62  (F_HAND_SILVER=59)
/// White Silver : 64..=67
/// Black Gold   : 69..=72  (F_HAND_GOLD=69)
/// White Gold   : 74..=77
/// Black Bishop : 79..=80  (F_HAND_BISHOP=79, max count 2)
/// White Bishop : 82..=83
/// Black Rook   : 85..=86  (F_HAND_ROOK=85)
/// White Rook   : 88..=89
/// ```
#[inline]
pub(crate) fn decode_hand_piece_fb(bp: super::bona_piece::BonaPiece) -> Option<(Color, PieceType)> {
    use super::bona_piece::{
        E_HAND_BISHOP, E_HAND_GOLD, E_HAND_KNIGHT, E_HAND_LANCE, E_HAND_PAWN, E_HAND_ROOK,
        E_HAND_SILVER, F_HAND_BISHOP, F_HAND_GOLD, F_HAND_KNIGHT, F_HAND_LANCE, F_HAND_PAWN,
        F_HAND_ROOK, F_HAND_SILVER, FE_HAND_END,
    };
    let v = bp.value();
    if v == 0 || (v as usize) >= FE_HAND_END {
        return None;
    }
    // Pawn (18 slots per color)
    if (F_HAND_PAWN..F_HAND_PAWN + 18).contains(&v) {
        return Some((Color::Black, PieceType::Pawn));
    }
    if (E_HAND_PAWN..E_HAND_PAWN + 18).contains(&v) {
        return Some((Color::White, PieceType::Pawn));
    }
    // Lance (4 slots per color)
    if (F_HAND_LANCE..F_HAND_LANCE + 4).contains(&v) {
        return Some((Color::Black, PieceType::Lance));
    }
    if (E_HAND_LANCE..E_HAND_LANCE + 4).contains(&v) {
        return Some((Color::White, PieceType::Lance));
    }
    // Knight
    if (F_HAND_KNIGHT..F_HAND_KNIGHT + 4).contains(&v) {
        return Some((Color::Black, PieceType::Knight));
    }
    if (E_HAND_KNIGHT..E_HAND_KNIGHT + 4).contains(&v) {
        return Some((Color::White, PieceType::Knight));
    }
    // Silver
    if (F_HAND_SILVER..F_HAND_SILVER + 4).contains(&v) {
        return Some((Color::Black, PieceType::Silver));
    }
    if (E_HAND_SILVER..E_HAND_SILVER + 4).contains(&v) {
        return Some((Color::White, PieceType::Silver));
    }
    // Gold
    if (F_HAND_GOLD..F_HAND_GOLD + 4).contains(&v) {
        return Some((Color::Black, PieceType::Gold));
    }
    if (E_HAND_GOLD..E_HAND_GOLD + 4).contains(&v) {
        return Some((Color::White, PieceType::Gold));
    }
    // Bishop (2 slots per color)
    if (F_HAND_BISHOP..F_HAND_BISHOP + 2).contains(&v) {
        return Some((Color::Black, PieceType::Bishop));
    }
    if (E_HAND_BISHOP..E_HAND_BISHOP + 2).contains(&v) {
        return Some((Color::White, PieceType::Bishop));
    }
    // Rook
    if (F_HAND_ROOK..F_HAND_ROOK + 2).contains(&v) {
        return Some((Color::Black, PieceType::Rook));
    }
    if (E_HAND_ROOK..E_HAND_ROOK + 2).contains(&v) {
        return Some((Color::White, PieceType::Rook));
    }
    None
}

// =============================================================================
// append_changed_hand_threat_indices (差分更新)
// =============================================================================

/// 差分計算で使う中間バッファの上限
///
/// 1 ブロック (drop_owner, hand_class) あたり source_set ≤ 16 sq、
/// 各 source drop から target ≤ 20 (slider 含む最悪ケース)、
/// 最大 2×7 ブロックで約 4,480。余裕を見て 8,192。
const MAX_INTERMEDIATE_HAND_THREATS: usize = 8_192;

/// DirtyPiece から HandThreat 特徴量の差分 (removed / added) を計算する
///
/// ## 対応ケース
///
/// ほぼ全ての move 種別に対応する (incremental_ok 率 ~96%)。
///
/// - **Board move (`dirty_num == 1`)**: 非成り、もしくは非 Pawn 成り
///   - 非成り: source_set diff
///   - 非 Pawn 成り: to_sq の piece class 変化を after 列挙で拾う
/// - **Capture (`dirty_num == 2`)**: 非成り、もしくは非 Pawn 成り
///   - 非 Pawn capture: source_set diff、必要なら capture transition direct push
///   - Pawn capture: cap 側の `pawn_file_flip = (cap_color, file)` で
///     二歩 state 変化を扱う
/// - **Drop (`dirty_num == 1`, old が手駒)**: 全駒種
///   - 非 Pawn drop: source_set diff、必要なら drop transition direct push
///   - Pawn drop: drop 側の `pawn_file_flip = (dropper, file)` で対応
///
/// ## 仕組み
///
/// 各ブロック (drop_owner, hand_class) の影響 drop_sq 集合（source_set）は
/// 以下で過剰包含する:
///
/// - `{from_sq, to_sq}` (占有変化、drop 候補自体)
/// - `attacks_from_dropped(hc, !drop_color, from_sq, EMPTY)` (非 slider は
///   reverse_attackers、slider は空盤上 ray で「from_sq を経由する」Y も含む)
/// - 同様に `to_sq` 版
/// - Pawn 二歩 flip がある block では `FILE_BB[flipped_file]` も追加
///
/// Slider block では Y の実 attack_bb は Y ∉ source_set でも変化しうるが、
/// その Y の feature は必ず attack_to_sq ∈ {from, to} または ray 上にある。
/// 前者は source_set で捕捉、後者は Y 自身が ray_set で拾われるため、
/// ソート+マージ diff で正しく計算できる。
///
/// attack_bb 計算は before では `before_occ`、after では `after_occ` を使う
/// (slider の場合の ray 遮断を正確に反映するため)。
///
/// 0↔1 hand block transition (capture で初獲得 / drop で最後の 1 枚) は
/// 直接 `removed`/`added` IndexList に push する direct push 方式で扱う
/// (sort-merge buffer をバイパス)。
///
/// ## 未対応ケース (full rebuild fallback)
///
/// - 玉移動 (HM mirror 跨ぎは外側 needs_hand_threat_refresh で先に判定)
/// - Pawn 成り (Pawn → ProPawn は動 player 側の pawn file flip があり未対応)
/// - 異常 dirty (dirty_num > 2、decode 失敗、cap_color == old_color など防御的)
///
/// ## 戻り値
/// - `true`: 差分計算成功、`removed` / `added` に diff が格納された
/// - `false`: 対応外ケースまたは overflow → 呼び出し元で full rebuild が必要
pub fn append_changed_hand_threat_indices<P: HandThreatPosLike>(
    pos: &P,
    dirty_piece: &DirtyPiece,
    perspective: Color,
    king_sq: Square,
    removed: &mut IndexList<MAX_CHANGED_HAND_THREAT_FEATURES>,
    added: &mut IndexList<MAX_CHANGED_HAND_THREAT_FEATURES>,
) -> bool {
    // === Step 1: 対応可能ケースの判定 ===
    if dirty_piece.dirty_num == 0 {
        bump!(INCREMENTAL_OK);
        return true;
    }
    if dirty_piece.dirty_num > 2 {
        bump!(FALLBACK_OTHER);
        return false;
    }

    // 玉移動の検出。HM mirror 跨ぎは外側 needs_hand_threat_refresh で fallback 済み。
    // 残るのは within-mirror king move のみ → King を board piece として扱う。
    // King は ThreatClass に含まれないので attack target からは自動 filter される。
    let is_king_move = dirty_piece.king_moved[0] || dirty_piece.king_moved[1];

    let cp0 = &dirty_piece.changed_piece[0];

    // 二歩 state が flip した (color, file)。最大 2 つ同時 flip に対応:
    //  - drop or capture-of-pawn: slot 0 のみ使用
    //  - Pawn 成り + Pawn 捕獲: slot 0 = attacker flip, slot 1 = cap_color flip
    //    (両方とも from_sq.file == to_sq.file、同じ file で色違い)
    let mut pawn_file_flip: [Option<(Color, u8)>; 2] = [None, None];
    let mut is_drop_1to0_transition = false;
    let is_drop;
    let old_color: Color;
    let old_pt: PieceType;
    let from_sq: Square;
    let new_pt: PieceType;
    let to_sq: Square;

    if is_king_move {
        // King 移動 (within-mirror)。decode_board_square_fb で from/to を抽出。
        // King 捕獲も可能 (dirty_num=2)。capture 側は後段の通常経路で処理。
        use super::threat_features::decode_board_square_fb;
        let king_color = if dirty_piece.king_moved[0] {
            Color::Black
        } else {
            Color::White
        };
        let from = decode_board_square_fb(cp0.old_piece.fb);
        let to = decode_board_square_fb(cp0.new_piece.fb);
        let (Some(from), Some(to)) = (from, to) else {
            bump!(FALLBACK_OTHER);
            return false;
        };
        is_drop = false;
        from_sq = from;
        to_sq = to;
        old_color = king_color;
        old_pt = PieceType::King;
        new_pt = PieceType::King;
    } else {
        // 通常の board move / capture / drop
        let old_info_board = decode_board_threat_info_fb(cp0.old_piece.fb);
        let new_info = decode_board_threat_info_fb(cp0.new_piece.fb);
        let Some((nc, _new_class, npt, nto)) = new_info else {
            bump!(FALLBACK_OTHER);
            return false;
        };
        new_pt = npt;
        to_sq = nto;
        let new_color_local = nc;

        if let Some((oc, _, op, fs)) = old_info_board {
            is_drop = false;
            old_color = oc;
            old_pt = op;
            from_sq = fs;
        } else {
            // Drop: cp0.old_piece.fb は手駒 BonaPiece → decode
            let Some((dropper, dropped_pt)) = decode_hand_piece_fb(cp0.old_piece.fb) else {
                bump!(FALLBACK_OTHER);
                return false;
            };
            if dropped_pt != new_pt {
                bump!(FALLBACK_OTHER);
                return false;
            }
            if dropped_pt == PieceType::Pawn {
                pawn_file_flip[0] = Some((dropper, to_sq.file() as u8));
            }
            if pos.hand_count(dropper, dropped_pt) == 0 {
                is_drop_1to0_transition = true;
            }
            if dirty_piece.dirty_num != 1 {
                bump!(FALLBACK_OTHER);
                return false;
            }
            is_drop = true;
            old_color = dropper;
            old_pt = dropped_pt;
            from_sq = to_sq; // sentinel
        }
        if old_color != new_color_local {
            bump!(FALLBACK_OTHER);
            return false;
        }
    }

    // Promotion 分類と incremental 可否判定:
    //  - 成り対象が Pawn (Pawn → ProPawn): 動 player 側の file(from_sq) pawn count が
    //    1→0 に変化 → attacker 側の pawn_file_flip[0] を設定する。
    //    Pawn は直進移動のみなので from_sq.file == to_sq.file。
    //    成り + Pawn 捕獲 (cap_pt_board == Pawn) の場合、cap_color 側の flip は同じ file
    //    になるので後段の capture path で slot[1] に設定する。
    //  - それ以外の成り (Lance/Knight/Silver/Bishop/Rook → 成駒) は動 player
    //    側の pawn file 不変。to_sq の piece class 変化は source_set + before/after
    //    列挙で自然に吸収される。
    //
    // Drop の場合は old_pt == new_pt なので is_promotion は false。
    let is_promotion = !is_drop && old_pt != new_pt;
    if is_promotion && old_pt == PieceType::Pawn {
        // attacker 側の file(from_sq) pawn file flip 1→0
        pawn_file_flip[0] = Some((old_color, from_sq.file() as u8));
    }
    // 非 Pawn promotion (with/without capture) は後段の source_set 経路で扱える。
    // capture 側の Pawn 関与判定は capture path で行う。

    // 捕獲の場合: dirty_piece[1] から captured piece 情報を取得。
    //
    // 重要: cp1.old_piece.fb は盤上駒 → decode_board_threat_info_fb は成駒を Gold に
    // 正規化して返すため、手駒 block の識別には cp1.new_piece.fb (手駒 BonaPiece) から
    // decode_hand_piece_fb で実手駒種を取得する。
    //
    // 0↔1 transition は直接 push 方式で対応。
    // 非 Pawn が生歩を捕獲するケース (PAWN_CAP_BOARD_PAWN) は
    // `pawn_file_flip = Some((cap_color, to_sq.file))` を設定して対応する。
    //
    //  - `cap_before_piece_at_to`: before 状態の to_sq 駒情報 (source_set path 用)
    //  - `capture_transition_block`: (drop_color, HandThreatClass) が transition する場合 Some
    //  - `pawn_file_flip` (関数先頭で宣言済み): 二歩 state が flip した (color, file)
    //    非 Pawn が生歩を捕獲するケース (PAWN_CAP_BOARD_PAWN) で使用。
    //    cap_color 側の board pawn file count が 1→0 で flip。
    let (cap_before_piece_at_to, capture_transition_block): (
        Option<(Color, PieceType)>,
        Option<(Color, HandThreatClass)>,
    ) = if dirty_piece.dirty_num == 2 {
        let cp1 = &dirty_piece.changed_piece[1];
        // cp1.old_piece = before 状態の盤上駒 (captured 駒)、threat info を取得
        let cap_old = decode_board_threat_info_fb(cp1.old_piece.fb);
        let Some((cap_color, _cap_class, cap_pt_board, cap_sq)) = cap_old else {
            bump!(FALLBACK_CAPTURE_OTHER);
            return false;
        };
        if cap_sq != to_sq {
            bump!(FALLBACK_CAPTURE_OTHER);
            return false;
        }
        // cp1.new_piece = after 状態の手駒 BonaPiece、実手駒種を decode
        let Some((new_hand_color, cap_pt_hand_base)) = decode_hand_piece_fb(cp1.new_piece.fb)
        else {
            bump!(FALLBACK_CAPTURE_OTHER);
            return false;
        };
        if new_hand_color != old_color {
            bump!(FALLBACK_CAPTURE_OTHER);
            return false;
        }
        if cap_color == old_color {
            bump!(FALLBACK_CAPTURE_OTHER);
            return false;
        }
        // 動いた駒が Pawn のケース:
        //  - 非成り Pawn 移動は同 file 直進なので動 player 側の pawn file count 不変
        //  - 成りは前段の is_promotion check で fallback 済み
        // → 動 player 側の pawn file flip は無く、capture 側 (cap_pt_board) の
        //   flip 判定だけ行えばよい。
        // cap_pt_board == Pawn (生歩を捕獲) → pawn_file_flip (cap_color)
        if cap_pt_board == PieceType::Pawn {
            // cap_color の file(to_sq) pawn state が 1→0 で flip
            // (二歩ルールにより before に必ず 1 枚、after に 0 枚)
            debug_assert_eq!(cap_pt_hand_base, PieceType::Pawn);
            // slot 0 は Pawn 成りで使用済みの場合があるので slot 1 に格納。
            // (attacker pawn 成り + cap-of-pawn: 両 flip が必要)
            let flip = Some((cap_color, to_sq.file() as u8));
            if pawn_file_flip[0].is_none() {
                pawn_file_flip[0] = flip;
            } else {
                pawn_file_flip[1] = flip;
            }
        }
        // cap_pt_board != Pawn && cap_pt_hand_base == Pawn: ProPawn (Tokin) を捕獲。
        // board 側 pawn_bb は変化せず (ProPawn は pawn_bb に入っていない)、
        // cap_color の pawn file state も flip しない。
        // attacker 側 hand[Pawn] は capture で +1 されるので、後段の
        // transition 判定 (after_count == 1 で 0→1 transition) で自然に扱われる。
        // transition 検出: after_count == 1 なら before=0 で 0→1 transition
        let after_count = pos.hand_count(old_color, cap_pt_hand_base);
        let trans_block = if after_count == 1 {
            let hc = piece_type_to_hand_threat_class(cap_pt_hand_base)
                .expect("cap_pt_hand_base covers all 7 hand classes");
            Some((old_color, hc))
        } else {
            None
        };
        (Some((cap_color, cap_pt_board)), trans_block)
    } else {
        (None, None)
    };

    // Drop 1→0 transition block: is_drop_1to0_transition が立っていれば (dropper, dropped_pt)
    let drop_transition_block: Option<(Color, HandThreatClass)> = if is_drop_1to0_transition {
        // old_pt は dropped_pt と等しい (sentinel で設定済)
        piece_type_to_hand_threat_class(old_pt).map(|hc| (old_color, hc))
    } else {
        None
    };

    // === Step 2: 占有状態の before/after 再構成 ===
    let after_occ = pos.occupied();
    // Board move: from_sq occupied before → empty after
    //             to_sq empty before → occupied after (non-capture)
    //                  occupied before → occupied after with different piece (capture)
    // Drop:       from_sq は存在しない (sentinel = to_sq)
    //             to_sq empty before → occupied after
    let before_occ = if is_drop {
        // Drop: to_sq was empty before, from_sq doesn't exist
        after_occ & !Bitboard::from_square(to_sq)
    } else if cap_before_piece_at_to.is_some() {
        // Capture: from_sq occupied before, to_sq occupied in both
        after_occ | Bitboard::from_square(from_sq)
    } else {
        // Non-capture move: from_sq occupied before, to_sq empty before
        (after_occ | Bitboard::from_square(from_sq)) & !Bitboard::from_square(to_sq)
    };

    let hm = is_hm_mirror(king_sq, perspective);
    let friend_color = perspective;

    // === Step 3: ブロック単位で before/after を制限列挙 ===
    let mut before_buf = [0u32; MAX_INTERMEDIATE_HAND_THREATS];
    let mut after_buf = [0u32; MAX_INTERMEDIATE_HAND_THREATS];
    let mut before_len = 0usize;
    let mut after_len = 0usize;

    for &drop_color in &[friend_color, !friend_color] {
        let drop_owner = if drop_color == friend_color { 0 } else { 1 };

        for &hand_class in &ALL_HAND_THREAT_CLASSES {
            let is_cap_trans = capture_transition_block == Some((drop_color, hand_class));
            let is_drop_trans = drop_transition_block == Some((drop_color, hand_class));

            let oriented_color = if perspective == Color::Black {
                drop_color
            } else {
                !drop_color
            };

            // --- Transition block の直接 push 処理 ---
            if is_cap_trans {
                // 0→1 transition: block 全体の after features を added に直接 push
                for drop_raw in 0..81u8 {
                    let drop_sq = Square::from_u8(drop_raw).unwrap();
                    if after_occ.contains(drop_sq) {
                        continue;
                    }
                    if !is_legal_drop_rank(hand_class, drop_color, drop_sq) {
                        continue;
                    }
                    if hand_class == HandThreatClass::Pawn
                        && pos.has_pawn_on_file(drop_color, drop_sq)
                    {
                        continue;
                    }
                    let attack_bb =
                        attacks_from_dropped(hand_class, drop_color, drop_sq, after_occ);
                    let mut targets = attack_bb & after_occ;
                    let drop_sq_n = normalize_sq(drop_sq, perspective, hm);
                    while !targets.is_empty() {
                        let to_target = targets.pop();
                        let target_pc = pos.piece_at(to_target);
                        if target_pc.is_none() {
                            continue;
                        }
                        let target_pt = target_pc.piece_type();
                        let Some(attacked_class) = ThreatClass::from_piece_type(target_pt) else {
                            continue;
                        };
                        let attacked_side = if target_pc.color() == friend_color {
                            0
                        } else {
                            1
                        };
                        let to_sq_n = normalize_sq(to_target, perspective, hm);
                        let idx = hand_threat_index(
                            drop_owner,
                            hand_class,
                            oriented_color,
                            attacked_side,
                            attacked_class,
                            drop_sq_n,
                            to_sq_n,
                        );
                        if !added.push(idx) {
                            bump!(FALLBACK_BUFFER_OVERFLOW);
                            return false;
                        }
                    }
                }
                continue; // sort-merge 経路を skip
            }

            if is_drop_trans {
                // 1→0 transition: block 全体の before features を removed に直接 push
                for drop_raw in 0..81u8 {
                    let drop_sq = Square::from_u8(drop_raw).unwrap();
                    if before_occ.contains(drop_sq) {
                        continue;
                    }
                    if !is_legal_drop_rank(hand_class, drop_color, drop_sq) {
                        continue;
                    }
                    // before 状態の二歩判定 (pawn_file_flip 対応)
                    if hand_class == HandThreatClass::Pawn
                        && has_pawn_on_file_before(pos, drop_color, drop_sq, pawn_file_flip)
                    {
                        continue;
                    }
                    let attack_bb =
                        attacks_from_dropped(hand_class, drop_color, drop_sq, before_occ);
                    let mut targets = attack_bb & before_occ;
                    let drop_sq_n = normalize_sq(drop_sq, perspective, hm);
                    while !targets.is_empty() {
                        let to_target = targets.pop();
                        let (target_color, target_pt) = if !is_drop && to_target == from_sq {
                            (old_color, old_pt)
                        } else if to_target == to_sq {
                            if let Some((cc, cp)) = cap_before_piece_at_to {
                                (cc, cp)
                            } else {
                                continue;
                            }
                        } else {
                            let pc = pos.piece_at(to_target);
                            if pc.is_none() {
                                continue;
                            }
                            (pc.color(), pc.piece_type())
                        };
                        let Some(attacked_class) = ThreatClass::from_piece_type(target_pt) else {
                            continue;
                        };
                        let attacked_side = if target_color == friend_color { 0 } else { 1 };
                        let to_sq_n = normalize_sq(to_target, perspective, hm);
                        let idx = hand_threat_index(
                            drop_owner,
                            hand_class,
                            oriented_color,
                            attacked_side,
                            attacked_class,
                            drop_sq_n,
                            to_sq_n,
                        );
                        if !removed.push(idx) {
                            bump!(FALLBACK_BUFFER_OVERFLOW);
                            return false;
                        }
                    }
                }
                continue; // sort-merge 経路を skip
            }

            // --- 非 transition block: source_set 制限列挙 + sort-merge ---
            if pos.hand_count(drop_color, hand_class.as_piece_type()) == 0 {
                continue;
            }

            // source_set (from/to + 逆方向 attack)
            let rev_from = attacks_from_dropped(hand_class, !drop_color, from_sq, Bitboard::EMPTY);
            let rev_to = attacks_from_dropped(hand_class, !drop_color, to_sq, Bitboard::EMPTY);
            let mut source_set =
                Bitboard::from_square(from_sq) | Bitboard::from_square(to_sq) | rev_from | rev_to;

            // pawn_file_flip が該当 block に影響する場合、
            // flipped file 全体を source_set に追加 (二歩 state 変化で
            // drop 候補が増減する可能性があるため)
            if hand_class == HandThreatClass::Pawn {
                for flip in pawn_file_flip {
                    if let Some((fc, ff)) = flip
                        && fc == drop_color
                    {
                        source_set |= FILE_BB[ff as usize];
                    }
                }
            }

            let before_range = source_set;
            let after_range = source_set;

            // --- After 側列挙 ---
            let mut after_drops = after_range & !after_occ;
            while !after_drops.is_empty() {
                let drop_sq = after_drops.pop();
                if !is_legal_drop_rank(hand_class, drop_color, drop_sq) {
                    continue;
                }
                if hand_class == HandThreatClass::Pawn && pos.has_pawn_on_file(drop_color, drop_sq)
                {
                    continue;
                }
                let attack_bb = attacks_from_dropped(hand_class, drop_color, drop_sq, after_occ);
                let mut targets = attack_bb & after_occ;
                let drop_sq_n = normalize_sq(drop_sq, perspective, hm);
                while !targets.is_empty() {
                    let to_target = targets.pop();
                    let target_pc = pos.piece_at(to_target);
                    if target_pc.is_none() {
                        continue;
                    }
                    let target_pt = target_pc.piece_type();
                    let Some(attacked_class) = ThreatClass::from_piece_type(target_pt) else {
                        continue;
                    };
                    let attacked_side = if target_pc.color() == friend_color {
                        0
                    } else {
                        1
                    };
                    let to_sq_n = normalize_sq(to_target, perspective, hm);
                    let idx = hand_threat_index(
                        drop_owner,
                        hand_class,
                        oriented_color,
                        attacked_side,
                        attacked_class,
                        drop_sq_n,
                        to_sq_n,
                    );
                    if after_len >= MAX_INTERMEDIATE_HAND_THREATS {
                        bump!(FALLBACK_BUFFER_OVERFLOW);
                        return false;
                    }
                    after_buf[after_len] = idx as u32;
                    after_len += 1;
                }
            }

            // --- Before 側列挙 ---
            // 注意: Pawn 二歩判定は pawn_file_flip が存在する場合 before 状態を
            //       has_pawn_on_file_before で再構成する必要がある。
            //       target piece info は from_sq のみ before = (old_color, old_pt)、
            //       他のマスは pos のまま (非取り・非成りなので変化なし)。
            let mut before_drops = before_range & !before_occ;
            while !before_drops.is_empty() {
                let drop_sq = before_drops.pop();
                if !is_legal_drop_rank(hand_class, drop_color, drop_sq) {
                    continue;
                }
                if hand_class == HandThreatClass::Pawn
                    && has_pawn_on_file_before(pos, drop_color, drop_sq, pawn_file_flip)
                {
                    continue;
                }
                let attack_bb = attacks_from_dropped(hand_class, drop_color, drop_sq, before_occ);
                let mut targets = attack_bb & before_occ;
                let drop_sq_n = normalize_sq(drop_sq, perspective, hm);
                while !targets.is_empty() {
                    let to_target = targets.pop();
                    // target の before 状態 piece info
                    // Drop の場合 from_sq = to_sq (sentinel) で、to_sq は before で空
                    // (before_occ に含まれないので通常 to_sq が target にならない)。
                    // to_target == from_sq 分岐は drop で無効化する。
                    let (target_color, target_pt) = if !is_drop && to_target == from_sq {
                        (old_color, old_pt)
                    } else if to_target == to_sq {
                        if let Some((cap_color, cap_pt)) = cap_before_piece_at_to {
                            (cap_color, cap_pt)
                        } else {
                            // non-capture / drop: to_sq was empty → skip
                            continue;
                        }
                    } else {
                        let pc = pos.piece_at(to_target);
                        if pc.is_none() {
                            continue;
                        }
                        (pc.color(), pc.piece_type())
                    };
                    let Some(attacked_class) = ThreatClass::from_piece_type(target_pt) else {
                        continue;
                    };
                    let attacked_side = if target_color == friend_color { 0 } else { 1 };
                    let to_sq_n = normalize_sq(to_target, perspective, hm);
                    let idx = hand_threat_index(
                        drop_owner,
                        hand_class,
                        oriented_color,
                        attacked_side,
                        attacked_class,
                        drop_sq_n,
                        to_sq_n,
                    );
                    if before_len >= MAX_INTERMEDIATE_HAND_THREATS {
                        bump!(FALLBACK_BUFFER_OVERFLOW);
                        return false;
                    }
                    before_buf[before_len] = idx as u32;
                    before_len += 1;
                }
            }
        }
    }

    // === Step 4: ソート + マージで set difference ===
    before_buf[..before_len].sort_unstable();
    after_buf[..after_len].sort_unstable();

    let mut bi = 0usize;
    let mut ai = 0usize;
    while bi < before_len && ai < after_len {
        let bv = before_buf[bi];
        let av = after_buf[ai];
        if bv < av {
            if !removed.push(bv as usize) {
                bump!(FALLBACK_BUFFER_OVERFLOW);
                return false;
            }
            bi += 1;
        } else if bv > av {
            if !added.push(av as usize) {
                bump!(FALLBACK_BUFFER_OVERFLOW);
                return false;
            }
            ai += 1;
        } else {
            bi += 1;
            ai += 1;
        }
    }
    while bi < before_len {
        if !removed.push(before_buf[bi] as usize) {
            bump!(FALLBACK_BUFFER_OVERFLOW);
            return false;
        }
        bi += 1;
    }
    while ai < after_len {
        if !added.push(after_buf[ai] as usize) {
            bump!(FALLBACK_BUFFER_OVERFLOW);
            return false;
        }
        ai += 1;
    }

    bump!(INCREMENTAL_OK);
    true
}

// =============================================================================
// for_each_active_hand_threat_index / append_active_hand_threat_indices
// =============================================================================

/// 現局面の全 hand threat pair を列挙し、各 index に対して `f` を呼ぶ。
///
/// rebuild_hand_threat の hot path で使用。Vec などの heap 割り当てを避けるため、
/// 列挙ごとにクロージャを呼ぶ形式。
pub fn for_each_active_hand_threat_index<F: FnMut(usize)>(
    pos: &Position,
    perspective: Color,
    king_sq: Square,
    mut f: F,
) {
    let hm = is_hm_mirror(king_sq, perspective);
    let occupied = pos.occupied();
    let empty = !occupied;
    let friend_color = perspective;
    let enemy_color = !perspective;

    for &drop_color in &[friend_color, enemy_color] {
        let drop_owner = if drop_color == friend_color { 0 } else { 1 };
        let hand = pos.hand(drop_color);

        for &hand_class in &ALL_HAND_THREAT_CLASSES {
            if hand.count(hand_class.as_piece_type()) == 0 {
                continue;
            }

            let oriented_color = if perspective == Color::Black {
                drop_color
            } else {
                !drop_color
            };

            // legal_rank の bitboard と AND して候補を絞り込む
            // (per-sq の is_legal_drop_rank check を回避)
            let mut candidates = empty & legal_drop_rank_bb(hand_class, drop_color);
            // Pawn の場合、自色 pawn が既に乗っている file 全体を除外
            if hand_class == HandThreatClass::Pawn {
                let pawn_files = pawn_files_bb(pos.pieces(drop_color, PieceType::Pawn));
                candidates &= !pawn_files;
            }
            while !candidates.is_empty() {
                let drop_sq = candidates.pop();

                let attack_bb = attacks_from_dropped(hand_class, drop_color, drop_sq, occupied);
                let mut targets = attack_bb & occupied;
                let drop_sq_n = normalize_sq(drop_sq, perspective, hm);
                while !targets.is_empty() {
                    let to_sq = targets.pop();
                    let target_pc = pos.piece_on(to_sq);
                    let target_pt = target_pc.piece_type();
                    let Some(attacked_class) = ThreatClass::from_piece_type(target_pt) else {
                        continue;
                    };
                    let attacked_side = if target_pc.color() == friend_color {
                        0
                    } else {
                        1
                    };
                    let to_sq_n = normalize_sq(to_sq, perspective, hm);
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
                    f(idx);
                }
            }
        }
    }
}

/// 現局面の全 hand threat pair を列挙し、`indices` に追加する
///
/// テスト用ヘルパー。本番経路は `for_each_active_hand_threat_index` を直接使う。
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
                if hand_class == HandThreatClass::Pawn && has_pawn_on_file(pos, drop_color, drop_sq)
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

                    let attacked_side = if target_pc.color() == friend_color {
                        0
                    } else {
                        1
                    };

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
        assert_eq!(HandThreatClass::Pawn.as_board_class() as usize, ThreatClass::Pawn as usize);
        assert_eq!(HandThreatClass::Gold.as_board_class() as usize, ThreatClass::GoldLike as usize);
        assert_eq!(HandThreatClass::Rook.as_board_class() as usize, ThreatClass::Rook as usize);
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
            let should_refresh = needs_hand_threat_refresh(&dirty, king_sq_after, perspective);

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

    /// decode_hand_piece_fb が手駒 BonaPiece から実手駒種を正しく復元する
    #[test]
    fn test_decode_hand_piece_fb_basic() {
        use super::super::bona_piece::{BonaPiece, ExtBonaPiece};

        // Black Silver 1 枚
        let bp = ExtBonaPiece::from_hand(Color::Black, PieceType::Silver, 1);
        assert_eq!(decode_hand_piece_fb(bp.fb), Some((Color::Black, PieceType::Silver)));

        // White Gold 3 枚
        let bp = ExtBonaPiece::from_hand(Color::White, PieceType::Gold, 3);
        assert_eq!(decode_hand_piece_fb(bp.fb), Some((Color::White, PieceType::Gold)));

        // Black Pawn 18 枚 (最大)
        let bp = ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 18);
        assert_eq!(decode_hand_piece_fb(bp.fb), Some((Color::Black, PieceType::Pawn)));

        // ZERO
        assert_eq!(decode_hand_piece_fb(BonaPiece::ZERO), None);
    }

    /// 全手駒種 × 両色 × 各 count を round-trip 検証
    #[test]
    fn test_decode_hand_piece_fb_exhaustive() {
        use super::super::bona_piece::ExtBonaPiece;
        let cases = [
            (PieceType::Pawn, 18),
            (PieceType::Lance, 4),
            (PieceType::Knight, 4),
            (PieceType::Silver, 4),
            (PieceType::Gold, 4),
            (PieceType::Bishop, 2),
            (PieceType::Rook, 2),
        ];
        for &(pt, max_count) in &cases {
            for color in [Color::Black, Color::White] {
                for count in 1..=max_count {
                    let bp = ExtBonaPiece::from_hand(color, pt, count);
                    assert_eq!(
                        decode_hand_piece_fb(bp.fb),
                        Some((color, pt)),
                        "round-trip failed: color={color:?} pt={pt:?} count={count}"
                    );
                }
            }
        }
    }

    /// 盤上駒 BonaPiece は decode_hand_piece_fb で None を返すべき
    #[test]
    fn test_decode_hand_piece_fb_rejects_board() {
        use super::super::bona_piece::BonaPiece;
        use super::super::bona_piece::FE_HAND_END;
        // FE_HAND_END 以降は盤上駒 → None
        let bp = BonaPiece::new(FE_HAND_END as u16);
        assert_eq!(decode_hand_piece_fb(bp), None);
        let bp = BonaPiece::new((FE_HAND_END + 100) as u16);
        assert_eq!(decode_hand_piece_fb(bp), None);
    }

    /// 非捕獲・非 Pawn promotion (Silver → ProSilver) で差分更新が一致
    ///
    /// 非 Pawn 成りは:
    /// - 持ち駒 state 不変 (非捕獲)
    /// - pawn file state 不変 (Pawn でない)
    /// - to_sq の piece class だけ変化 (Silver → GoldLike)
    ///
    /// 既存の source_set 列挙が before/after で正しい piece class を取得できることを確認。
    ///
    /// sfen file 順序: 文字列の左端が file 9、右端が file 1。
    #[test]
    fn test_hand_threat_incremental_noncap_nonpawn_promotion() {
        // 先手銀が 2三 → 2二+ (成り、敵駒なし、持ち駒あり)
        // rank 3 に Silver を file 2 に置く: "7S1" = 7 empty (files 9..3), S at file 2, 1 empty at file 1
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/7S1/9/9/9/9/9/4K4 b R 1").expect("silver promotion sfen");
        let m = Move::from_usi("2c2b+").expect("2c2b+");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Knight promotion (Knight → ProKnight / GoldLike) 非捕獲
    #[test]
    fn test_hand_threat_incremental_noncap_knight_promotion() {
        // 2c の Knight を 1a+ に動かす (2→1 file, 3→1 rank の knight jump)
        // rank 3 file 2 に N: "7N1"
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/7N1/9/9/9/9/9/4K4 b R 1").expect("knight promotion sfen");
        let m = Move::from_usi("2c1a+").expect("2c1a+");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// 成駒捕獲 (ProSilver → 手駒 Silver) のシナリオで hand transition 判定が正しい
    ///
    /// 旧実装の `cap_pt_board.unpromote()` では `decode_board_threat_info_fb` が返す
    /// `PieceType::Gold` をそのまま Gold として扱い、本来更新すべき hand[Silver] が
    /// 無視されていた。`decode_hand_piece_fb` への切替で正しく Silver block の
    /// transition を判定できることを verify_incremental_hand_threat で確認する。
    #[test]
    fn test_hand_threat_incremental_capture_promoted_silver() {
        // Black Silver at 3三 (file 3 rank 2 zero-indexed), captures promoted White Silver at 3二
        // rank 2: 6 empties (f9..f4), +s at f3, 1 empty f2, 1 empty f1 = "6+s2"
        // rank 3: 6 empties (f9..f4), S at f3, 2 empties = "6S2"
        // Black already has Silver in hand (count=2 so capture keeps >=2)
        let mut pos = Position::new();
        pos.set_sfen("4k4/6+s2/6S2/9/9/9/9/9/4K4 b 2S 1")
            .expect("promoted silver capture sfen");
        // 3c3b (non-promoting capture of promoted silver) - wait, Silver capturing ProSilver
        // from=3c (file 3 rank 2), to=3b (file 3 rank 1)
        let m = Move::from_usi("3c3b").expect("3c3b");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Pawn 非捕獲成り: attacker 側 file(from_sq) pawn count 1→0 flip
    #[test]
    fn test_hand_threat_incremental_pawn_promotion_noncap() {
        // Black Pawn at 5d promotes by moving to 5c (empty)
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/4P4/9/9/9/9/4K4 b - 1")
            .expect("pawn promotion noncap sfen");
        let m = Move::from_usi("5d5c+").expect("5d5c+");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Pawn 成り + 非 Pawn 捕獲: attacker 側 file(from_sq) 1→0 flip のみ
    #[test]
    fn test_hand_threat_incremental_pawn_promotion_capture_knight() {
        // Black Pawn at 5d promotes capturing White Knight at 5c
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/4n4/4P4/9/9/9/9/4K4 b - 1")
            .expect("pawn promotion cap knight sfen");
        let m = Move::from_usi("5d5c+").expect("5d5c+");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Pawn 成り + Pawn 捕獲: attacker と cap_color の両 flip
    /// (両方とも file(from_sq) == file(to_sq) で 1→0)
    #[test]
    fn test_hand_threat_incremental_pawn_promotion_capture_pawn() {
        // Black Pawn at 5d captures White Pawn at 5c and promotes
        // Before: Black file 5 pawn = 1, White file 5 pawn = 1
        // After: Black file 5 pawn = 0 (moved+promoted), White file 5 pawn = 0 (captured)
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/4p4/4P4/9/9/9/9/4K4 b - 1")
            .expect("pawn promotion cap pawn sfen");
        let m = Move::from_usi("5d5c+").expect("5d5c+");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// ProPawn (Tokin) を捕獲 → hand[Pawn] 更新 (board 側 pawn file flip なし)
    ///
    /// Black Silver が White ProPawn (Tokin) を 5d で捕獲。
    /// - cap_pt_board = Gold (decode_board_threat_info_fb が Pro* を Gold に正規化)
    /// - cap_pt_hand_base = Pawn (ProPawn は手駒では Pawn に戻る)
    /// - board 側の pawn file flip は無し (ProPawn は pawn_bb に入っていない)
    /// - Black の hand[Pawn] が 1→2 (non-transition 版)
    #[test]
    fn test_hand_threat_incremental_capture_propawn_nontrans() {
        // Black Silver 5e captures White ProPawn (Tokin) at 5d
        // rank 4 (d): 4 empty + tokin + 4 empty = "4+p4"
        // rank 5 (e): 4 empty + S + 4 empty = "4S4"
        // Black has 1 Pawn in hand → after = 2 (non-transition)
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/4+p4/4S4/9/9/9/4K4 b P 1")
            .expect("silver captures propawn sfen");
        let m = Move::from_usi("5e5d").expect("5e5d");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// ProPawn を捕獲 → hand[Pawn] 0→1 transition
    #[test]
    fn test_hand_threat_incremental_capture_propawn_trans() {
        // Black Silver 5e captures White ProPawn at 5d (black has NO pawn)
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/4+p4/4S4/9/9/9/4K4 b - 1")
            .expect("silver captures propawn transition sfen");
        let m = Move::from_usi("5e5d").expect("5e5d");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// 非 Pawn が生歩を捕獲 → pawn file flip + hand[Pawn] 更新
    ///
    /// Black Silver が White Pawn を 5d で捕獲。
    /// - White の file 5 pawn state が 1→0 で flip
    /// - Black の hand[Pawn] が 0→1 (transition) ← fallback
    /// non-transition 版は hand[Pawn] を事前に 1 枚与える
    #[test]
    fn test_hand_threat_incremental_nonpawn_captures_pawn_nontrans() {
        // Black Silver 5e は White Pawn を 5d に取りに行く (silver 5e → 5d)
        // rank 4 (d): 4 empty + p + 4 empty = "4p4"
        // rank 5 (e): 4 empty + S + 4 empty = "4S4"
        // Black has 1 Pawn already → after capture = 2 (non-transition)
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/4p4/4S4/9/9/9/4K4 b P 1")
            .expect("nonpawn captures pawn sfen");
        let m = Move::from_usi("5e5d").expect("5e5d");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// 成り + 捕獲 (両方非 Pawn): Bishop が Knight を捕獲して成る
    #[test]
    fn test_hand_threat_incremental_capture_promotion_bishop_knight() {
        // Black Bishop at 5e captures White Knight at 2b and promotes
        // diagonal: 5e → 4d → 3c → 2b
        // Black has 1 Knight in hand already (non-transition)
        let mut pos = Position::new();
        pos.set_sfen("4k4/1n7/9/9/4B4/9/9/9/4K4 b N 1")
            .expect("bishop captures knight with promotion");
        let m = Move::from_usi("5e2b+").expect("5e2b+");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// 非 Pawn が生歩を捕獲して成る: cap side の pawn file flip + 成り
    #[test]
    fn test_hand_threat_incremental_capture_promotion_with_pawn_cap() {
        // Black Silver at 4d captures White Pawn at 4c and promotes (4d4c+)
        // Black hand has 1 Pawn already (non-transition)
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/5p3/5S3/9/9/9/9/4K4 b P 1")
            .expect("silver captures pawn with promotion");
        let m = Move::from_usi("4d4c+").expect("4d4c+");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Pawn が非 Pawn を捕獲: 動 side の pawn file 不変、cap side の file 不変
    /// (cap_pt が Pawn ではないため、pawn file flip なし)
    #[test]
    fn test_hand_threat_incremental_pawn_captures_nonpawn() {
        // Black Pawn 5e captures White Knight 5d
        // Black hand has 1 Knight already (non-transition)
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/4n4/4P4/9/9/9/4K4 b N 1")
            .expect("pawn captures knight sfen");
        let m = Move::from_usi("5e5d").expect("5e5d");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Pawn が Pawn を捕獲: cap side の file 1→0 flip
    #[test]
    fn test_hand_threat_incremental_pawn_captures_pawn() {
        // Black Pawn 5e captures White Pawn 5d
        // Black hand has 1 Pawn already (non-transition for hand[Pawn])
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/4p4/4P4/9/9/9/4K4 b P 1")
            .expect("pawn captures pawn sfen");
        let m = Move::from_usi("5e5d").expect("5e5d");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Pawn drop: dropper の file(to_sq) が 0→1 で flip
    /// non-transition (hand に複数 Pawn を持つ) ケース
    #[test]
    fn test_hand_threat_incremental_pawn_drop_nontrans() {
        // Black has 2 Pawn in hand, drops one at 5e (file 5, empty)
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b 2P 1").expect("pawn drop sfen");
        let m = Move::from_usi("P*5e").expect("P*5e");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Pawn drop with target: drop 先が attack 範囲に駒を持つ
    #[test]
    fn test_hand_threat_incremental_pawn_drop_with_target() {
        // White Knight at 5d (above black drop at 5e for attack)
        // Black has 2 Pawn in hand, drops at 5e
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/4n4/9/9/9/9/4K4 b 2P 1")
            .expect("pawn drop with target sfen");
        let m = Move::from_usi("P*5e").expect("P*5e");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Pawn drop 1→0 transition: 最後の Pawn を打つ
    #[test]
    fn test_hand_threat_incremental_pawn_drop_1to0() {
        // Black has 1 Pawn in hand, drops it (transition)
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b P 1").expect("pawn drop 1->0 sfen");
        let m = Move::from_usi("P*5e").expect("P*5e");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// 同じパターンで White も Pawn を hand に持つ (both sides have pawn in hand)
    #[test]
    fn test_hand_threat_incremental_nonpawn_captures_pawn_both_hand() {
        // Black Silver 5e captures White Pawn at 5d
        // Both sides have Pawn in hand already
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/4p4/4S4/9/9/9/4K4 b Pp 1").expect("both hand pawn sfen");
        let m = Move::from_usi("5e5d").expect("5e5d");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Capture 0→1 transition (us captures non-Pawn piece, gains first of type)
    #[test]
    fn test_hand_threat_incremental_capture_0_1_transition() {
        // Black Silver at 3c captures White Silver at 3b.
        // Black initially has NO Silver in hand, gains 1 after capture.
        // rank 2 (b): "6s2" = 6 empty, s at file 3, 2 empty
        // rank 3 (c): "6S2" = 6 empty, S at file 3, 2 empty
        let mut pos = Position::new();
        pos.set_sfen("4k4/6s2/6S2/9/9/9/9/9/4K4 b - 1")
            .expect("silver capture 0->1 sfen");
        let m = Move::from_usi("3c3b").expect("3c3b");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Capture 0→1 transition (Bishop, slider with many features)
    #[test]
    fn test_hand_threat_incremental_capture_0_1_bishop() {
        // Black Bishop at 5e captures White Bishop at 1a.
        // Bishop diagonal: 5e → 4d → 3c → 2b → 1a (if all empty)
        // Black gains first Bishop.
        let mut pos = Position::new();
        pos.set_sfen("8b/9/9/9/4B4/9/9/9/4K4 b - 1").expect("bishop capture 0->1 sfen");
        // 白キングがないと違反になるかも; k を追加
        pos.set_sfen("3k4b/9/9/9/4B4/9/9/9/4K4 b - 1")
            .expect("bishop capture 0->1 sfen with kings");
        let m = Move::from_usi("5e1a").expect("5e1a");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Drop 1→0 transition (dropper has exactly 1 piece before drop)
    #[test]
    fn test_hand_threat_incremental_drop_1_0_transition() {
        // Black has exactly 1 Silver in hand, drops it at 5e.
        // This causes hand[Silver] 1→0 transition.
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b S 1").expect("silver drop 1->0 sfen");
        let m = Move::from_usi("S*5e").expect("S*5e");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// 非 Pawn drop (Silver drop) 非遷移 (before hand count >= 2)
    #[test]
    fn test_hand_threat_incremental_noncap_silver_drop() {
        // Black has 2 Silver in hand, drops one at 5五 (5e, empty sq)
        // White King at 5一, Black King at 5九, nothing else on board
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b 2S 1").expect("silver drop sfen");
        let m = Move::from_usi("S*5e").expect("S*5e");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Rook drop (slider, non-transition) が正しく動作する
    #[test]
    fn test_hand_threat_incremental_noncap_rook_drop() {
        // Black has 2 Rook in hand, drops one at 5e
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b 2R 1").expect("rook drop sfen");
        let m = Move::from_usi("R*5e").expect("R*5e");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Silver drop が実際に attack pair を発生させるケース (target 駒あり)
    #[test]
    fn test_hand_threat_incremental_drop_with_targets() {
        // Black has 2 Silver in hand, drops at 3三.
        // 3三 Silver attacks 2二, 4二, 2四, 4四. Put White knight at 2二 for an attack target.
        // rank 1 = "4k4", rank 2 = "7n1" (n at file 2), rank 9 = "4K4"
        let mut pos = Position::new();
        pos.set_sfen("4k4/7n1/9/9/9/9/9/9/4K4 b 2S 1")
            .expect("silver drop with target sfen");
        let m = Move::from_usi("S*3c").expect("S*3c");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// 成桂捕獲 (ProKnight → 手駒 Knight)
    #[test]
    fn test_hand_threat_incremental_capture_promoted_knight() {
        // Black Silver at 3b captures ProKnight at 2a (knight promoted to gold-class)
        // Wait, use Gold capturing ProKnight for simplicity
        // rank 1 (a): "4k3+n" = 4 empty, k, 3 empty, +n at f1
        // rank 2 (b): "8G" = 8 empty, G at f1
        // Black already has Knight count = 2
        let mut pos = Position::new();
        pos.set_sfen("4k3+n/8G/9/9/9/9/9/9/4K4 b 2N 1")
            .expect("promoted knight capture sfen");
        // Gold at 1b captures ProKnight at 1a
        let m = Move::from_usi("1b1a").expect("1b1a");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// Bishop promotion (Bishop → Horse) 非捕獲、slider の attack 変化あり
    #[test]
    fn test_hand_threat_incremental_noncap_bishop_promotion() {
        // Black Bishop at 5五 → 2二+ (対角移動、非捕獲、成り)
        // 持ち駒に Rook (slider) 持たせて、slider drop の ray 変化も exercise
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/4B4/9/9/9/4K4 b R 1").expect("bishop promotion sfen");
        let m = Move::from_usi("5e2b+").expect("5e2b+");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// King move within HM mirror (capture なし)
    /// king は ThreatClass に含まれないため target としては filter されるが、
    /// from_sq/to_sq の occupancy 変化は通常の board move と同じく diff される。
    #[test]
    fn test_hand_threat_incremental_king_move_within_mirror() {
        // Black King at 5h, moves to 5g. White king at 5a.
        // Both kings in file 5 (mid file), within HM mirror zone.
        // hand に Silver と Pawn を持たせて diff の中身が空でないようにする
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/4K4/9 b SP 1").expect("king move sfen");
        let m = Move::from_usi("5h5g").expect("5h5g");
        verify_incremental_hand_threat(&mut pos, m);
    }

    /// King move with capture (within mirror)
    #[test]
    fn test_hand_threat_incremental_king_capture_within_mirror() {
        // Black King at 5h captures white pawn at 5g.
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/4p4/4K4/9 b S 1").expect("king capture sfen");
        let m = Move::from_usi("5h5g").expect("5h5g");
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

    /// 中盤局面から複数手を順次 do_move し、各手で diff vs full rebuild の一致を確認する
    /// (verify_nnue_accumulator の startpos 起点では拾えない中盤特有のパターンを補強)
    #[test]
    fn test_hand_threat_incremental_midgame_sequence() {
        use crate::movegen::{MoveList, generate_legal_all};

        let sfens = [
            "l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        ];
        for sfen in &sfens {
            let mut pos = Position::new();
            pos.set_sfen(sfen).expect("sfen");
            // 各局面で 12 手まで決定的に first legal move を辿り、各 do_move で
            // verify_incremental_hand_threat により diff vs full rebuild の一致を確認する。
            for _ in 0..12 {
                let mut moves = MoveList::new();
                generate_legal_all(&pos, &mut moves);
                if moves.is_empty() {
                    break;
                }
                let m = moves[0];
                verify_incremental_hand_threat(&mut pos, m);
                let gc = pos.gives_check(m);
                pos.do_move(m, gc);
            }
        }
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
        pos.set_sfen("4k4/9/9/9/4R4/9/9/9/4K4 b P 1").expect("minimal test sfen");

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
