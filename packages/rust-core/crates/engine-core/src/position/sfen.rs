//! SFEN形式の解析・出力

use crate::types::{Color, File, Piece, PieceType, Rank, Square};

use super::pos::Position;
use super::zobrist::{zobrist_hand, zobrist_psq, zobrist_side};

/// 平手初期局面のSFEN
pub const SFEN_HIRATE: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

/// SFENパースエラー
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfenError {
    /// 盤面の形式が不正
    Board(String),
    /// 手番の形式が不正
    SideToMove(String),
    /// 手駒の形式が不正
    Hand(String),
    /// 手数の形式が不正
    Ply(String),
}

impl std::fmt::Display for SfenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SfenError::Board(s) => write!(f, "Invalid board: {s}"),
            SfenError::SideToMove(s) => write!(f, "Invalid side to move: {s}"),
            SfenError::Hand(s) => write!(f, "Invalid hand: {s}"),
            SfenError::Ply(s) => write!(f, "Invalid ply: {s}"),
        }
    }
}

impl std::error::Error for SfenError {}

impl Position {
    /// 平手初期局面を設定
    pub fn set_hirate(&mut self) {
        self.set_sfen(SFEN_HIRATE).unwrap();
    }

    /// SFEN文字列から局面を設定
    pub fn set_sfen(&mut self, sfen: &str) -> Result<(), SfenError> {
        // 局面をクリア
        *self = Position::new();

        let parts: Vec<&str> = sfen.split_whitespace().collect();
        if parts.len() < 3 {
            return Err(SfenError::Board("SFEN must have at least 3 parts".to_string()));
        }

        // 1. 盤面
        self.parse_board(parts[0])?;

        // 2. 手番
        match parts[1] {
            "b" => self.side_to_move = Color::Black,
            "w" => self.side_to_move = Color::White,
            _ => {
                return Err(SfenError::SideToMove(format!(
                    "Expected 'b' or 'w', got '{}'",
                    parts[1]
                )))
            }
        }

        // 3. 手駒
        self.parse_hand(parts[2])?;

        // 4. 手数（オプション）
        if parts.len() >= 4 {
            self.game_ply = parts[3].parse().map_err(|_| SfenError::Ply(parts[3].to_string()))?;
        } else {
            self.game_ply = 1;
        }

        // ハッシュ値の計算
        self.compute_hash();

        // pin情報と王手マスの更新
        self.update_blockers_and_pinners();
        self.update_check_squares();

        // 王手駒の計算
        let them = !self.side_to_move;
        self.state.checkers =
            self.attackers_to_c(self.king_square[self.side_to_move.index()], them);

        Ok(())
    }

    /// 現局面のSFEN文字列を取得
    pub fn to_sfen(&self) -> String {
        let mut result = String::new();

        // 1. 盤面
        for rank in 0..9 {
            let r = Rank::ALL[rank];
            let mut empty_count = 0;

            for file in (0..9).rev() {
                let f = File::ALL[file];
                let sq = Square::new(f, r);
                let pc = self.piece_on(sq);

                if pc.is_none() {
                    empty_count += 1;
                } else {
                    if empty_count > 0 {
                        result.push_str(&empty_count.to_string());
                        empty_count = 0;
                    }
                    result.push_str(&piece_to_sfen(pc));
                }
            }

            if empty_count > 0 {
                result.push_str(&empty_count.to_string());
            }

            if rank < 8 {
                result.push('/');
            }
        }

        // 2. 手番
        result.push(' ');
        result.push(if self.side_to_move == Color::Black {
            'b'
        } else {
            'w'
        });

        // 3. 手駒
        result.push(' ');
        let hand_str = self.hand_to_sfen();
        if hand_str.is_empty() {
            result.push('-');
        } else {
            result.push_str(&hand_str);
        }

        // 4. 手数
        result.push(' ');
        result.push_str(&self.game_ply.to_string());

        result
    }

    /// 盤面部分をパース
    fn parse_board(&mut self, board_str: &str) -> Result<(), SfenError> {
        let ranks: Vec<&str> = board_str.split('/').collect();
        if ranks.len() != 9 {
            return Err(SfenError::Board(format!("Expected 9 ranks, got {}", ranks.len())));
        }

        for (rank_idx, rank_str) in ranks.iter().enumerate() {
            let rank = Rank::ALL[rank_idx];
            let mut file_idx = 8i32; // 9筋から開始
            let mut promoted = false;

            for c in rank_str.chars() {
                if c == '+' {
                    promoted = true;
                    continue;
                }

                if let Some(digit) = c.to_digit(10) {
                    file_idx -= digit as i32;
                    if file_idx < -1 {
                        return Err(SfenError::Board(format!(
                            "Too many squares in rank {rank_idx}"
                        )));
                    }
                } else {
                    if file_idx < 0 {
                        return Err(SfenError::Board(format!(
                            "Too many pieces in rank {rank_idx}"
                        )));
                    }

                    let file = File::ALL[file_idx as usize];
                    let sq = Square::new(file, rank);

                    let pc = sfen_char_to_piece(c, promoted)?;
                    self.put_piece(pc, sq);

                    // 玉の位置を記録
                    if pc.piece_type() == PieceType::King {
                        self.king_square[pc.color().index()] = sq;
                    }

                    promoted = false;
                    file_idx -= 1;
                }
            }

            if file_idx != -1 {
                return Err(SfenError::Board(format!(
                    "Rank {rank_idx} has wrong number of squares"
                )));
            }
        }

        Ok(())
    }

    /// 手駒部分をパース
    fn parse_hand(&mut self, hand_str: &str) -> Result<(), SfenError> {
        if hand_str == "-" {
            return Ok(());
        }

        let mut count = 0u32;
        for c in hand_str.chars() {
            if let Some(digit) = c.to_digit(10) {
                count = count * 10 + digit;
            } else {
                let (color, pt) = sfen_hand_char_to_piece(c)?;
                let actual_count = if count == 0 { 1 } else { count };

                for _ in 0..actual_count {
                    self.hand[color.index()] = self.hand[color.index()].add(pt);
                }
                count = 0;
            }
        }

        Ok(())
    }

    /// 手駒をSFEN文字列に変換
    fn hand_to_sfen(&self) -> String {
        let mut result = String::new();

        // 先手の手駒（大文字）
        for (pt, c) in [
            (PieceType::Rook, 'R'),
            (PieceType::Bishop, 'B'),
            (PieceType::Gold, 'G'),
            (PieceType::Silver, 'S'),
            (PieceType::Knight, 'N'),
            (PieceType::Lance, 'L'),
            (PieceType::Pawn, 'P'),
        ] {
            let cnt = self.hand[Color::Black.index()].count(pt);
            if cnt > 0 {
                if cnt > 1 {
                    result.push_str(&cnt.to_string());
                }
                result.push(c);
            }
        }

        // 後手の手駒（小文字）
        for (pt, c) in [
            (PieceType::Rook, 'r'),
            (PieceType::Bishop, 'b'),
            (PieceType::Gold, 'g'),
            (PieceType::Silver, 's'),
            (PieceType::Knight, 'n'),
            (PieceType::Lance, 'l'),
            (PieceType::Pawn, 'p'),
        ] {
            let cnt = self.hand[Color::White.index()].count(pt);
            if cnt > 0 {
                if cnt > 1 {
                    result.push_str(&cnt.to_string());
                }
                result.push(c);
            }
        }

        result
    }

    /// ハッシュ値を計算
    fn compute_hash(&mut self) {
        let mut board_key = 0u64;
        let mut hand_key = 0u64;

        // 盤上の駒
        for sq_idx in 0..Square::NUM {
            let sq = unsafe { Square::from_u8_unchecked(sq_idx as u8) };
            let pc = self.piece_on(sq);
            if pc.is_some() {
                board_key ^= zobrist_psq(pc, sq);
            }
        }

        // 手番
        if self.side_to_move == Color::White {
            board_key ^= zobrist_side();
        }

        // 手駒
        for color in [Color::Black, Color::White] {
            for pt in [
                PieceType::Pawn,
                PieceType::Lance,
                PieceType::Knight,
                PieceType::Silver,
                PieceType::Gold,
                PieceType::Bishop,
                PieceType::Rook,
            ] {
                let cnt = self.hand[color.index()].count(pt);
                for _ in 0..cnt {
                    hand_key ^= zobrist_hand(color, pt);
                }
            }
        }

        self.state.board_key = board_key;
        self.state.hand_key = hand_key;
    }
}

/// 駒をSFEN文字列に変換
fn piece_to_sfen(pc: Piece) -> String {
    let base = match pc.piece_type() {
        PieceType::Pawn => "P",
        PieceType::Lance => "L",
        PieceType::Knight => "N",
        PieceType::Silver => "S",
        PieceType::Bishop => "B",
        PieceType::Rook => "R",
        PieceType::Gold => "G",
        PieceType::King => "K",
        PieceType::ProPawn => "+P",
        PieceType::ProLance => "+L",
        PieceType::ProKnight => "+N",
        PieceType::ProSilver => "+S",
        PieceType::Horse => "+B",
        PieceType::Dragon => "+R",
    };

    if pc.color() == Color::White {
        base.to_lowercase()
    } else {
        base.to_string()
    }
}

/// SFEN文字を駒に変換
fn sfen_char_to_piece(c: char, promoted: bool) -> Result<Piece, SfenError> {
    let is_black = c.is_uppercase();
    let color = if is_black { Color::Black } else { Color::White };

    let base_pt = match c.to_ascii_uppercase() {
        'P' => PieceType::Pawn,
        'L' => PieceType::Lance,
        'N' => PieceType::Knight,
        'S' => PieceType::Silver,
        'B' => PieceType::Bishop,
        'R' => PieceType::Rook,
        'G' => PieceType::Gold,
        'K' => PieceType::King,
        _ => return Err(SfenError::Board(format!("Unknown piece: {c}"))),
    };

    let pt = if promoted {
        base_pt
            .promote()
            .ok_or_else(|| SfenError::Board(format!("Cannot promote: {c}")))?
    } else {
        base_pt
    };

    Ok(Piece::new(color, pt))
}

/// SFEN手駒文字を駒種に変換
fn sfen_hand_char_to_piece(c: char) -> Result<(Color, PieceType), SfenError> {
    let is_black = c.is_uppercase();
    let color = if is_black { Color::Black } else { Color::White };

    let pt = match c.to_ascii_uppercase() {
        'P' => PieceType::Pawn,
        'L' => PieceType::Lance,
        'N' => PieceType::Knight,
        'S' => PieceType::Silver,
        'B' => PieceType::Bishop,
        'R' => PieceType::Rook,
        'G' => PieceType::Gold,
        _ => return Err(SfenError::Hand(format!("Unknown hand piece: {c}"))),
    };

    Ok((color, pt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_hirate() {
        let mut pos = Position::new();
        pos.set_hirate();

        assert_eq!(pos.side_to_move(), Color::Black);
        assert_eq!(pos.game_ply(), 1);

        // 先手の駒配置チェック
        assert_eq!(pos.piece_on(Square::new(File::File9, Rank::Rank9)), Piece::B_LANCE);
        assert_eq!(pos.piece_on(Square::new(File::File5, Rank::Rank9)), Piece::B_KING);
        assert_eq!(pos.piece_on(Square::new(File::File7, Rank::Rank7)), Piece::B_PAWN);
        assert_eq!(pos.piece_on(Square::new(File::File8, Rank::Rank8)), Piece::B_BISHOP);
        assert_eq!(pos.piece_on(Square::new(File::File2, Rank::Rank8)), Piece::B_ROOK);

        // 後手の駒配置チェック
        assert_eq!(pos.piece_on(Square::new(File::File9, Rank::Rank1)), Piece::W_LANCE);
        assert_eq!(pos.piece_on(Square::new(File::File5, Rank::Rank1)), Piece::W_KING);
        assert_eq!(pos.piece_on(Square::new(File::File7, Rank::Rank3)), Piece::W_PAWN);

        // 玉の位置
        assert_eq!(pos.king_square(Color::Black), Square::new(File::File5, Rank::Rank9));
        assert_eq!(pos.king_square(Color::White), Square::new(File::File5, Rank::Rank1));

        // 手駒なし
        assert!(pos.hand(Color::Black).is_empty());
        assert!(pos.hand(Color::White).is_empty());
    }

    #[test]
    fn test_sfen_roundtrip() {
        let test_cases = [
            SFEN_HIRATE,
            "8l/1l+R2P3/p2pBG1pp/kps1p4/Nn1P2G2/P1P1P2PP/1PS6/1KSG3+r1/LN2+p3L w Sbgn3p 124",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        ];

        for sfen in test_cases {
            let mut pos = Position::new();
            pos.set_sfen(sfen).unwrap();
            let result = pos.to_sfen();
            assert_eq!(result, sfen, "SFEN roundtrip failed for: {sfen}");
        }
    }

    #[test]
    fn test_sfen_with_hands() {
        let sfen = "4k4/9/9/9/9/9/9/9/4K4 b 2P 1";
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();

        assert_eq!(pos.hand(Color::Black).count(PieceType::Pawn), 2);
        assert_eq!(pos.hand(Color::White).count(PieceType::Pawn), 0);
    }

    #[test]
    fn test_sfen_promoted_pieces() {
        let sfen = "4k4/9/9/9/4+P4/9/9/9/4K4 b - 1";
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();

        let sq = Square::new(File::File5, Rank::Rank5);
        assert_eq!(pos.piece_on(sq), Piece::B_PRO_PAWN);
    }

    #[test]
    fn test_sfen_white_to_move() {
        let sfen = "4k4/9/9/9/9/9/9/9/4K4 w - 1";
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();

        assert_eq!(pos.side_to_move(), Color::White);
    }

    #[test]
    fn test_sfen_error_invalid_board() {
        let mut pos = Position::new();
        let result = pos.set_sfen("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_piece_to_sfen() {
        assert_eq!(piece_to_sfen(Piece::B_PAWN), "P");
        assert_eq!(piece_to_sfen(Piece::W_PAWN), "p");
        assert_eq!(piece_to_sfen(Piece::B_PRO_PAWN), "+P");
        assert_eq!(piece_to_sfen(Piece::W_HORSE), "+b");
    }
}
