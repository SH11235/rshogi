//! SFEN形式の解析・出力

use crate::eval::material::compute_material_value;
use crate::nnue::piece_list::piece_number_base;
use crate::nnue::{ExtBonaPiece, PieceNumber};
use crate::types::{Color, File, Hand, Piece, PieceType, Rank, Square};

use super::pos::{Position, is_minor_piece};
use super::zobrist::{zobrist_hand, zobrist_no_pawns, zobrist_psq, zobrist_side};

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
                )));
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

        self.finalize_after_population()
    }

    /// SFEN 文字列を経ずに、盤面・手駒・手番から局面を構築する。
    ///
    /// `set_sfen` の文字列パース（`generate_sfen` 由来の `String` 生成や `split` の `Vec` 確保）
    /// を避けたいホットパス（PSV → 局面の大量変換など）向け。`board` はマス index（0..81、
    /// `Square` の index と同順）で空マスは `Piece::NONE`、`hand` は `[先手, 後手]`。手数は 1
    /// に設定する（PSV 変換用途では手数は評価に影響しない）。`set_sfen` と同じ後処理
    /// （PieceList / ハッシュ / 利き / pin / 王手 / material の再計算と在庫検証）を通すため、
    /// 得られる局面は同一盤面を `set_sfen` で構築したものと一致する。
    pub fn set_from_parts(
        &mut self,
        board: &[Piece; Square::NUM],
        hand: &[Hand; Color::NUM],
        side_to_move: Color,
    ) -> Result<(), SfenError> {
        *self = Position::new();

        for (sq_idx, &pc) in board.iter().enumerate() {
            if pc.is_none() {
                continue;
            }
            // sq_idx は board.iter() の添字で 0..Square::NUM(81) の範囲内。範囲外になるのは
            // 入力エラーではなく内部不変条件の破壊（ロジックバグ）なので panic させる。
            let sq = Square::from_u8(sq_idx as u8)
                .expect("sq_idx は 0..Square::NUM の範囲内であり from_u8 は必ず Some");
            self.put_piece(pc, sq);
            if pc.piece_type() == PieceType::King {
                // put_piece は king_square を更新しないため、parse_board と同様に明示的にセットする。
                self.king_square[pc.color().index()] = sq;
            }
        }

        self.hand = *hand;
        self.side_to_move = side_to_move;
        self.game_ply = 1;

        self.finalize_after_population()
    }

    /// 盤面・手駒・手番を投入済みの状態から、PieceList・ハッシュ・利き・pin・王手・material を
    /// 再計算し、駒在庫を検証して局面を確定する（`set_sfen` / `set_from_parts` 共通の後処理）。
    fn finalize_after_population(&mut self) -> Result<(), SfenError> {
        self.validate_piece_inventory()?;

        // PieceList の初期化
        self.init_piece_list();

        // ハッシュ値の計算
        self.compute_hash();

        // pin情報と王手マスの更新
        self.update_blockers_and_pinners();
        self.update_check_squares();

        // 盤面の利き数を再計算
        self.recompute_board_effects();

        // 王手駒の計算
        let them = !self.side_to_move;
        self.state_mut().checkers =
            self.attackers_to_c(self.king_square[self.side_to_move.index()], them);

        // material_value を再計算
        self.state_mut().material_value = compute_material_value(self);

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
                count =
                    count.checked_mul(10).and_then(|v| v.checked_add(digit)).ok_or_else(|| {
                        SfenError::Hand("Hand count overflow while parsing digits".to_string())
                    })?;
            } else {
                let (color, pt) = sfen_hand_char_to_piece(c)?;
                let actual_count = if count == 0 { 1 } else { count };

                let current = self.hand[color.index()].count(pt);
                let max = hand_max(pt);
                if actual_count > max || current + actual_count > max {
                    return Err(SfenError::Hand(format!(
                        "Too many {:?} in hand: {} (current {}, max {})",
                        pt, actual_count, current, max
                    )));
                }

                for _ in 0..actual_count {
                    self.hand[color.index()] = self.hand[color.index()].add(pt);
                }
                count = 0;
            }
        }

        if count != 0 {
            return Err(SfenError::Hand("Hand string ends with digit but no piece".to_string()));
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

    /// 盤上と手駒を合わせた総駒数が初期枚数を超えていないことを検証する。
    fn validate_piece_inventory(&self) -> Result<(), SfenError> {
        let mut counts = [0u8; 8];

        for sq_idx in 0..Square::NUM {
            // SAFETY: sq_idx は 0..81 の範囲内
            let sq = unsafe { Square::from_u8_unchecked(sq_idx as u8) };
            let pc = self.piece_on(sq);
            if pc.is_none() {
                continue;
            }

            let raw_pt = pc.piece_type().unpromote() as u8;
            let idx = pt_to_counter_index(raw_pt);
            counts[idx] = counts[idx].saturating_add(1);
        }

        for color in [Color::Black, Color::White] {
            for pt in PieceType::HAND_PIECES {
                let idx = pt_to_counter_index(pt as u8);
                counts[idx] = counts[idx].saturating_add(self.hand[color.index()].count(pt) as u8);
            }
        }

        for (raw_pt, label) in [
            (PieceType::Pawn as u8, "Pawn"),
            (PieceType::Lance as u8, "Lance"),
            (PieceType::Knight as u8, "Knight"),
            (PieceType::Silver as u8, "Silver"),
            (PieceType::Gold as u8, "Gold"),
            (PieceType::Bishop as u8, "Bishop"),
            (PieceType::Rook as u8, "Rook"),
            (PieceType::King as u8, "King"),
        ] {
            let idx = pt_to_counter_index(raw_pt);
            let total = counts[idx];
            let max = piece_inventory_max(raw_pt);
            if total > max {
                return Err(SfenError::Board(format!(
                    "Too many {label} in position: total {total}, max {max}"
                )));
            }
        }

        Ok(())
    }

    /// PieceList を盤面と手駒から初期化
    ///
    /// 駒種ごとに PieceNumber カウンタを管理し、盤面走査 → 手駒走査の順で構築する。
    pub(crate) fn init_piece_list(&mut self) {
        use crate::nnue::piece_list::PieceList;

        self.piece_list = PieceList::new();

        // 駒種ごとの PieceNumber カウンタ（[base_index] → 割り当て済み枚数）
        // 8エントリ: 歩,香,桂,銀,金,角,飛,玉
        let mut counters = [0u8; 8];

        // 盤上の駒を走査
        for sq_idx in 0..Square::NUM {
            // SAFETY: sq_idx は 0..81 の範囲内
            let sq = unsafe { Square::from_u8_unchecked(sq_idx as u8) };
            let pc = self.piece_on(sq);
            if pc.is_none() {
                continue;
            }

            let pt = pc.piece_type();
            let base = piece_number_base(pt);
            let raw_pt = pt.unpromote() as u8;
            let base_idx = super::sfen::pt_to_counter_index(raw_pt);
            assert!(
                counters[base_idx] < piece_inventory_max(raw_pt),
                "piece inventory overflow while initializing PieceList: pt={pt:?}, count={}, max={}",
                counters[base_idx] + 1,
                piece_inventory_max(raw_pt)
            );
            let piece_no = PieceNumber(base + counters[base_idx]);
            counters[base_idx] += 1;

            let bp = ExtBonaPiece::from_board(pc, sq);
            self.piece_list.put_piece_on_board(piece_no, bp, sq);
        }

        // 手駒を走査
        for color in [Color::Black, Color::White] {
            for pt in PieceType::HAND_PIECES {
                let count = self.hand[color.index()].count(pt) as u8;
                for i in 1..=count {
                    let base = piece_number_base(pt);
                    let raw_pt = pt as u8;
                    let base_idx = pt_to_counter_index(raw_pt);
                    assert!(
                        counters[base_idx] < piece_inventory_max(raw_pt),
                        "piece inventory overflow while initializing PieceList: pt={pt:?}, count={}, max={}",
                        counters[base_idx] + 1,
                        piece_inventory_max(raw_pt)
                    );
                    let piece_no = PieceNumber(base + counters[base_idx]);
                    counters[base_idx] += 1;

                    let bp = ExtBonaPiece::from_hand(color, pt, i);
                    self.piece_list.put_piece_on_hand(piece_no, bp);
                }
            }
        }
    }

    /// ハッシュ値を計算
    pub(crate) fn compute_hash(&mut self) {
        let mut board_key = 0u64;
        let mut hand_key = 0u64;
        let mut pawn_key = zobrist_no_pawns();
        let mut minor_piece_key = 0u64;
        let mut non_pawn_key = [0u64; Color::NUM];

        // 盤上の駒
        for sq_idx in 0..Square::NUM {
            let sq = unsafe { Square::from_u8_unchecked(sq_idx as u8) };
            let pc = self.piece_on(sq);
            if pc.is_some() {
                let z = zobrist_psq(pc, sq);
                board_key ^= z;

                if pc.piece_type() == PieceType::Pawn {
                    pawn_key ^= z;
                } else {
                    if is_minor_piece(pc) {
                        minor_piece_key ^= z;
                    }
                    non_pawn_key[pc.color().index()] ^= z;
                }
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
                let cnt = self.hand[color.index()].count(pt) as u64;
                if cnt > 0 {
                    let z = zobrist_hand(color, pt);
                    hand_key = hand_key.wrapping_add(z.wrapping_mul(cnt));
                }
            }
        }

        let st = self.state_mut();
        st.board_key = board_key;
        st.hand_key = hand_key;
        st.pawn_key = pawn_key;
        st.minor_piece_key = minor_piece_key;
        st.non_pawn_key = non_pawn_key;
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

/// PieceType(生駒) → counter配列のインデックスへの変換
///
/// Pawn(1)→0, Lance(2)→1, Knight(3)→2, Silver(4)→3,
/// Bishop(5)→5, Rook(6)→6, Gold(7)→4, King(8)→7
fn pt_to_counter_index(raw_pt: u8) -> usize {
    const TABLE: [u8; 9] = [
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
    TABLE[raw_pt as usize] as usize
}

/// 手駒で持てる枚数の上限を返す
fn hand_max(pt: PieceType) -> u32 {
    match pt {
        PieceType::Pawn => 18,
        PieceType::Lance | PieceType::Knight | PieceType::Silver | PieceType::Gold => 4,
        PieceType::Bishop | PieceType::Rook => 2,
        _ => 0,
    }
}

/// 局面全体で存在しうる駒枚数の上限を返す。
fn piece_inventory_max(raw_pt: u8) -> u8 {
    match raw_pt {
        x if x == PieceType::Pawn as u8 => 18,
        x if x == PieceType::Lance as u8 => 4,
        x if x == PieceType::Knight as u8 => 4,
        x if x == PieceType::Silver as u8 => 4,
        x if x == PieceType::Gold as u8 => 4,
        x if x == PieceType::Bishop as u8 => 2,
        x if x == PieceType::Rook as u8 => 2,
        x if x == PieceType::King as u8 => 2,
        _ => 0,
    }
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
    fn test_sfen_roundtrip_max_hands() {
        let sfen = "4k4/9/9/9/9/9/9/9/4K4 b 2R2B4G4S4N4L18P 37";
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        assert_eq!(pos.to_sfen(), sfen);
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
    fn test_sfen_lowercase_bishop_is_white_hand() {
        let sfen = "4k4/9/9/9/9/9/9/9/4K4 b b 1";
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();

        assert_eq!(pos.hand(Color::Black).count(PieceType::Bishop), 0);
        assert_eq!(pos.hand(Color::White).count(PieceType::Bishop), 1);
    }

    #[test]
    fn test_sfen_rejects_piece_inventory_overflow() {
        let sfen = "lnsgkg1nl/5s1b1/1p1pppp1p/p1p6/7p1/P5P2/1PPPPP+bPP/1R3S3/LNSGKG1NL b b 13";
        let mut pos = Position::new();
        let err = pos.set_sfen(sfen).expect_err("角系が3枚ある局面は不正");

        assert!(
            err.to_string().contains("Too many Bishop in position"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_sfen_hand_invalid_too_many_pawns() {
        let sfen = "4k4/9/9/9/9/9/9/9/4K4 b 19P 1";
        let mut pos = Position::new();
        let result = pos.set_sfen(sfen);
        assert!(result.is_err());
    }

    #[test]
    fn test_sfen_hand_invalid_trailing_digit() {
        let sfen = "4k4/9/9/9/9/9/9/9/4K4 b 3 1";
        let mut pos = Position::new();
        let result = pos.set_sfen(sfen);
        assert!(result.is_err());
    }

    #[test]
    fn test_sfen_hand_invalid_duplicate_overflow() {
        // 2Rまでが上限のところにさらに1Rを加える
        let sfen = "4k4/9/9/9/9/9/9/9/4K4 b 2R1R 1";
        let mut pos = Position::new();
        let result = pos.set_sfen(sfen);
        assert!(result.is_err());
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

    #[test]
    fn test_set_from_parts_matches_set_sfen() {
        // set_from_parts（String を経由しない構築）が、同一盤面を set_sfen で構築した
        // 局面と一致することを保証する。手駒・成り駒・駒落ち・後手番を網羅。
        // 手数は set_from_parts が 1 固定のため、SFEN 側も手数 1 で揃える。
        let sfens = [
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPP1P/1B5R1/LNSGKGSNL b P 1",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1+B5R1/LNSGKGSNL w - 1",
            "lnsgkgsn1/1r5b1/ppppppppp/9/9/9/1PPPPPPPP/1B5R1/LNSGKGSN1 b - 1",
            "9/9/9/9/4k4/9/9/9/4K4 w 2G2g 1",
            // 手番側（先手）が王手を受けている局面。先手玉 5e を後手飛車 5a が同一筋で王手。
            // finalize_after_population の王手計算が set_from_parts でも働くことを確認する。
            "4r4/9/9/9/4K4/9/9/9/4k4 b - 1",
        ];
        let mut any_in_check = false;
        for sfen in sfens {
            let mut from_sfen = Position::new();
            from_sfen.set_sfen(sfen).expect("set_sfen should succeed");

            // 構築済み局面から素の要素を取り出す
            let mut board = [Piece::NONE; Square::NUM];
            for (i, cell) in board.iter_mut().enumerate() {
                let sq = Square::from_u8(i as u8).expect("valid square index");
                *cell = from_sfen.piece_on(sq);
            }
            let hand = [from_sfen.hand(Color::Black), from_sfen.hand(Color::White)];

            let mut from_parts = Position::new();
            from_parts
                .set_from_parts(&board, &hand, from_sfen.side_to_move())
                .expect("set_from_parts should succeed");

            assert_eq!(from_parts.to_sfen(), from_sfen.to_sfen(), "sfen mismatch for {sfen}");
            assert_eq!(from_parts.key(), from_sfen.key(), "key mismatch for {sfen}");
            assert_eq!(from_parts.in_check(), from_sfen.in_check(), "in_check mismatch for {sfen}");

            any_in_check |= from_sfen.in_check();
        }
        // 王手計算の parity を実際に検証するため、in_check() == true の局面を最低 1 件通す。
        assert!(any_in_check, "王手局面を最低 1 件含むこと");
    }
}
