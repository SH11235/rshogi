//! PackedSfen/PackedSfenValue復号モジュール
//!
//! YaneuraOuのpack形式（PackedSfenValue）を読み込み、SFEN文字列に変換する。
//!
//! # 概要
//!
//! このモジュールは、将棋エンジンYaneuraOuが生成する圧縮された局面データ形式（PackedSfen）を
//! 標準的なSFEN（Shogi Forsyth-Edwards Notation）形式に変換する機能を提供します。
//!
//! # 使用例
//!
//! ```rust,ignore
//! use tools::packed_sfen::{PackedSfenValue, unpack_sfen, move16_to_usi};
//!
//! // バイト列からPackedSfenValueを読み込み
//! let bytes = [0u8; 40]; // 実際のデータ
//! let psv = PackedSfenValue::from_bytes(&bytes).unwrap();
//!
//! // SFEN文字列に変換
//! let sfen = unpack_sfen(&psv.sfen).unwrap();
//! println!("SFEN: {}", sfen);
//!
//! // 指し手をUSI形式に変換
//! let usi_move = move16_to_usi(psv.move16);
//! println!("Move: {}", usi_move);
//! ```
//!
//! # データ形式
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

use rshogi_core::position::Position;
use rshogi_core::types::{Color, Hand, Move, Piece, PieceType, Square};

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

    /// PackedSfenValueをバイト列にシリアライズ
    pub fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..32].copy_from_slice(&self.sfen);
        bytes[32..34].copy_from_slice(&self.score.to_le_bytes());
        bytes[34..36].copy_from_slice(&self.move16.to_le_bytes());
        bytes[36..38].copy_from_slice(&self.game_ply.to_le_bytes());
        bytes[38] = self.game_result as u8;
        bytes[39] = self.padding;
        bytes
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

    /// 1ビット読み込む
    ///
    /// # 戻り値
    /// 読み込んだビット値 (0 または 1)
    ///
    /// # オーバーフロー時の動作
    /// ビットストリームの終端を超えて読み込もうとした場合は 0 を返す。
    /// これは意図的な設計で、ハフマン復号時に終端を超えた読み込みが
    /// 発生しても安全に処理を継続できるようにしている。
    /// 呼び出し側は `remaining()` で残りビット数を確認することで、
    /// 終端を検出できる。
    fn read_one_bit(&mut self) -> u8 {
        let byte_idx = self.bit_cursor / 8;
        if byte_idx >= self.data.len() {
            return 0; // オーバーフロー時は0を返す（意図的な動作）
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
/// 戻り値: Some(駒種インデックス) または None=空きマス
/// 駒種インデックス: 1=歩, 2=香, 3=桂, 4=銀, 5=角, 6=飛, 7=金
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
/// 戻り値: Some((駒種インデックス, 成りフラグ=駒箱の駒)) または None (不正なデータ)
fn decode_huffman_hand_piece(stream: &mut BitStream) -> Option<(usize, bool)> {
    let mut code = 0u8;
    let mut bits = 0u8;

    loop {
        code |= stream.read_one_bit() << bits;
        bits += 1;

        if bits > 5 {
            return None; // 不正なハフマン符号
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
                return Some((i, is_piecebox));
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
///
/// # 制限事項
///
/// **標準局面（盤上+手駒=40枚）のみ対応**
///
/// このハフマン符号化形式は、盤上の駒と手駒の合計が40枚の場合に
/// 常に256bit（32バイト）になるよう設計されています（YaneuraOu準拠）。
///
/// 駒落ち局面や詰将棋など、駒数が40枚未満の局面をpackしたデータでは、
/// 末尾の0埋め部分が歩として誤読される可能性があります。
/// 詳細は `pack_position` のドキュメントを参照してください。
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
    if black_king_sq >= 81 {
        return Err(format!("Invalid black king position: {black_king_sq}"));
    }
    board[black_king_sq as usize] = Piece::B_KING;

    // 後手玉位置 (7bit)
    let white_king_sq = stream.read_n_bit(7) as u8;
    if white_king_sq >= 81 {
        return Err(format!("Invalid white king position: {white_king_sq}"));
    }
    board[white_king_sq as usize] = Piece::W_KING;

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
    const MAX_HAND_ITERATIONS: usize = 256; // 十分に大きい値
    let mut iterations = 0;

    while stream.remaining() > 0 && iterations < MAX_HAND_ITERATIONS {
        iterations += 1;

        let (piece_idx, is_piecebox) = match decode_huffman_hand_piece(&mut stream) {
            Some(result) => result,
            None => return Err("Invalid hand piece huffman code".to_string()),
        };

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

    if iterations >= MAX_HAND_ITERATIONS {
        return Err("Hand piece parsing exceeded maximum iterations".to_string());
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
/// ## 駒打ちの判定
/// `from >= 81` の場合は駒打ち。この場合、`from - 81` が駒種インデックスになります：
/// - 1: 歩(P), 2: 香(L), 3: 桂(N), 4: 銀(S), 5: 角(B), 6: 飛(R), 7: 金(G)
///
/// ## 例
/// ```text
/// // 7g7f の場合: from=60, to=59
/// // move16 = 59 | (60 << 7) = 0x1E3B
///
/// // 2c2b+ の場合: from=11, to=10, promote=true
/// // move16 = 10 | (11 << 7) | 0x4000 = 0x458A
///
/// // P*5e の場合: to=40, piece=1(歩)
/// // move16 = 40 | (82 << 7) = 0x2928
/// ```
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

// ============================================================================
// Pack機能（Position → PackedSfen）
// ============================================================================

/// ビットストリーム書き込み用構造体
struct BitStreamWriter {
    data: [u8; 32],
    bit_cursor: usize,
}

impl BitStreamWriter {
    /// 新規作成
    fn new() -> Self {
        Self {
            data: [0u8; 32],
            bit_cursor: 0,
        }
    }

    /// 1ビット書き込む
    fn write_one_bit(&mut self, b: bool) {
        if self.bit_cursor < 256 {
            let byte_idx = self.bit_cursor / 8;
            let bit_idx = self.bit_cursor & 7;
            if b {
                self.data[byte_idx] |= 1 << bit_idx;
            }
            self.bit_cursor += 1;
        }
    }

    /// nビット書き込む（下位ビットから順に）
    fn write_n_bit(&mut self, d: u32, n: usize) {
        for i in 0..n {
            self.write_one_bit((d >> i) & 1 != 0);
        }
    }

    /// 現在のビット位置を取得
    fn bit_position(&self) -> usize {
        self.bit_cursor
    }

    /// データを取得
    fn finish(self) -> [u8; 32] {
        self.data
    }
}

/// 駒種インデックスを取得
/// 戻り値: 1=歩, 2=香, 3=桂, 4=銀, 5=角, 6=飛, 7=金
fn piece_type_to_index(pt: PieceType) -> usize {
    match pt {
        PieceType::Pawn | PieceType::ProPawn => 1,
        PieceType::Lance | PieceType::ProLance => 2,
        PieceType::Knight | PieceType::ProKnight => 3,
        PieceType::Silver | PieceType::ProSilver => 4,
        PieceType::Bishop | PieceType::Horse => 5,
        PieceType::Rook | PieceType::Dragon => 6,
        PieceType::Gold => 7,
        _ => 0, // 玉や空は0
    }
}

/// 盤上の駒をハフマン符号化して書き込む
fn write_board_piece(stream: &mut BitStreamWriter, piece: Piece) {
    if piece.is_none() {
        // 空升: 0 (1bit)
        stream.write_one_bit(false);
        return;
    }

    let pt = piece.piece_type();
    let raw_pt = pt.unpromote();
    let promoted = pt.is_promoted();
    let color = piece.color();
    let idx = piece_type_to_index(pt);

    // ハフマン符号を書き込み
    let huff = &HUFFMAN_TABLE[idx];
    stream.write_n_bit(huff.code as u32, huff.bits as usize);

    // 金以外は成りフラグ
    if raw_pt != PieceType::Gold {
        stream.write_one_bit(promoted);
    }

    // 先後フラグ (0=先手, 1=後手)
    stream.write_one_bit(color == Color::White);
}

/// 駒箱パディングを書き込む（成りフラグ=1の歩）
///
/// 駒落ち局面や詰将棋など、駒数が40枚未満の場合に256bitに達するまで
/// 駒箱フラグ（成りフラグ=1）でパディングする。unpack時に駒箱の駒は
/// 無視されるため、余った0ビットが歩として誤読される問題を防げる。
///
/// 駒箱の歩は3ビット: ハフマン(1bit=0) + 成りフラグ(1bit=1) + 先後フラグ(1bit=0)
fn write_piecebox_padding(stream: &mut BitStreamWriter) {
    // 歩のハフマンコード（手駒用）: 0x01 >> 1 = 0, 1bit
    stream.write_one_bit(false);
    // 成りフラグ = 1（駒箱フラグ）
    stream.write_one_bit(true);
    // 先後フラグ = 0（先手扱い、どちらでもよい）
    stream.write_one_bit(false);
}

/// 手駒をハフマン符号化して書き込む
/// 手駒用は盤上用のコードを1bit右シフトした形式
fn write_hand_piece(stream: &mut BitStreamWriter, pt: PieceType, color: Color) {
    let idx = piece_type_to_index(pt);
    let huff = &HUFFMAN_TABLE[idx];

    // コードを1bit右シフトして書き込み
    stream.write_n_bit((huff.code >> 1) as u32, (huff.bits - 1) as usize);

    // 金以外は成りフラグ（手駒は成っていないので0）
    if pt != PieceType::Gold {
        stream.write_one_bit(false);
    }

    // 先後フラグ
    stream.write_one_bit(color == Color::White);
}

/// Position から PackedSfen を生成
///
/// # 引数
/// * `pos` - 変換する局面
///
/// # 戻り値
/// 32バイトのPackedSfen
///
/// # 対応局面
///
/// 標準局面（40枚）および駒落ち局面・詰将棋（40枚未満）の両方に対応。
///
/// 駒数が40枚未満の場合、余ったビットは「駒箱フラグ（成りフラグ=1）」で
/// パディングされます。駒箱の駒はunpack時に無視されるため、
/// 余った0ビットが歩として誤読される問題を防止しています。
pub fn pack_position(pos: &Position) -> [u8; 32] {
    let mut stream = BitStreamWriter::new();

    // 1. 手番 (1bit): 0=先手, 1=後手
    stream.write_one_bit(pos.side_to_move() == Color::White);

    // 2. 先手玉位置 (7bit)
    let black_king_sq = pos.king_square(Color::Black);
    stream.write_n_bit(black_king_sq.index() as u32, 7);

    // 3. 後手玉位置 (7bit)
    let white_king_sq = pos.king_square(Color::White);
    stream.write_n_bit(white_king_sq.index() as u32, 7);

    // 4. 盤上の駒 (81マス、玉はスキップ)
    for sq_idx in 0..81u8 {
        let sq = Square::from_u8(sq_idx).expect("sq_idx should be in valid range 0-80");
        let piece = pos.piece_on(sq);

        // 玉はスキップ
        if piece.is_some() && piece.piece_type() == PieceType::King {
            continue;
        }

        write_board_piece(&mut stream, piece);
    }

    // 5. 手駒
    // 先手と後手の手駒を順に書き込む
    let piece_order = [
        PieceType::Rook,
        PieceType::Bishop,
        PieceType::Gold,
        PieceType::Silver,
        PieceType::Knight,
        PieceType::Lance,
        PieceType::Pawn,
    ];

    for &color in &[Color::Black, Color::White] {
        let hand = pos.hand(color);
        for &pt in &piece_order {
            let count = hand.count(pt);
            for _ in 0..count {
                write_hand_piece(&mut stream, pt, color);
            }
        }
    }

    // 6. 駒箱パディング
    // 駒落ち局面や詰将棋など駒数が40枚未満の場合、256bitに達するまで
    // 駒箱フラグ（成りフラグ=1）でパディングする。
    // 駒箱の歩は3ビットなので、残り3ビット以上あればパディングを追加。
    // 残り1-2ビットの場合は0埋めのままだが、3ビット未満では
    // 有効な手駒としてデコードされないため安全。
    while stream.bit_position() + 3 <= 256 {
        write_piecebox_padding(&mut stream);
    }

    stream.finish()
}

/// Move を Move16形式に変換
///
/// ## Move16形式
/// - bits 0-6:  移動先マス (to)
/// - bits 7-13: 移動元マス (from) または打つ駒種+81
/// - bit 14:    成りフラグ
pub fn move_to_move16(mv: Move) -> u16 {
    if mv == Move::NONE || mv == Move::NULL {
        return 0;
    }

    let to = mv.to().index() as u16;

    if mv.is_drop() {
        // 駒打ち
        let pt = mv.drop_piece_type();
        let pt_index = match pt {
            PieceType::Pawn => 1,
            PieceType::Lance => 2,
            PieceType::Knight => 3,
            PieceType::Silver => 4,
            PieceType::Bishop => 5,
            PieceType::Rook => 6,
            PieceType::Gold => 7,
            _ => 0,
        };
        to | ((81 + pt_index) << 7)
    } else {
        // 通常の移動
        let from = mv.from().index() as u16;
        let promote = if mv.is_promotion() { 0x4000 } else { 0 };
        to | (from << 7) | promote
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

    #[test]
    fn test_packed_sfen_value_from_bytes_too_short() {
        let bytes = [0u8; 39]; // 40バイト未満
        assert!(PackedSfenValue::from_bytes(&bytes).is_none());
    }

    #[test]
    fn test_unpack_sfen_invalid_black_king_position() {
        let mut data = [0u8; 32];
        // 手番: 先手 (bit 0 = 0)
        // 先手玉位置: 127 (7bit, 不正な位置)
        // bits 1-7 に 127 を設定
        data[0] = 0b11111110; // bit0=0(手番), bits1-7=0x7F(127)の下位7bit
                              // bit7は次のバイトに跨る
                              // 127 = 0b01111111, bit0が手番なので、bits1-7に入れる
                              // data[0] のbits 1-7: 127の下位6bit = 0b111111 << 1 = 0xFE
                              // data[1] の bit0: 127の最上位bit = 1
        data[0] = 0b11111110; // bits 1-7 = 0b0111111 (63)
        data[1] = 0b00000001; // bit 0 = 1 → 127 = 63 + 64

        let result = unpack_sfen(&data);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid black king position"));
    }

    #[test]
    fn test_unpack_sfen_boundary_king_position_80() {
        // 80は有効な位置（9x9盤の最後のマス）
        // このテストでは、位置80が境界値として有効であることを確認
        // 実際のunpackは盤上の駒も必要なのでエラーになる可能性があるが、
        // 境界値チェック（>= 81）には引っかからないことを確認

        let mut data = [0u8; 32];
        // 手番: 先手 (bit 0 = 0)
        // 先手玉位置: 80 (7bit) = 0b1010000
        // data[0] = 80 << 1 | 0 = 160 = 0xA0
        data[0] = 0xA0;

        // 後手玉位置: 0 (7bit) - data[1]のbits 1-7
        // このデータは完全ではないが、先手玉80がエラーにならないことを確認

        // unpack_sfen は他の理由でエラーになる可能性があるが、
        // "Invalid black king position: 80" というエラーは出ないはず
        let result = unpack_sfen(&data);
        if let Err(e) = &result {
            assert!(
                !e.contains("Invalid black king position"),
                "Position 80 should be valid, got error: {e}"
            );
        }
        // Ok の場合も、エラーの場合も、位置80の境界チェックは通過している
    }

    #[test]
    fn test_bitstream_overflow_returns_zero() {
        let data = [0b11111111u8];
        let mut stream = BitStream::new(&data);

        // 8ビット全て読む
        for _ in 0..8 {
            stream.read_one_bit();
        }

        // オーバーフロー時は0を返す
        assert_eq!(stream.read_one_bit(), 0);
        assert_eq!(stream.read_one_bit(), 0);
    }

    #[test]
    fn test_bitstream_remaining() {
        let data = [0u8; 4]; // 32ビット
        let mut stream = BitStream::new(&data);

        assert_eq!(stream.remaining(), 32);

        stream.read_one_bit();
        assert_eq!(stream.remaining(), 31);

        stream.read_n_bit(10);
        assert_eq!(stream.remaining(), 21);
    }

    #[test]
    fn test_move16_to_usi_invalid() {
        // move16 = 0 は "none" を返す
        assert_eq!(move16_to_usi(0), "none");

        // 不正な駒種インデックス (81 + 0 = 81, pt_index = 0)
        let move16 = 40 | (81 << 7);
        assert_eq!(move16_to_usi(move16), "none");

        // 不正な駒種インデックス (81 + 8 = 89)
        let move16 = 40 | (89 << 7);
        assert_eq!(move16_to_usi(move16), "none");
    }

    #[test]
    fn test_bitstream_writer() {
        let mut writer = BitStreamWriter::new();
        writer.write_one_bit(false);
        writer.write_one_bit(true);
        writer.write_one_bit(false);
        writer.write_one_bit(true);

        let data = writer.finish();
        // ビット順: 0, 1, 0, 1 → バイト[0] = 0b00001010 = 10
        assert_eq!(data[0] & 0x0F, 0b1010);
    }

    #[test]
    fn test_pack_unpack_roundtrip_hirate() {
        // 平手初期局面でのroundtripテスト
        let mut pos = Position::new();
        pos.set_hirate();

        // pack
        let packed = pack_position(&pos);

        // unpack
        let sfen = unpack_sfen(&packed).expect("unpack should succeed");

        // 平手SFENと比較（手数部分を除く）
        let expected = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";
        assert!(
            sfen.starts_with(expected),
            "SFEN mismatch:\n  got: {sfen}\n  expected: {expected}"
        );
    }

    #[test]
    fn test_pack_unpack_roundtrip_with_hands() {
        // 手駒ありの局面
        // 盤上の歩を1枚減らして（7九→空）、先手の手駒に歩1枚
        let mut pos = Position::new();
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPP1P/1B5R1/LNSGKGSNL b P 1";
        pos.set_sfen(sfen).expect("set_sfen should succeed");

        // pack
        let packed = pack_position(&pos);

        // unpack
        let unpacked_sfen = unpack_sfen(&packed).expect("unpack should succeed");

        // 手駒部分を確認（先手に歩1枚）
        assert!(
            unpacked_sfen.contains(" P ") || unpacked_sfen.contains(" P 1"),
            "Hand pieces should be preserved: {unpacked_sfen}"
        );
    }

    #[test]
    fn test_pack_unpack_roundtrip_promoted() {
        // 成り駒を含む局面
        let mut pos = Position::new();
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1+B5R1/LNSGKGSNL b - 1";
        pos.set_sfen(sfen).expect("set_sfen should succeed");

        // pack
        let packed = pack_position(&pos);

        // unpack
        let unpacked_sfen = unpack_sfen(&packed).expect("unpack should succeed");

        // 成り角が保存されているか確認
        assert!(
            unpacked_sfen.contains("+B"),
            "Promoted bishop should be preserved: {unpacked_sfen}"
        );
    }

    #[test]
    fn test_move_to_move16_and_back() {
        // 7七(file=7, rank=7)のマス番号 = (7-1)*9 + (7-1) = 54 + 6 = 60
        // 7六(file=7, rank=6)のマス番号 = (7-1)*9 + (6-1) = 54 + 5 = 59
        let sq_77 = Square::from_u8(60).unwrap();
        let sq_76 = Square::from_u8(59).unwrap();

        // 通常の移動
        let mv = Move::new_move(sq_77, sq_76, false);
        let move16 = move_to_move16(mv);
        let mv_back = move16_to_move(move16);
        assert_eq!(mv, mv_back);

        // 2三(file=2, rank=3)のマス番号 = (2-1)*9 + (3-1) = 9 + 2 = 11
        // 2二(file=2, rank=2)のマス番号 = (2-1)*9 + (2-1) = 9 + 1 = 10
        let sq_23 = Square::from_u8(11).unwrap();
        let sq_22 = Square::from_u8(10).unwrap();

        // 成り
        let mv = Move::new_move(sq_23, sq_22, true);
        let move16 = move_to_move16(mv);
        let mv_back = move16_to_move(move16);
        assert_eq!(mv, mv_back);

        // 駒打ち
        let mv = Move::new_drop(PieceType::Pawn, Square::SQ_55);
        let move16 = move_to_move16(mv);
        let mv_back = move16_to_move(move16);
        assert_eq!(mv, mv_back);
    }

    #[test]
    fn test_packed_sfen_value_to_bytes_roundtrip() {
        let psv = PackedSfenValue {
            sfen: [
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
                24, 25, 26, 27, 28, 29, 30, 31, 32,
            ],
            score: -123,
            move16: 0x1234,
            game_ply: 42,
            game_result: -1,
            padding: 0,
        };

        let bytes = psv.to_bytes();
        let psv2 = PackedSfenValue::from_bytes(&bytes).unwrap();

        assert_eq!(psv.sfen, psv2.sfen);
        assert_eq!(psv.score, psv2.score);
        assert_eq!(psv.move16, psv2.move16);
        assert_eq!(psv.game_ply, psv2.game_ply);
        assert_eq!(psv.game_result, psv2.game_result);
    }
}
