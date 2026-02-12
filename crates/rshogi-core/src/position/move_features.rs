//! 指し手の特徴量抽出（解説生成用）
//!
//! `move-features` フィーチャーフラグで有効化される。
//! アプリ側の AI 解説生成で使用するため、通常のエンジンビルドには含まれない。

use serde::Serialize;

use super::Position;
use crate::types::{Move, PieceType};

/// 指し手の特徴量（JSON シリアライズ対応）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveFeatures {
    /// 動かした駒の基本駒種（"P","L","N","S","B","R","G","K"）
    pub moved_piece: String,
    /// 動かした駒が既に成り駒だったか
    pub moved_piece_promoted: bool,
    /// 駒取りかどうか
    pub is_capture: bool,
    /// 取った駒の基本駒種
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_piece: Option<String>,
    /// 取った駒が成り駒だったか
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_piece_promoted: Option<bool>,
    /// 成りかどうか
    pub is_promote: bool,
    /// 駒打ちかどうか
    pub is_drop: bool,
    /// 王手かどうか
    pub is_check: bool,
}

impl Position {
    /// 指し手の特徴量を抽出する。
    ///
    /// パスや無効手の場合は `None` を返す。
    pub fn extract_move_features(&self, m: Move) -> Option<MoveFeatures> {
        if !m.is_normal() {
            return None;
        }

        let is_check = self.gives_check(m);

        if m.is_drop() {
            let pt = m.drop_piece_type();
            return Some(MoveFeatures {
                moved_piece: piece_type_to_short(pt),
                moved_piece_promoted: false,
                is_capture: false,
                captured_piece: None,
                captured_piece_promoted: None,
                is_promote: false,
                is_drop: true,
                is_check,
            });
        }

        // 通常移動
        let from = m.from();
        let to = m.to();
        let piece_on_from = self.piece_on(from);
        if piece_on_from.is_none() {
            return None;
        }

        let pt = piece_on_from.piece_type();
        let base_pt = pt.unpromote();
        let is_promoted = pt.is_promoted();

        let piece_on_to = self.piece_on(to);
        let (is_capture, captured_piece, captured_piece_promoted) = if piece_on_to.is_some() {
            let cpt = piece_on_to.piece_type();
            (true, Some(piece_type_to_short(cpt.unpromote())), Some(cpt.is_promoted()))
        } else {
            (false, None, None)
        };

        Some(MoveFeatures {
            moved_piece: piece_type_to_short(base_pt),
            moved_piece_promoted: is_promoted,
            is_capture,
            captured_piece,
            captured_piece_promoted,
            is_promote: m.is_promote(),
            is_drop: false,
            is_check,
        })
    }
}

fn piece_type_to_short(pt: PieceType) -> String {
    match pt {
        PieceType::Pawn | PieceType::ProPawn => "P",
        PieceType::Lance | PieceType::ProLance => "L",
        PieceType::Knight | PieceType::ProKnight => "N",
        PieceType::Silver | PieceType::ProSilver => "S",
        PieceType::Bishop | PieceType::Horse => "B",
        PieceType::Rook | PieceType::Dragon => "R",
        PieceType::Gold => "G",
        PieceType::King => "K",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use crate::position::SFEN_HIRATE;
    use crate::types::Move;

    use super::*;

    fn setup_hirate() -> Position {
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        pos
    }

    fn apply_moves(pos: &mut Position, moves: &[&str]) {
        for mv_str in moves {
            let m = Move::from_usi(mv_str).unwrap();
            let gc = pos.gives_check(m);
            pos.do_move(m, gc);
        }
    }

    #[test]
    fn test_normal_move() {
        let pos = setup_hirate();
        // 7六歩（7g7f）
        let m = Move::from_usi("7g7f").unwrap();
        let features = pos.extract_move_features(m).unwrap();

        assert_eq!(features.moved_piece, "P");
        assert!(!features.moved_piece_promoted);
        assert!(!features.is_capture);
        assert!(features.captured_piece.is_none());
        assert!(!features.is_promote);
        assert!(!features.is_drop);
        assert!(!features.is_check);
    }

    #[test]
    fn test_capture() {
        let mut pos = setup_hirate();
        // 角道を開けて角交換
        apply_moves(&mut pos, &["7g7f", "3c3d", "8h2b+"]);
        // 2b+: 角が2二の銀を取って成る（前の手で処理済み）
        // 既に適用済みなので、直前の手を見る
        // 代わりに新しい局面で確認: 3三角成を単独でテスト
        let mut pos2 = setup_hirate();
        apply_moves(&mut pos2, &["7g7f", "3c3d"]);
        // 8八角で2二角成
        let m = Move::from_usi("8h2b+").unwrap();
        let features = pos2.extract_move_features(m).unwrap();

        assert_eq!(features.moved_piece, "B");
        assert!(!features.moved_piece_promoted);
        assert!(features.is_capture);
        assert_eq!(features.captured_piece.as_deref(), Some("B"));
        assert!(!features.captured_piece_promoted.unwrap());
        assert!(features.is_promote);
        assert!(!features.is_drop);
    }

    #[test]
    fn test_drop() {
        let mut pos = setup_hirate();
        // 角交換して歩を打つシナリオ
        apply_moves(&mut pos, &["7g7f", "3c3d", "8h2b+", "3a2b", "B*5e"]);
        // B*5e を再現: 角交換後に角を打つ
        let mut pos2 = setup_hirate();
        apply_moves(&mut pos2, &["7g7f", "3c3d", "8h2b+", "3a2b"]);
        let m = Move::from_usi("B*5e").unwrap();
        let features = pos2.extract_move_features(m).unwrap();

        assert_eq!(features.moved_piece, "B");
        assert!(!features.moved_piece_promoted);
        assert!(!features.is_capture);
        assert!(features.captured_piece.is_none());
        assert!(!features.is_promote);
        assert!(features.is_drop);
    }

    #[test]
    fn test_promoted_piece_move() {
        // 馬（成り角）が移動するテスト
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/4+B4/9/9/9/4K4 b - 1").unwrap();
        // 5e の馬を 4d に移動
        let m = Move::from_usi("5e4d").unwrap();
        let features = pos.extract_move_features(m).unwrap();

        assert_eq!(features.moved_piece, "B");
        assert!(features.moved_piece_promoted); // 馬 = 成り角
        assert!(!features.is_capture);
        assert!(!features.is_promote); // 既に成っている
        assert!(!features.is_drop);
    }

    #[test]
    fn test_pass_returns_none() {
        let pos = setup_hirate();
        let features = pos.extract_move_features(Move::PASS);
        assert!(features.is_none());
    }

    #[test]
    fn test_none_returns_none() {
        let pos = setup_hirate();
        let features = pos.extract_move_features(Move::NONE);
        assert!(features.is_none());
    }

    #[test]
    fn test_check_detection() {
        // 金打ちで王手
        let mut pos = Position::new();
        // 先手に持ち金があり、後手玉が5aの局面
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b G 1").unwrap();
        // G*5b: 金を5bに打つ → 5aの玉に王手
        let m = Move::from_usi("G*5b").unwrap();
        let features = pos.extract_move_features(m).unwrap();

        assert!(features.is_check);
        assert_eq!(features.moved_piece, "G");
        assert!(features.is_drop);
    }
}
