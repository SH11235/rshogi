//! PackedSfen/PackedSfenValue復号モジュール
//!
//! YaneuraOuのpack形式（PackedSfenValue）を読み込み、SFEN文字列に変換する。
//!
//! ## PackedSfenValue (40バイト/レコード)
//!
//! | フィールド  | サイズ | 説明                                    |
//! |-------------|--------|-----------------------------------------|
//! | sfen        | 32     | PackedSfen (256bit)                     |
//! | score       | 2      | 評価値 (i16)                            |
//! | move        | 2      | 最善手 Move16形式 (u16)                 |
//! | game_ply    | 2      | 手数 (u16)                              |
//! | game_result | 1      | 勝敗 (i8: 1=勝ち, 0=引分, -1=負け)     |
//! | padding     | 1      | パディング                              |
//!
//! ## PackedSfen形式 (32バイト = 256bit)
//!
//! ビットストリームで以下の順序で格納:
//! 1. 手番 (1bit): 0=先手, 1=後手
//! 2. 先手玉位置 (7bit): 0-80のマス番号
//! 3. 後手玉位置 (7bit): 0-80のマス番号
//! 4. 盤上の駒 (ハフマン符号化): 81マス分（玉のマスはスキップ）
//! 5. 手駒 (ハフマン符号化): 残りビットで表現

use engine_core::types::{Color, Hand, Move, Piece, PieceType, Square};

/// PackedSfenValue (40バイト)
#[derive(Debug, Clone, Copy)]
pub struct PackedSfenValue {
    /// PackedSfen (32バイト)
    pub sfen: [u8; 32],
    /// 評価値
    pub score: i16,
    /// 最善手 (Move16形式)
    pub move16: u16,
    /// 手数
    pub game_ply: u16,
    /// 勝敗 (1=勝ち, 0=引分, -1=負け)
    pub game_result: i8,
    /// パディング
    pub padding: u8,
}

impl PackedSfenValue {
    /// サイズ (バイト)
    pub const SIZE: usize = 40;

    /// バイト列からPackedSfenValueを読み込む
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }

        let mut sfen = [0u8; 32];
        sfen.copy_from_slice(&bytes[0..32]);

        let score = i16::from_le_bytes([bytes[32], bytes[33]]);
        let move16 = u16::from_le_bytes([bytes[34], bytes[35]]);
        let game_ply = u16::from_le_bytes([bytes[36], bytes[37]]);
        let game_result = bytes[38] as i8;
        let padding = bytes[39];

        Some(Self {
            sfen,
            score,
            move16,
            game_ply,
            game_result,
            padding,
        })
    }
}

/// ビットストリーム読み込み用構造体
struct BitStream<'a> {
    data: &'a [u8],
    bit_cursor: usize,
}

impl<'a> BitStream<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_cursor: 0,
        }
    }

    /// 1ビット読み込む (オーバーフロー時は0を返す)
    fn read_one_bit(&mut self) -> u8 {
        let byte_idx = self.bit_cursor / 8;
        if byte_idx >= self.data.len() {
            return 0; // オーバーフロー時は0を返す
        }
        let bit_idx = self.bit_cursor & 7;
        self.bit_cursor += 1;
        (self.data[byte_idx] >> bit_idx) & 1
    }

    /// 残りビット数
    fn remaining(&self) -> usize {
        let total_bits = self.data.len() * 8;
        total_bits.saturating_sub(self.bit_cursor)
    }

    /// nビット読み込む (下位ビットから順に格納)
    fn read_n_bit(&mut self, n: usize) -> u32 {
        let mut result = 0u32;
        for i in 0..n {
            result |= (self.read_one_bit() as u32) << i;
        }
        result
    }

    /// 現在のカーソル位置
    fn cursor(&self) -> usize {
        self.bit_cursor
    }
}

/// ハフマン符号化テーブル（盤上の駒）
///
/// | 駒種 | コード   | ビット数 |
/// |------|----------|----------|
/// | 空   | 0        | 1        |
/// | 歩   | 01       | 2        |
/// | 香   | 0011     | 4        |
/// | 桂   | 1011     | 4        |
/// | 銀   | 0111     | 4        |
/// | 角   | 011111   | 6        |
/// | 飛   | 111111   | 6        |
/// | 金   | 01111    | 5        |
#[derive(Debug, Clone, Copy)]
struct HuffmanCode {
    code: u8,
    bits: u8,
}

const HUFFMAN_TABLE: [HuffmanCode; 8] = [
    HuffmanCode {
        code: 0x00,
        bits: 1,
    }, // NO_PIECE (空)
    HuffmanCode {
        code: 0x01,
        bits: 2,
    }, // PAWN (歩)
    HuffmanCode {
        code: 0x03,
        bits: 4,
    }, // LANCE (香)
    HuffmanCode {
        code: 0x0b,
        bits: 4,
    }, // KNIGHT (桂)
    HuffmanCode {
        code: 0x07,
        bits: 4,
    }, // SILVER (銀)
    HuffmanCode {
        code: 0x1f,
        bits: 6,
    }, // BISHOP (角)
    HuffmanCode {
        code: 0x3f,
        bits: 6,
    }, // ROOK (飛)
    HuffmanCode {
        code: 0x0f,
        bits: 5,
    }, // GOLD (金)
];

/// ハフマン符号から駒種を復号する
/// 戻り値: (駒種, 玉=true) または None=空きマス
fn decode_huffman_piece(stream: &mut BitStream) -> Option<usize> {
    let mut code = 0u8;
    let mut bits = 0u8;

    loop {
        code |= stream.read_one_bit() << bits;
        bits += 1;

        if bits > 6 {
            return None; // エラー
        }

        // ハフマンテーブルと照合
        for (i, h) in HUFFMAN_TABLE.iter().enumerate() {
            if h.code == code && h.bits == bits {
                return if i == 0 { None } else { Some(i) };
            }
        }
    }
}

/// 手駒用ハフマン符号から駒種を復号する
/// 盤上の駒の符号からbit0を削除した形式
/// 戻り値: (駒種インデックス, 成りフラグ=駒箱の駒)
fn decode_huffman_hand_piece(stream: &mut BitStream) -> (usize, bool) {
    let mut code = 0u8;
    let mut bits = 0u8;

    loop {
        code |= stream.read_one_bit() << bits;
        bits += 1;

        if bits > 5 {
            panic!("Invalid hand piece huffman code");
        }

        // 手駒用テーブルは盤上テーブルのコードを>>1したもの
        for (i, h) in HUFFMAN_TABLE.iter().enumerate().skip(1) {
            if (h.code >> 1) == code && (h.bits - 1) == bits {
                // 金以外は成りフラグを読む (成り=1なら駒箱の駒)
                let is_piecebox = if i != 7 {
                    // 金以外
                    stream.read_one_bit() != 0
                } else {
                    false
                };
                return (i, is_piecebox);
            }
        }
    }
}

/// 駒種インデックスからPieceTypeへの変換
/// インデックス: 1=歩, 2=香, 3=桂, 4=銀, 5=角, 6=飛, 7=金
fn piece_type_from_index(index: usize) -> Option<PieceType> {
    match index {
        1 => Some(PieceType::Pawn),
        2 => Some(PieceType::Lance),
        3 => Some(PieceType::Knight),
        4 => Some(PieceType::Silver),
        5 => Some(PieceType::Bishop),
        6 => Some(PieceType::Rook),
        7 => Some(PieceType::Gold),
        _ => None,
    }
}

/// PackedSfenをSFEN文字列に変換
pub fn unpack_sfen(packed: &[u8; 32]) -> Result<String, String> {
    let mut stream = BitStream::new(packed);

    // 手番 (1bit)
    let side_to_move = if stream.read_one_bit() == 0 {
        Color::Black
    } else {
        Color::White
    };

    // 盤面 (81マス)
    let mut board = [Piece::NONE; 81];

    // 先手玉位置 (7bit)
    let black_king_sq = stream.read_n_bit(7) as u8;
    if black_king_sq < 81 {
        board[black_king_sq as usize] = Piece::B_KING;
    }

    // 後手玉位置 (7bit)
    let white_king_sq = stream.read_n_bit(7) as u8;
    if white_king_sq < 81 {
        board[white_king_sq as usize] = Piece::W_KING;
    }

    // 盤上の駒 (ハフマン符号化)
    for (sq, cell) in board.iter_mut().enumerate() {
        // 玉がすでにいるマスはスキップ
        // Note: cell.is_some() を先にチェックしないと piece_type() がパニックする
        if cell.is_some() && cell.piece_type() == PieceType::King {
            continue;
        }

        let piece_idx = decode_huffman_piece(&mut stream);

        if let Some(idx) = piece_idx {
            let pt = piece_type_from_index(idx).ok_or("Invalid piece type")?;

            // 金以外は成りフラグを読む
            let promoted = if pt != PieceType::Gold {
                stream.read_one_bit() != 0
            } else {
                false
            };

            // 先後フラグを読む
            let color = if stream.read_one_bit() == 0 {
                Color::Black
            } else {
                Color::White
            };

            let piece = if promoted {
                Piece::new(color, pt.promote().ok_or("Cannot promote")?)
            } else {
                Piece::new(color, pt)
            };
            *cell = piece;
        }

        if stream.cursor() > 256 {
            return Err(format!("BitStream overflow at sq {sq}"));
        }
    }

    // 手駒 (残りのビット)
    let mut hands = [Hand::EMPTY; 2];

    while stream.remaining() > 0 {
        let (piece_idx, is_piecebox) = decode_huffman_hand_piece(&mut stream);

        // 駒箱の駒は無視
        if is_piecebox {
            // 金以外は先後フラグも読む
            if piece_idx != 7 && stream.remaining() > 0 {
                let _ = stream.read_one_bit();
            }
            continue;
        }

        // 先後フラグを読む
        if stream.remaining() == 0 {
            break;
        }
        let color = if stream.read_one_bit() == 0 {
            Color::Black
        } else {
            Color::White
        };

        let pt = piece_type_from_index(piece_idx).ok_or("Invalid hand piece type")?;
        hands[color.index()] = hands[color.index()].add(pt);
    }

    // SFEN文字列を生成
    Ok(generate_sfen(&board, &hands, side_to_move))
}

/// 盤面と手駒からSFEN文字列を生成
fn generate_sfen(board: &[Piece; 81], hands: &[Hand; 2], side_to_move: Color) -> String {
    let mut sfen = String::new();

    // 盤面部分
    for rank in 0..9 {
        if rank > 0 {
            sfen.push('/');
        }
        let mut empty_count = 0;
        for file in (0..9).rev() {
            let sq = file * 9 + rank;
            let piece = board[sq];
            if piece.is_none() {
                empty_count += 1;
            } else {
                if empty_count > 0 {
                    sfen.push_str(&empty_count.to_string());
                    empty_count = 0;
                }
                sfen.push_str(&piece_to_sfen_char(piece));
            }
        }
        if empty_count > 0 {
            sfen.push_str(&empty_count.to_string());
        }
    }

    // 手番
    sfen.push(' ');
    sfen.push(if side_to_move == Color::Black {
        'b'
    } else {
        'w'
    });
    sfen.push(' ');

    // 手駒
    let hand_str = generate_hand_sfen(&hands[0], &hands[1]);
    if hand_str.is_empty() {
        sfen.push('-');
    } else {
        sfen.push_str(&hand_str);
    }

    // 手数は省略（1固定）
    sfen.push_str(" 1");

    sfen
}

/// 駒をSFEN文字に変換
fn piece_to_sfen_char(piece: Piece) -> String {
    let pt = piece.piece_type();
    let promoted = pt.is_promoted();
    let raw_pt = pt.unpromote();

    let c = match raw_pt {
        PieceType::Pawn => 'P',
        PieceType::Lance => 'L',
        PieceType::Knight => 'N',
        PieceType::Silver => 'S',
        PieceType::Bishop => 'B',
        PieceType::Rook => 'R',
        PieceType::Gold => 'G',
        PieceType::King => 'K',
        _ => '?',
    };

    let c = if piece.color() == Color::White {
        c.to_ascii_lowercase()
    } else {
        c
    };

    if promoted {
        format!("+{c}")
    } else {
        c.to_string()
    }
}

/// 手駒をSFEN形式で生成
fn generate_hand_sfen(black_hand: &Hand, white_hand: &Hand) -> String {
    let mut result = String::new();

    // 先手の手駒 (飛角金銀桂香歩の順)
    let piece_order = [
        PieceType::Rook,
        PieceType::Bishop,
        PieceType::Gold,
        PieceType::Silver,
        PieceType::Knight,
        PieceType::Lance,
        PieceType::Pawn,
    ];

    for &pt in &piece_order {
        let count = black_hand.count(pt);
        if count > 0 {
            let c = match pt {
                PieceType::Pawn => 'P',
                PieceType::Lance => 'L',
                PieceType::Knight => 'N',
                PieceType::Silver => 'S',
                PieceType::Gold => 'G',
                PieceType::Bishop => 'B',
                PieceType::Rook => 'R',
                _ => continue,
            };
            if count > 1 {
                result.push_str(&count.to_string());
            }
            result.push(c);
        }
    }

    // 後手の手駒
    for &pt in &piece_order {
        let count = white_hand.count(pt);
        if count > 0 {
            let c = match pt {
                PieceType::Pawn => 'p',
                PieceType::Lance => 'l',
                PieceType::Knight => 'n',
                PieceType::Silver => 's',
                PieceType::Gold => 'g',
                PieceType::Bishop => 'b',
                PieceType::Rook => 'r',
                _ => continue,
            };
            if count > 1 {
                result.push_str(&count.to_string());
            }
            result.push(c);
        }
    }

    result
}

/// Move16形式をUSI形式の指し手文字列に変換
///
/// ## Move16形式
/// - bits 0-6:  移動先マス (to)
/// - bits 7-13: 移動元マス (from) または打つ駒種 (駒打ちの場合)
/// - bit 14:    成りフラグ
/// - bit 15:    未使用 (YaneuraOuでは0)
///
/// 打ち駒の判定: from >= 81 の場合は打ち駒
pub fn move16_to_usi(move16: u16) -> String {
    if move16 == 0 {
        return "none".to_string();
    }

    let to = (move16 & 0x7F) as u8;
    let from_or_pt = ((move16 >> 7) & 0x7F) as u8;
    let promote = (move16 & 0x4000) != 0;

    if from_or_pt >= 81 {
        // 打ち駒
        let pt_index = from_or_pt - 81;
        let pt_char = match pt_index {
            0 => return "none".to_string(), // 無効
            1 => 'P',
            2 => 'L',
            3 => 'N',
            4 => 'S',
            5 => 'B',
            6 => 'R',
            7 => 'G',
            _ => return "none".to_string(),
        };

        if let Some(to_sq) = Square::from_u8(to) {
            format!("{pt_char}*{}", to_sq.to_usi())
        } else {
            "none".to_string()
        }
    } else {
        // 通常の移動
        if let (Some(from_sq), Some(to_sq)) = (Square::from_u8(from_or_pt), Square::from_u8(to)) {
            let promote_str = if promote { "+" } else { "" };
            format!("{}{}{promote_str}", from_sq.to_usi(), to_sq.to_usi())
        } else {
            "none".to_string()
        }
    }
}

/// Move16形式をMove型に変換
pub fn move16_to_move(move16: u16) -> Move {
    if move16 == 0 {
        return Move::NONE;
    }

    let to = (move16 & 0x7F) as u8;
    let from_or_pt = ((move16 >> 7) & 0x7F) as u8;
    let promote = (move16 & 0x4000) != 0;

    if from_or_pt >= 81 {
        // 打ち駒
        let pt_index = from_or_pt - 81;
        let pt = match pt_index {
            1 => PieceType::Pawn,
            2 => PieceType::Lance,
            3 => PieceType::Knight,
            4 => PieceType::Silver,
            5 => PieceType::Bishop,
            6 => PieceType::Rook,
            7 => PieceType::Gold,
            _ => return Move::NONE,
        };

        if let Some(to_sq) = Square::from_u8(to) {
            Move::new_drop(pt, to_sq)
        } else {
            Move::NONE
        }
    } else {
        // 通常の移動
        if let (Some(from_sq), Some(to_sq)) = (Square::from_u8(from_or_pt), Square::from_u8(to)) {
            Move::new_move(from_sq, to_sq, promote)
        } else {
            Move::NONE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitstream() {
        let data = [0b10101010u8, 0b01010101u8];
        let mut stream = BitStream::new(&data);

        assert_eq!(stream.read_one_bit(), 0);
        assert_eq!(stream.read_one_bit(), 1);
        assert_eq!(stream.read_one_bit(), 0);
        assert_eq!(stream.read_one_bit(), 1);
    }

    #[test]
    fn test_move16_to_usi() {
        // 通常の移動: 7g(60) -> 7f(59)
        // File7=6, Rank7=6 → sq=6*9+6=60
        // File7=6, Rank6=5 → sq=6*9+5=59
        let move16 = 59 | (60 << 7);
        assert_eq!(move16_to_usi(move16), "7g7f");

        // 成り: 2c(11) -> 2b(10)
        // File2=1, Rank3=2 → sq=1*9+2=11
        // File2=1, Rank2=1 → sq=1*9+1=10
        let move16 = 10 | (11 << 7) | 0x4000;
        assert_eq!(move16_to_usi(move16), "2c2b+");

        // 駒打ち: P*5e (歩を5五に打つ)
        // File5=4, Rank5=4 → sq=4*9+4=40
        // 打ち駒: from = 81 + piece_type (歩=1)
        let move16 = 40 | (82 << 7);
        assert_eq!(move16_to_usi(move16), "P*5e");
    }

    #[test]
    fn test_packed_sfen_value_from_bytes() {
        let mut bytes = [0u8; 40];
        // score = 100
        bytes[32] = 100;
        bytes[33] = 0;
        // move16 = 0x1234
        bytes[34] = 0x34;
        bytes[35] = 0x12;
        // game_ply = 50
        bytes[36] = 50;
        bytes[37] = 0;
        // game_result = 1
        bytes[38] = 1;

        let psv = PackedSfenValue::from_bytes(&bytes).unwrap();
        assert_eq!(psv.score, 100);
        assert_eq!(psv.move16, 0x1234);
        assert_eq!(psv.game_ply, 50);
        assert_eq!(psv.game_result, 1);
    }
}
