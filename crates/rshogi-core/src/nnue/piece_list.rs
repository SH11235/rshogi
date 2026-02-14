//! PieceList - 全40駒の BonaPiece 管理と逆引きテーブル
//!
//! YaneuraOu の EvalList に相当する。
//! 全40駒（歩18, 香4, 桂4, 銀4, 金4, 角2, 飛2, 玉2）の BonaPiece を
//! PieceNumber で管理し、Square → PieceNumber / BonaPiece(fb) → PieceNumber の
//! 逆引きテーブルで O(1) アクセスを実現する。

use super::bona_piece::{BonaPiece, ExtBonaPiece, FE_HAND_END};
use crate::types::Square;

/// 駒番号（全40駒に対する一意な番号）
///
/// YaneuraOu の PieceNumber に準拠した番号割り当て:
/// - 歩: 0-17 (18枚)
/// - 香: 18-21 (4枚)
/// - 桂: 22-25 (4枚)
/// - 銀: 26-29 (4枚)
/// - 金: 30-33 (4枚)
/// - 角: 34-35 (2枚)
/// - 飛: 36-37 (2枚)
/// - 玉: 38-39 (2枚)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct PieceNumber(pub u8);

impl PieceNumber {
    /// 無効値
    pub const NONE: PieceNumber = PieceNumber(u8::MAX);

    /// 先手玉の PieceNumber
    pub const KING: u8 = 38;

    /// 駒の総数
    pub const NB: usize = 40;
}

/// 駒種ごとの PieceNumber 開始位置
/// index: 0=歩, 1=香, 2=桂, 3=銀, 4=金, 5=角, 6=飛
/// (手駒の PieceType 順: Pawn=1..Rook=6, Gold=7 → index = pt-1, ただし Gold は 4)
const PIECE_NUMBER_BASE: [u8; 8] = [
    0,  // 歩: 0-17
    18, // 香: 18-21
    22, // 桂: 22-25
    26, // 銀: 26-29
    30, // 金: 30-33
    34, // 角: 34-35
    36, // 飛: 36-37
    38, // 玉: 38-39
];

/// PieceType から PIECE_NUMBER_BASE のインデックスへ変換
///
/// PieceType::Pawn(1) → 0, Lance(2) → 1, Knight(3) → 2, Silver(4) → 3,
/// Bishop(5) → 5, Rook(6) → 6, Gold(7) → 4, King(8) → 7
const PT_TO_BASE_INDEX: [u8; 9] = [
    u8::MAX, // 0: unused
    0,       // 1: Pawn
    1,       // 2: Lance
    2,       // 3: Knight
    3,       // 4: Silver
    5,       // 5: Bishop
    6,       // 6: Rook
    4,       // 7: Gold
    7,       // 8: King
];

/// PieceList - 全40駒の BonaPiece 管理テーブル
///
/// YaneuraOu の EvalList に相当。
/// 合計 ~332 bytes で L1 キャッシュ内に収まる。
#[derive(Clone)]
pub struct PieceList {
    /// 各 PieceNumber の BonaPiece (先手視点)
    piece_list_fb: [BonaPiece; PieceNumber::NB],
    /// 各 PieceNumber の BonaPiece (後手視点)
    piece_list_fw: [BonaPiece; PieceNumber::NB],
    /// 盤上逆引き: Square → PieceNumber
    piece_no_on_board: [PieceNumber; Square::NUM + 1],
    /// 手駒逆引き: BonaPiece(fb) → PieceNumber
    piece_no_on_hand: [PieceNumber; FE_HAND_END],
}

impl PieceList {
    /// 全要素を無効値で初期化
    pub fn new() -> Self {
        Self {
            piece_list_fb: [BonaPiece::ZERO; PieceNumber::NB],
            piece_list_fw: [BonaPiece::ZERO; PieceNumber::NB],
            piece_no_on_board: [PieceNumber::NONE; Square::NUM + 1],
            piece_no_on_hand: [PieceNumber::NONE; FE_HAND_END],
        }
    }

    /// 盤上駒を設定し逆引きテーブルを更新
    #[inline]
    pub fn put_piece_on_board(&mut self, piece_no: PieceNumber, bp: ExtBonaPiece, sq: Square) {
        self.piece_list_fb[piece_no.0 as usize] = bp.fb;
        self.piece_list_fw[piece_no.0 as usize] = bp.fw;
        self.piece_no_on_board[sq.index()] = piece_no;
    }

    /// 手駒を設定し逆引きテーブルを更新
    #[inline]
    pub fn put_piece_on_hand(&mut self, piece_no: PieceNumber, bp: ExtBonaPiece) {
        self.piece_list_fb[piece_no.0 as usize] = bp.fb;
        self.piece_list_fw[piece_no.0 as usize] = bp.fw;
        debug_assert!(
            (bp.fb.value() as usize) < FE_HAND_END,
            "fb ({}) out of hand range (< {})",
            bp.fb.value(),
            FE_HAND_END
        );
        self.piece_no_on_hand[bp.fb.value() as usize] = piece_no;
    }

    /// 盤上逆引き: Square → PieceNumber
    #[inline]
    pub fn piece_no_of_board(&self, sq: Square) -> PieceNumber {
        self.piece_no_on_board[sq.index()]
    }

    /// 手駒逆引き: BonaPiece(fb) → PieceNumber
    #[inline]
    pub fn piece_no_of_hand(&self, fb: BonaPiece) -> PieceNumber {
        debug_assert!(
            (fb.value() as usize) < FE_HAND_END,
            "fb ({}) out of hand range (< {})",
            fb.value(),
            FE_HAND_END
        );
        self.piece_no_on_hand[fb.value() as usize]
    }

    /// PieceNumber → ExtBonaPiece 取得
    #[inline]
    pub fn bona_piece(&self, piece_no: PieceNumber) -> ExtBonaPiece {
        ExtBonaPiece {
            fb: self.piece_list_fb[piece_no.0 as usize],
            fw: self.piece_list_fw[piece_no.0 as usize],
        }
    }

    /// fb 配列への参照
    #[inline]
    pub fn piece_list_fb(&self) -> &[BonaPiece; PieceNumber::NB] {
        &self.piece_list_fb
    }

    /// fw 配列への参照
    #[inline]
    pub fn piece_list_fw(&self) -> &[BonaPiece; PieceNumber::NB] {
        &self.piece_list_fw
    }
}

impl Default for PieceList {
    fn default() -> Self {
        Self::new()
    }
}

/// PieceType から PieceNumber の開始位置を取得
///
/// 成駒は生駒と同じ PieceNumber 範囲を使用するため、unpromote して参照する。
/// 呼び出し側で駒種ごとのカウンタを管理し、base + count で PieceNumber を算出する。
#[inline]
pub fn piece_number_base(pt: crate::types::PieceType) -> u8 {
    let raw_pt = pt.unpromote() as u8;
    debug_assert!(raw_pt <= 8, "Invalid PieceType: {raw_pt}");
    let idx = PT_TO_BASE_INDEX[raw_pt as usize];
    debug_assert!(idx != u8::MAX, "Unsupported PieceType for piece_number_base");
    PIECE_NUMBER_BASE[idx as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PieceType;

    #[test]
    fn test_piece_number_base() {
        assert_eq!(piece_number_base(PieceType::Pawn), 0);
        assert_eq!(piece_number_base(PieceType::Lance), 18);
        assert_eq!(piece_number_base(PieceType::Knight), 22);
        assert_eq!(piece_number_base(PieceType::Silver), 26);
        assert_eq!(piece_number_base(PieceType::Gold), 30);
        assert_eq!(piece_number_base(PieceType::Bishop), 34);
        assert_eq!(piece_number_base(PieceType::Rook), 36);
        assert_eq!(piece_number_base(PieceType::King), 38);
    }

    #[test]
    fn test_piece_number_base_promoted() {
        // 成駒は生駒と同じ base を返す
        assert_eq!(piece_number_base(PieceType::ProPawn), piece_number_base(PieceType::Pawn));
        assert_eq!(piece_number_base(PieceType::Horse), piece_number_base(PieceType::Bishop));
        assert_eq!(piece_number_base(PieceType::Dragon), piece_number_base(PieceType::Rook));
    }

    #[test]
    fn test_piece_list_put_and_get() {
        let mut pl = PieceList::new();
        let pn = PieceNumber(0);
        let bp = ExtBonaPiece::new(BonaPiece::new(100), BonaPiece::new(200));
        let sq = Square::SQ_55;

        pl.put_piece_on_board(pn, bp, sq);

        assert_eq!(pl.piece_no_of_board(sq), pn);
        assert_eq!(pl.bona_piece(pn), bp);
        assert_eq!(pl.piece_list_fb()[0], BonaPiece::new(100));
        assert_eq!(pl.piece_list_fw()[0], BonaPiece::new(200));
    }

    #[test]
    fn test_piece_list_hand() {
        let mut pl = PieceList::new();
        let pn = PieceNumber(0);
        let bp = ExtBonaPiece::new(BonaPiece::new(1), BonaPiece::new(20)); // F_HAND_PAWN count=1

        pl.put_piece_on_hand(pn, bp);

        assert_eq!(pl.piece_no_of_hand(BonaPiece::new(1)), pn);
        assert_eq!(pl.bona_piece(pn), bp);
    }
}
