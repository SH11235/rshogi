//! 王手候補駒テーブル (CheckCandidateBB)
//!
//! YaneuraOu準拠の事前計算テーブル。敵玉の位置ごとに、直接王手を
//! 実現できる可能性のある駒の候補マスを保持する。
//! generate_checks で非blocker駒のフィルタリングに使用し、
//! 全駒走査を回避して王手生成を高速化する。

use std::sync::OnceLock;

use crate::types::{Color, PieceType, Square};

use super::sliders::{bishop_effect, horse_effect, lance_step_effect};
use super::{
    Bitboard, GOLD_EFFECT, KING_EFFECT, KNIGHT_EFFECT, PAWN_EFFECT, RANK_BB, SILVER_EFFECT,
};

/// 王手候補テーブルの駒種数 (PAWN, LANCE, KNIGHT, SILVER, BISHOP, HORSE, GOLD)
const CHECK_CANDIDATE_NUM: usize = 7;

/// 王手候補テーブル [Color][PieceTypeIndex][KingSquare]
static CHECK_CANDIDATE_TABLE: OnceLock<
    [[[Bitboard; Square::NUM]; CHECK_CANDIDATE_NUM]; Color::NUM],
> = OnceLock::new();

fn check_candidate_table() -> &'static [[[Bitboard; Square::NUM]; CHECK_CANDIDATE_NUM]; Color::NUM]
{
    CHECK_CANDIDATE_TABLE.get_or_init(init_check_candidate)
}

/// PieceType → テーブルインデックス変換
#[inline]
fn pt_to_index(pt: PieceType) -> usize {
    match pt {
        PieceType::Pawn => 0,
        PieceType::Lance => 1,
        PieceType::Knight => 2,
        PieceType::Silver => 3,
        PieceType::Bishop => 4,
        PieceType::Horse => 5,
        PieceType::Gold => 6,
        _ => unreachable!("check_candidate_bb: unsupported piece type"),
    }
}

/// 指定マス(ksq)にいる敵玉に対して直接王手可能な候補位置を返す
///
/// ROOK/DRAGON は全域候補のため本テーブルに含めない。
/// 呼び出し側で無条件に含める。
#[inline]
pub fn check_candidate_bb(us: Color, pt: PieceType, ksq: Square) -> Bitboard {
    check_candidate_table()[us.index()][pt_to_index(pt)][ksq.index()]
}

/// 敵陣ビットボード（成りの条件判定用）
fn enemy_field(us: Color) -> Bitboard {
    match us {
        Color::Black => RANK_BB[0] | RANK_BB[1] | RANK_BB[2],
        Color::White => RANK_BB[6] | RANK_BB[7] | RANK_BB[8],
    }
}

// ヘルパー: テーブル参照の近接駒利き
#[inline]
fn pawn_eff(c: Color, sq: Square) -> Bitboard {
    PAWN_EFFECT[c.index()][sq.index()]
}
#[inline]
fn knight_eff(c: Color, sq: Square) -> Bitboard {
    KNIGHT_EFFECT[c.index()][sq.index()]
}
#[inline]
fn silver_eff(c: Color, sq: Square) -> Bitboard {
    SILVER_EFFECT[c.index()][sq.index()]
}
#[inline]
fn gold_eff(c: Color, sq: Square) -> Bitboard {
    GOLD_EFFECT[c.index()][sq.index()]
}
#[inline]
fn king_eff(sq: Square) -> Bitboard {
    KING_EFFECT[sq.index()]
}

/// 指定筋・段からSquareを生成（盤内チェック済み前提）
///
/// # Safety
/// file: 0..=8, rank: 0..=8 であること
#[inline]
unsafe fn make_square(file: i32, rank: i32) -> Square {
    debug_assert!((0..9).contains(&file) && (0..9).contains(&rank));
    Square::from_u8_unchecked((file * 9 + rank) as u8)
}

fn init_check_candidate() -> [[[Bitboard; Square::NUM]; CHECK_CANDIDATE_NUM]; Color::NUM] {
    let mut result = [[[Bitboard::EMPTY; Square::NUM]; CHECK_CANDIDATE_NUM]; Color::NUM];

    for us in [Color::Black, Color::White] {
        let them = !us;
        let ef = enemy_field(us);

        for ksq in Square::all() {
            let ksq_bb = Bitboard::from_square(ksq);
            // 敵玉位置に敵の金を置いた利き & 敵陣
            let enemy_gold = gold_eff(them, ksq) & ef;

            // === PAWN (index 0) ===
            // 不成: pawnEffect(them, ksq) の各マスから pawn 逆利きで到達可能
            // 成り: enemyGold の各マスから pawn 逆利きで到達可能
            {
                let mut target = Bitboard::EMPTY;
                for sq in pawn_eff(them, ksq).iter() {
                    target |= pawn_eff(them, sq);
                }
                for sq in enemy_gold.iter() {
                    target |= pawn_eff(them, sq);
                }
                result[us.index()][0][ksq.index()] = target & !ksq_bb;
            }

            // === LANCE (index 1) ===
            // 不成: lanceStepEffect(them, ksq)（同筋の後方全域）
            // 成り: 敵陣内の王なら隣接筋の lanceStepEffect も追加
            {
                let mut target = lance_step_effect(them, ksq);
                if ef.contains(ksq) {
                    let file = ksq.file().index() as i32;
                    let rank = ksq.rank().index() as i32;
                    if file > 0 {
                        // SAFETY: file-1 >= 0, rank は元のksqと同じで盤内
                        let adj = unsafe { make_square(file - 1, rank) };
                        target |= lance_step_effect(them, adj);
                    }
                    if file < 8 {
                        // SAFETY: file+1 <= 8, rank は元のksqと同じで盤内
                        let adj = unsafe { make_square(file + 1, rank) };
                        target |= lance_step_effect(them, adj);
                    }
                }
                result[us.index()][1][ksq.index()] = target;
            }

            // === KNIGHT (index 2) ===
            // 不成: knightEffect(them, ksq) の各マスから knight 逆利き
            // 成り: enemyGold の各マスから knight 逆利き
            {
                let mut target = Bitboard::EMPTY;
                let combined = knight_eff(them, ksq) | enemy_gold;
                for sq in combined.iter() {
                    target |= knight_eff(them, sq);
                }
                result[us.index()][2][ksq.index()] = target & !ksq_bb;
            }

            // === SILVER (index 3) ===
            // 不成: silverEffect(them, ksq) の各マスから silver 逆利き
            // 成り(移動先が敵陣): enemyGold の各マスから silver 逆利き
            // 成り(移動元が敵陣): goldEffect(them, ksq) の各マスから敵陣内 silver 逆利き
            {
                let mut target = Bitboard::EMPTY;
                for sq in silver_eff(them, ksq).iter() {
                    target |= silver_eff(them, sq);
                }
                for sq in enemy_gold.iter() {
                    target |= silver_eff(them, sq);
                }
                for sq in gold_eff(them, ksq).iter() {
                    target |= ef & silver_eff(them, sq);
                }
                result[us.index()][3][ksq.index()] = target & !ksq_bb;
            }

            // === BISHOP (index 4) ===
            // 不成: bishopEffect(ksq, empty) の各マスから bishop 空盤利き
            // 成り(移動先が敵陣): kingEffect(ksq) & enemyField の各マスから bishop 空盤利き
            // 成り(移動元が敵陣): kingEffect(ksq) の各マスから敵陣内 bishop 空盤利き
            {
                let mut target = Bitboard::EMPTY;
                let bishop_step = bishop_effect(ksq, Bitboard::EMPTY);
                for sq in bishop_step.iter() {
                    target |= bishop_effect(sq, Bitboard::EMPTY);
                }
                for sq in (king_eff(ksq) & ef).iter() {
                    target |= bishop_effect(sq, Bitboard::EMPTY);
                }
                for sq in king_eff(ksq).iter() {
                    target |= ef & bishop_effect(sq, Bitboard::EMPTY);
                }
                result[us.index()][4][ksq.index()] = target & !ksq_bb;
            }

            // === HORSE (index 5, YaneuraOuではROOKスロットに格納) ===
            // horseEffect(ksq, empty) の各マスから horse 空盤利き
            {
                let mut target = Bitboard::EMPTY;
                let horse_step = horse_effect(ksq, Bitboard::EMPTY);
                for sq in horse_step.iter() {
                    target |= horse_effect(sq, Bitboard::EMPTY);
                }
                result[us.index()][5][ksq.index()] = target & !ksq_bb;
            }

            // === GOLD (index 6) ===
            // goldEffect(them, ksq) の各マスから gold 逆利き
            {
                let mut target = Bitboard::EMPTY;
                for sq in gold_eff(them, ksq).iter() {
                    target |= gold_eff(them, sq);
                }
                result[us.index()][6][ksq.index()] = target & !ksq_bb;
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank};

    #[test]
    fn test_check_candidate_pawn_basic() {
        // 先手の歩が5五の敵玉に王手できる候補位置をテスト
        // check_candidate_bb は「動くと王手になれる候補位置」を返す
        // 歩は1マス前進: 5七→5六(王手マス)→5五(玉) なので候補は5七
        let ksq = Square::new(File::File5, Rank::Rank5);
        let bb = check_candidate_bb(Color::Black, PieceType::Pawn, ksq);
        let sq_5g = Square::new(File::File5, Rank::Rank7);
        assert!(bb.contains(sq_5g), "5七に歩の王手候補がない");
        // 5六は候補ではない（5六の歩が動くと5五=玉マスで、王手ではなく捕獲）
        let sq_5f = Square::new(File::File5, Rank::Rank6);
        assert!(!bb.contains(sq_5f), "5六が候補に含まれている（不正）");
    }

    #[test]
    fn test_check_candidate_gold_basic() {
        let ksq = Square::new(File::File5, Rank::Rank5);
        let bb = check_candidate_bb(Color::Black, PieceType::Gold, ksq);
        // 金の王手候補は金の利きの逆利き（広い範囲）
        assert!(!bb.is_empty(), "金の王手候補が空");
        // 敵玉自身は含まれない
        assert!(!bb.contains(ksq), "敵玉位置が候補に含まれている");
    }

    #[test]
    fn test_check_candidate_rook_not_in_table() {
        // ROOK/DRAGON はテーブルに含めない（全域候補のため）
        // check_candidate_bb を ROOK で呼ぶとpanic
        let result = std::panic::catch_unwind(|| {
            check_candidate_bb(
                Color::Black,
                PieceType::Rook,
                Square::new(File::File5, Rank::Rank5),
            );
        });
        assert!(result.is_err(), "ROOK で check_candidate_bb が呼べてしまう");
    }

    #[test]
    fn test_check_candidate_lance_promotion() {
        // 先手の香が敵陣内の玉（1段目付近）に成りで王手できるケース
        // 1段目の玉に対して隣接筋からも候補がある
        let ksq = Square::new(File::File5, Rank::Rank1);
        let bb = check_candidate_bb(Color::Black, PieceType::Lance, ksq);
        // 同筋の2段目以降が候補
        let sq_5b = Square::new(File::File5, Rank::Rank2);
        assert!(bb.contains(sq_5b), "同筋が候補にない");
        // 敵陣なので隣接筋も候補
        let sq_4b = Square::new(File::File4, Rank::Rank2);
        assert!(bb.contains(sq_4b), "隣接筋が候補にない");
    }

    #[test]
    fn test_check_candidate_bishop_basic() {
        let ksq = Square::new(File::File5, Rank::Rank5);
        let bb = check_candidate_bb(Color::Black, PieceType::Bishop, ksq);
        // 角の候補は斜めライン上の広い範囲
        assert!(!bb.is_empty(), "角の王手候補が空");
        assert!(!bb.contains(ksq), "敵玉位置が候補に含まれている");
    }
}
