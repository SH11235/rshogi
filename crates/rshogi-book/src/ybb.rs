//! YaneuraOu のバイナリ定跡 `.ybb` のリーダ。
//!
//! 形式 (数値はすべて little endian):
//! - header 32 byte: magic `YANE-BINBOOK-V1\0` (16 byte)、局面数 u64、flags u64
//!   (bit0 = 手ごとに depth を持つ。未知の bit があれば読まない)
//! - index: 44 byte × 局面数。PackedSfen 32 byte、moves 領域先頭からの offset u64、
//!   手数 u16、手の数 u16。PackedSfen のバイト順 (memcmp) で重複なく並ぶ
//! - moves: index の直後。1 手 4 byte (Move16 u16、評価値 i16)、flags bit0 なら
//!   6 byte (+ depth u16)
//!
//! 局面は SFEN から YaneuraOu 形式の PackedSfen を作り、index を二分探索して引く。
//! ponder と採択回数は持たないので、それぞれ「指し手なし」と 0 として返す。

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use crate::reader::{PositionEntry, RawBookMove, parse_move_field};

pub(crate) const MAGIC: &[u8; 16] = b"YANE-BINBOOK-V1\0";
const HEADER_SIZE: u64 = 32;
const INDEX_RECORD_SIZE: u64 = 44;
const FLAG_MOVE_DEPTH: u64 = 1;
const KNOWN_FLAGS: u64 = FLAG_MOVE_DEPTH;

/// `.ybb` の中身の置き場。
#[derive(Debug)]
enum Source {
    /// ファイル全体を読み込んだもの。
    Memory(Vec<u8>),
    /// 開いたままのファイル。probe のたびに必要な所だけ読む。
    File(Mutex<File>),
}

/// 開いた `.ybb`。
#[derive(Debug)]
pub(crate) struct YbbBook {
    source: Source,
    len: u64,
    record_count: u64,
    move_record_size: u64,
}

/// 検証済みの header。
struct Header {
    record_count: u64,
    move_record_size: u64,
    /// index の終わり (= moves 領域の先頭) のバイト位置。
    index_end: u64,
}

impl Header {
    fn parse(header: &[u8; HEADER_SIZE as usize]) -> io::Result<Self> {
        if &header[..16] != MAGIC {
            return Err(invalid("not a ybb file (magic mismatch)"));
        }
        let record_count = u64::from_le_bytes(header[16..24].try_into().unwrap_or_default());
        let flags = u64::from_le_bytes(header[24..32].try_into().unwrap_or_default());
        if flags & !KNOWN_FLAGS != 0 {
            return Err(invalid(format!("ybb file has unknown flags {flags:#x}")));
        }
        let index_end = record_count
            .checked_mul(INDEX_RECORD_SIZE)
            .and_then(|n| n.checked_add(HEADER_SIZE))
            .ok_or_else(|| invalid(format!("ybb index of {record_count} positions overflows")))?;
        Ok(Self {
            record_count,
            move_record_size: if flags & FLAG_MOVE_DEPTH != 0 { 6 } else { 4 },
            index_end,
        })
    }

    /// `path` のファイルの header を読んで検証し、index がファイルに収まることも確かめる。
    fn read_file(path: &Path) -> io::Result<(Self, File, u64)> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let mut bytes = [0u8; HEADER_SIZE as usize];
        file.read_exact(&mut bytes)
            .map_err(|_| invalid("ybb file is shorter than its header"))?;
        let header = Self::parse(&bytes)?;
        header.check_len(len)?;
        Ok((header, file, len))
    }

    fn check_len(&self, len: u64) -> io::Result<()> {
        if self.index_end > len {
            return Err(invalid(format!(
                "ybb index of {} positions does not fit in {len} bytes",
                self.record_count
            )));
        }
        Ok(())
    }
}

impl YbbBook {
    fn new(source: Source, len: u64, header: Header) -> Self {
        Self {
            source,
            len,
            record_count: header.record_count,
            move_record_size: header.move_record_size,
        }
    }

    /// magic を読んだ後の `reader` から残りを読み込む。header を検証してから本体を読む。
    pub(crate) fn from_reader<R: Read>(magic: Vec<u8>, mut reader: R) -> io::Result<Self> {
        let mut bytes = magic;
        (&mut reader).take(HEADER_SIZE - bytes.len() as u64).read_to_end(&mut bytes)?;
        let header_bytes: [u8; HEADER_SIZE as usize] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| invalid("ybb file is shorter than its header"))?;
        let header = Header::parse(&header_bytes)?;
        reader.read_to_end(&mut bytes)?;
        let len = bytes.len() as u64;
        header.check_len(len)?;
        Ok(Self::new(Source::Memory(bytes), len, header))
    }

    /// ファイル全体を読み込む。header とファイル長を確かめてから読む。
    pub(crate) fn read_file(path: &Path) -> io::Result<Self> {
        let (header, mut file, len) = Header::read_file(path)?;
        // 確保に失敗しても abort せず、read_to_end と同じく Err で返す。
        let mut bytes = Vec::new();
        usize::try_from(len)
            .ok()
            .and_then(|n| bytes.try_reserve_exact(n).ok())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::OutOfMemory, "ybb file does not fit in memory")
            })?;
        file.seek(SeekFrom::Start(0))?;
        file.read_to_end(&mut bytes)?;
        let len = bytes.len() as u64;
        header.check_len(len)?;
        Ok(Self::new(Source::Memory(bytes), len, header))
    }

    /// ファイルを開くだけで、index も moves も読み込まない。
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let (header, file, len) = Header::read_file(path)?;
        Ok(Self::new(Source::File(Mutex::new(file)), len, header))
    }

    pub(crate) fn record_count(&self) -> u64 {
        self.record_count
    }

    /// 開いたままのファイルから引くなら、そのバイト数。
    pub(crate) fn file_len(&self) -> Option<u64> {
        match self.source {
            Source::Memory(_) => None,
            Source::File(_) => Some(self.len),
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|&end| end <= self.len)
            .ok_or_else(|| invalid(format!("ybb read past the end of file at byte {offset}")))?;
        match &self.source {
            Source::Memory(bytes) => buf.copy_from_slice(&bytes[offset as usize..end as usize]),
            Source::File(file) => {
                // ロック中に panic してもファイルハンドル自体は壊れない (読み出し位置は毎回 seek し直す)。
                let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                file.seek(SeekFrom::Start(offset))?;
                file.read_exact(buf)?;
            }
        }
        Ok(())
    }

    /// 局面を引く。`ignore_ply` が `false` なら手数も一致したときだけ返す。PackedSfen に
    /// できない SFEN は引けない局面として扱う。手の数が 0 の局面も空のエントリとして返す
    /// (呼び出し側で FlippedBook の反転局面を引き直さないため)。
    pub(crate) fn find(&self, sfen: &str, ignore_ply: bool) -> io::Result<Option<PositionEntry>> {
        let Some((target, ply)) = pack_sfen(sfen) else {
            return Ok(None);
        };
        let mut record = [0u8; INDEX_RECORD_SIZE as usize];
        let (mut lo, mut hi) = (0u64, self.record_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            self.read_at(HEADER_SIZE + mid * INDEX_RECORD_SIZE, &mut record)?;
            match target.as_slice().cmp(&record[..32]) {
                std::cmp::Ordering::Less => hi = mid,
                std::cmp::Ordering::Greater => lo = mid + 1,
                std::cmp::Ordering::Equal => {
                    let record_ply = u16::from_le_bytes([record[40], record[41]]);
                    if !ignore_ply && u64::from(record_ply) != ply {
                        return Ok(None);
                    }
                    let offset = u64::from_le_bytes(record[32..40].try_into().unwrap_or_default());
                    let count = u16::from_le_bytes([record[42], record[43]]);
                    let moves = self.read_moves(offset, count)?;
                    return Ok(Some(PositionEntry { moves }));
                }
            }
        }
        Ok(None)
    }

    fn read_moves(&self, offset: u64, count: u16) -> io::Result<Vec<RawBookMove>> {
        let start = (HEADER_SIZE + self.record_count * INDEX_RECORD_SIZE)
            .checked_add(offset)
            .ok_or_else(|| invalid(format!("ybb moves offset {offset} overflows")))?;
        let size = self.move_record_size as usize;
        let mut buf = vec![0u8; usize::from(count) * size];
        self.read_at(start, &mut buf)?;
        Ok(buf
            .chunks_exact(size)
            .map(|m| RawBookMove {
                move_usi: parse_move_field(&move16_to_usi(u16::from_le_bytes([m[0], m[1]]))),
                ponder_usi: None,
                value: i32::from(i16::from_le_bytes([m[2], m[3]])),
                depth: if size == 6 {
                    i32::from(u16::from_le_bytes([m[4], m[5]]))
                } else {
                    0
                },
                move_count: 0,
            })
            .collect())
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

// 駒の値は「先後 << 4 | 駒種」で、成りは駒種に +8。
const PAWN: u8 = 1;
const GOLD: u8 = 7;
const KING: u8 = 8;
const PROMOTE: u8 = 8;
const WHITE: u8 = 16;
const SQ_NB: u8 = 81;

/// 手駒と駒箱を書き出す駒種の順 (歩香桂銀金角飛)。駒種を表す整数値の順とは違う。
const PACK_HAND_ORDER: [u8; 7] = [1, 2, 3, 4, 7, 5, 6];

/// 駒種 (成りを除く) ごとの盤上のハフマン符号 (code, bits)。添字 0 は空き升。
const HUFFMAN: [(u32, u32); 8] = [
    (0x00, 1),
    (0x01, 2),
    (0x03, 4),
    (0x0b, 4),
    (0x07, 4),
    (0x1f, 6),
    (0x3f, 6),
    (0x0f, 5),
];
/// 駒箱 (盤にも手駒にも無い駒) の符号。
const HUFFMAN_PIECEBOX: [(u32, u32); 8] = [
    (0x00, 1),
    (0x02, 2),
    (0x09, 4),
    (0x0d, 4),
    (0x0b, 4),
    (0x2f, 6),
    (0x3f, 6),
    (0x1b, 5),
];
/// 平手の駒種ごとの枚数。足りない分を駒箱として書く。
const ALL_PIECES: [i32; 8] = [0, 18, 4, 4, 4, 2, 2, 4];

struct BitWriter {
    data: [u8; 32],
    cursor: usize,
}

impl BitWriter {
    fn bit(&mut self, value: bool) -> Option<()> {
        if self.cursor >= 256 {
            return None;
        }
        if value {
            self.data[self.cursor / 8] |= 1 << (self.cursor % 8);
        }
        self.cursor += 1;
        Some(())
    }

    fn bits(&mut self, value: u32, n: u32) -> Option<()> {
        (0..n).try_for_each(|i| self.bit(value & (1 << i) != 0))
    }
}

/// SFEN を YaneuraOu の PackedSfen (32 byte) と手数にする。手数が無ければ 1。
///
/// `.ybb` の index のキーとビット単位で一致させるため、盤面の合法性は問わない。
/// 玉の無い局面は玉の位置を 81 にする。
/// 駒が多すぎるなど 256 bit に収まらなければ `None`。
pub(crate) fn pack_sfen(sfen: &str) -> Option<([u8; 32], u64)> {
    let mut tokens = sfen.split_whitespace();
    let board_text = tokens.next()?;
    let turn = match tokens.next()? {
        "b" => false,
        "w" => true,
        _ => return None,
    };
    let hand_text = tokens.next()?;
    let ply = match tokens.next() {
        Some(t) => t.parse().ok()?,
        None => 1,
    };

    // 升の番号は (筋 - 1) * 9 + (段 - 1)。
    let mut board = [0u8; 81];
    let ranks: Vec<&str> = board_text.split('/').collect();
    if ranks.len() != 9 {
        return None;
    }
    for (rank, text) in ranks.iter().enumerate() {
        let mut file = 9i32;
        let mut promote = false;
        for c in text.chars() {
            if let Some(n) = c.to_digit(10) {
                if promote {
                    return None;
                }
                file -= n as i32;
                continue;
            }
            if c == '+' {
                if promote {
                    return None;
                }
                promote = true;
                continue;
            }
            let mut piece = piece_from_char(c)?;
            if !(1..=9).contains(&file) {
                return None;
            }
            if promote {
                if piece & 15 >= GOLD {
                    return None;
                }
                piece |= PROMOTE;
            }
            board[(file - 1) as usize * 9 + rank] = piece;
            file -= 1;
            promote = false;
        }
        if promote || file != 0 {
            return None;
        }
    }

    let mut hands = [[0i32; 8]; 2];
    if hand_text != "-" {
        let mut count: Option<i32> = None;
        for c in hand_text.chars() {
            if let Some(d) = c.to_digit(10) {
                count = Some(count.unwrap_or(0).checked_mul(10)?.checked_add(d as i32)?);
                continue;
            }
            let piece = piece_from_char(c)?;
            let piece_type = piece & 15;
            if !(PAWN..KING).contains(&piece_type) {
                return None;
            }
            let slot = &mut hands[usize::from(piece >> 4)][usize::from(piece_type)];
            *slot = slot.checked_add(count.take().unwrap_or(1))?;
        }
        if count.is_some() {
            return None;
        }
    }

    let mut w = BitWriter {
        data: [0; 32],
        cursor: 0,
    };
    w.bit(turn)?;
    for color in [0, WHITE] {
        let king = board.iter().position(|&p| p == color | KING).map_or(SQ_NB, |sq| sq as u8);
        w.bits(u32::from(king), 7)?;
    }
    let mut piecebox = ALL_PIECES;
    for &piece in &board {
        if piece & 15 == KING {
            continue;
        }
        let raw = usize::from(piece & 7);
        let (code, bits) = HUFFMAN[raw];
        w.bits(code, bits)?;
        if piece == 0 {
            continue;
        }
        if raw != usize::from(GOLD) {
            w.bit(piece & PROMOTE != 0)?;
        }
        w.bit(piece & WHITE != 0)?;
        piecebox[raw] -= 1;
    }
    for (color, hand) in hands.iter().enumerate() {
        for piece_type in PACK_HAND_ORDER {
            let pt = usize::from(piece_type);
            let (code, bits) = HUFFMAN[pt];
            for _ in 0..hand[pt] {
                w.bits(code >> 1, bits - 1)?;
                if piece_type != GOLD {
                    w.bit(false)?;
                }
                w.bit(color == 1)?;
            }
            piecebox[pt] -= hand[pt];
        }
    }
    for piece_type in PACK_HAND_ORDER {
        let pt = usize::from(piece_type);
        let (code, bits) = HUFFMAN_PIECEBOX[pt];
        if piecebox[pt] < 0 {
            return None;
        }
        for _ in 0..piecebox[pt] {
            w.bits(code, bits)?;
            if piece_type != GOLD {
                w.bit(false)?;
            }
        }
    }
    (w.cursor == 256).then_some((w.data, ply))
}

fn piece_from_char(c: char) -> Option<u8> {
    let index = "PLNSBRGK".find(c.to_ascii_uppercase())? as u8 + 1;
    Some(if c.is_ascii_lowercase() {
        WHITE | index
    } else {
        index
    })
}

/// YaneuraOu の Move16 を USI 文字列にする。特別な値と解釈できない値は
/// `none` / `null` / `resign` / `win` にする。
fn move16_to_usi(m: u16) -> String {
    const MOVE_NULL: u16 = (1 << 7) + 1;
    const MOVE_RESIGN: u16 = (2 << 7) + 2;
    const MOVE_WIN: u16 = (3 << 7) + 3;
    match m {
        MOVE_NULL => "null".to_string(),
        MOVE_RESIGN => "resign".to_string(),
        MOVE_WIN => "win".to_string(),
        _ => decode_move16(m).unwrap_or_else(|| "none".to_string()),
    }
}

fn decode_move16(m: u16) -> Option<String> {
    const MOVE_DROP: u16 = 1 << 14;
    const MOVE_PROMOTE: u16 = 1 << 15;
    let square = |sq: u16| {
        (sq < 81).then(|| {
            format!("{}{}", char::from(b'1' + (sq / 9) as u8), char::from(b'a' + (sq % 9) as u8))
        })
    };
    let to = m & 0x7f;
    let from = (m >> 7) & 0x7f;
    if m & MOVE_DROP != 0 {
        let piece = "PLNSBRG".get(usize::from(from).checked_sub(1)?..usize::from(from))?;
        return Some(format!("{piece}*{}", square(to)?));
    }
    if from == to {
        return None;
    }
    let suffix = if m & MOVE_PROMOTE != 0 { "+" } else { "" };
    Some(format!("{}{}{suffix}", square(from)?, square(to)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Book, BookEntryMove, BookOptions, DefaultBookRng, flipped_key, probe};
    use rshogi_core::position::Position;

    // testdata の .ybb は small.db を YaneuraOu-ScriptCollection の変換器で変換したもの:
    // convert_db_to_ybb(small.db, small[_depth], work, 500000, 512 MiB, 64, include_depth)
    const SMALL_DB: &[u8] = include_bytes!("../testdata/small.db");
    const SMALL_YBB: &[u8] = include_bytes!("../testdata/small.ybb");
    const SMALL_DEPTH_YBB: &[u8] = include_bytes!("../testdata/small_depth.ybb");

    fn testdata(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata").join(name)
    }

    fn db_sfens() -> Vec<String> {
        String::from_utf8(SMALL_DB.to_vec())
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("sfen ").map(str::to_string))
            .collect()
    }

    /// 本の局面・手数違い・先後反転を問い合わせ、`.db` と同じ結果になることを確かめる。採択回数は `.ybb` が持たないので比べない。
    fn assert_matches_db(ybb: &Book, db: &Book, with_depth: bool) {
        let options = [
            BookOptions {
                book_moves: 10000,
                eval_diff: 10000,
                ..Default::default()
            },
            BookOptions {
                book_moves: 10000,
                select_value: true,
                ..Default::default()
            },
            BookOptions {
                book_moves: 10000,
                eval_diff: 100,
                eval_black_limit: -100,
                eval_white_limit: -100,
                ..Default::default()
            },
        ];
        let mut hits = 0;
        for sfen in db_sfens() {
            let other_ply = format!("{} 99", crate::reader::strip_ply(&sfen));
            let flipped = flipped_key(&sfen).unwrap();
            for query in [&sfen, &other_ply, &flipped] {
                let mut expected = db.entry_moves(query).unwrap();
                for m in &mut expected {
                    m.move_count = 0;
                    if !with_depth {
                        m.depth = 0;
                    }
                }
                let actual = ybb.entry_moves(query).unwrap();
                assert_eq!(actual, expected, "{query}");
                hits += usize::from(!actual.is_empty());

                let mut position = Position::new();
                position.set_sfen(query).unwrap();
                for (i, opt) in options.iter().enumerate() {
                    let opt = BookOptions {
                        flipped_book: true,
                        depth_limit: if with_depth { 10 } else { 0 },
                        ..opt.clone()
                    };
                    for seed in 0..8 {
                        let run = |book: &Book| {
                            probe(
                                std::slice::from_ref(book),
                                &position,
                                &opt,
                                &mut DefaultBookRng::from_seed(seed),
                                |_| {},
                            )
                        };
                        assert_eq!(run(ybb), run(db), "{query} options #{i} seed {seed}");
                    }
                }
            }
        }
        assert!(hits >= 8, "hits={hits}");
    }

    #[test]
    fn matches_db_book_in_memory_and_on_the_fly() {
        for ignore_ply in [true, false] {
            let db = Book::from_bytes(SMALL_DB, ignore_ply).unwrap();
            for (bytes, name, with_depth) in [
                (SMALL_YBB, "small.ybb", false),
                (SMALL_DEPTH_YBB, "small_depth.ybb", true),
            ] {
                let memory = Book::from_bytes(bytes, ignore_ply).unwrap();
                assert_eq!(memory.position_count(), Some(8));
                assert_eq!(memory.on_the_fly_file_len(), None);
                assert_matches_db(&memory, &db, with_depth);

                let path = testdata(name);
                let from_path = Book::from_path(&path, ignore_ply).unwrap();
                assert_matches_db(&from_path, &db, with_depth);
                let otf = Book::open_on_the_fly(&path, ignore_ply).unwrap();
                assert_eq!(otf.on_the_fly_file_len(), Some(bytes.len() as u64));
                assert_matches_db(&otf, &db, with_depth);
            }
        }
    }

    #[test]
    fn ply_must_match_unless_ignored() {
        let hirate_99 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 99";
        let strict = Book::from_bytes(SMALL_YBB, false).unwrap();
        assert!(strict.entry_moves(hirate_99).unwrap().is_empty());
        let ignore = Book::from_bytes(SMALL_YBB, true).unwrap();
        let moves = ignore.entry_moves(hirate_99).unwrap();
        let expected = [("7g7f", 50), ("2g2f", 50), ("5g5f", -120)];
        let got: Vec<_> = moves.iter().map(|m| (m.move_usi.as_str(), m.value)).collect();
        assert_eq!(got, expected);
        let depth = Book::from_bytes(SMALL_DEPTH_YBB, true).unwrap();
        assert_eq!(
            depth.entry_moves(hirate_99).unwrap()[2],
            BookEntryMove {
                move_usi: "5g5f".to_string(),
                value: -120,
                depth: 65535,
                move_count: 0,
            }
        );
    }

    /// 実際の `.ybb` を on the fly で開いて平手を引き、変換元 (か変換先) の `.db` があれば、
    /// `.db` から等間隔に抜いた局面 (手数違い・先後反転を含む) で引いた結果と probe の選択が一致する
    /// ことを確かめる。`.db` は sfen 行が手数を除いた SFEN 順に並んでいること (on the fly で引く)。
    /// `.ybb` が持たないものは比べない: ponder (probe は指し手だけ比べる)、depth なしの `.ybb` の depth。
    /// probe は `book_moves=10000`・`eval_diff=10000`・`FlippedBook=true` で、count を使う
    /// `NarrowBook` / `ConsiderBookMoveCount` は使わない。一致は `.db` の count が局面内で揃っている
    /// ことが前提。
    /// `RSHOGI_YBB_PATH=<.ybb> [RSHOGI_YBB_DB_PATH=<.db>] cargo test --release -p rshogi-book -- --ignored real_ybb`
    #[test]
    #[ignore = "RSHOGI_YBB_PATH の定跡ファイルが要る"]
    fn real_ybb_matches_db() {
        use std::io::{BufRead, BufReader};

        let ybb_path = std::env::var("RSHOGI_YBB_PATH").expect("RSHOGI_YBB_PATH");
        let samples: u64 = std::env::var("RSHOGI_YBB_SAMPLES").map_or(4000, |s| s.parse().unwrap());
        let ybb = Book::open_on_the_fly(&ybb_path, true).unwrap();
        let mut header = [0u8; HEADER_SIZE as usize];
        File::open(&ybb_path).unwrap().read_exact(&mut header).unwrap();
        let with_depth = Header::parse(&header).unwrap().move_record_size == 6;
        let hirate = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let moves = ybb.entry_moves(hirate).unwrap();
        eprintln!("{} positions, hirate: {moves:?}", ybb.position_count().unwrap());
        assert!(!moves.is_empty());
        let Ok(db_path) = std::env::var("RSHOGI_YBB_DB_PATH") else {
            return;
        };

        let mut file = BufReader::new(File::open(&db_path).unwrap());
        let len = file.get_ref().metadata().unwrap().len();
        let mut sfens = Vec::new();
        let mut line = String::new();
        for i in 0..samples {
            file.seek(SeekFrom::Start(len * i / samples)).unwrap();
            file.read_line(&mut line).unwrap();
            loop {
                line.clear();
                if file.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                if let Some(sfen) = line.trim_end().strip_prefix("sfen ") {
                    sfens.push(sfen.to_string());
                    break;
                }
            }
        }
        sfens.dedup();
        let options = BookOptions {
            book_moves: 10000,
            eval_diff: 10000,
            ..Default::default()
        };
        for ignore_ply in [true, false] {
            let db = Book::open_on_the_fly(&db_path, ignore_ply).unwrap();
            let ybb = Book::open_on_the_fly(&ybb_path, ignore_ply).unwrap();
            let (mut queries, mut hits) = (0usize, 0usize);
            for sfen in &sfens {
                let mut position = Position::new();
                if position.set_sfen(sfen).is_err() {
                    continue;
                }
                let canonical = position.to_sfen();
                let other_ply = format!("{} 999", crate::reader::strip_ply(&canonical));
                let Some(flipped) = flipped_key(&canonical) else {
                    continue;
                };
                for query in [&canonical, &other_ply, &flipped] {
                    let mut expected = db.entry_moves(query).unwrap();
                    for m in &mut expected {
                        m.move_count = 0;
                        if !with_depth {
                            m.depth = 0;
                        }
                    }
                    assert_eq!(ybb.entry_moves(query).unwrap(), expected, "{query}");
                    queries += 1;
                    hits += usize::from(!expected.is_empty());
                }
                for query in [&canonical, &flipped] {
                    position.set_sfen(query).unwrap();
                    let run = |book: &Book| {
                        probe(
                            std::slice::from_ref(book),
                            &position,
                            &options,
                            &mut DefaultBookRng::from_seed(1),
                            |_| {},
                        )
                        .map(|r| r.best_move)
                    };
                    assert_eq!(run(&ybb), run(&db), "{query}");
                }
            }
            eprintln!(
                "ignore_ply={ignore_ply}: {} sampled sfen lines, {queries} queries, {hits} hits",
                sfens.len()
            );
        }
    }

    /// depth なしの `.ybb` を組み立てる。手は (Move16, 評価値)。
    fn build_ybb(records: &[(&str, &[(u16, i16)])]) -> Vec<u8> {
        let mut packed: Vec<_> = records
            .iter()
            .map(|(sfen, moves)| {
                let (key, ply) = pack_sfen(sfen).unwrap();
                (key, ply as u16, *moves)
            })
            .collect();
        packed.sort_by_key(|r| r.0);
        let mut out = MAGIC.to_vec();
        out.extend((packed.len() as u64).to_le_bytes());
        out.extend(0u64.to_le_bytes());
        let mut moves_area = Vec::new();
        for (key, ply, moves) in &packed {
            out.extend(key);
            out.extend((moves_area.len() as u64).to_le_bytes());
            out.extend(ply.to_le_bytes());
            out.extend((moves.len() as u16).to_le_bytes());
            for (m, v) in *moves {
                moves_area.extend(m.to_le_bytes());
                moves_area.extend(v.to_le_bytes());
            }
        }
        out.extend(moves_area);
        out
    }

    #[test]
    fn position_without_moves_does_not_fall_back_to_flipped() {
        let hirate_b = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let hirate_w = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1";
        // 3c3d: 3c = 2*9+2, 3d = 2*9+3。
        let bytes = build_ybb(&[(hirate_b, &[]), (hirate_w, &[((20 << 7) | 21, 10)])]);
        let book = Book::from_bytes(&bytes, true).unwrap();
        assert_eq!(book.find_raw(hirate_b).unwrap().map(|e| e.moves.len()), Some(0));
        assert!(book.entry_moves(hirate_b).unwrap().is_empty());
        let options = BookOptions {
            flipped_book: true,
            ..Default::default()
        };
        let run = |sfen: &str| {
            let mut position = Position::new();
            position.set_sfen(sfen).unwrap();
            probe(
                std::slice::from_ref(&book),
                &position,
                &options,
                &mut DefaultBookRng::from_seed(0),
                |_| {},
            )
            .map(|r| r.best_move.to_usi())
        };
        assert_eq!(run(hirate_b), None);
        assert_eq!(run(hirate_w), Some("3c3d".to_string()));
    }

    #[test]
    fn position_without_moves_falls_through_to_next_priority_book() {
        let hirate_b = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        // 2g2f: 2g = 1*9+6, 2f = 1*9+5。
        let books = [
            Book::from_bytes(&build_ybb(&[(hirate_b, &[])]), false).unwrap(),
            Book::from_bytes(&build_ybb(&[(hirate_b, &[((15 << 7) | 14, 10)])]), false).unwrap(),
        ];
        let mut position = Position::new();
        position.set_sfen(hirate_b).unwrap();
        let best = probe(
            &books,
            &position,
            &BookOptions::default(),
            &mut DefaultBookRng::from_seed(0),
            |_| {},
        )
        .map(|r| r.best_move.to_usi());
        assert_eq!(best.as_deref(), Some("2g2f"));
    }

    #[test]
    fn header_is_checked_before_reading_the_body() {
        struct Fail;
        impl Read for Fail {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("body must not be read"))
            }
        }
        let mut unknown_flag = SMALL_YBB[..32].to_vec();
        unknown_flag[24] = 2;
        let mut overflow = SMALL_YBB[..32].to_vec();
        overflow[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        for header in [unknown_flag, overflow] {
            let err = Book::from_reader(header.as_slice().chain(Fail), true).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        }
    }

    #[test]
    fn ybb_extension_with_wrong_magic_is_rejected() {
        let dir =
            std::env::temp_dir().join(format!("rshogi_book_ybb_magic_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut bytes = b"XANE-BINBOOK-V1\0".to_vec();
        bytes.extend([0u8; 16]);
        let short = dir.join("short.ybb");
        std::fs::write(&short, b"YANE").unwrap();
        let broken = dir.join("broken.ybb");
        std::fs::write(&broken, &bytes).unwrap();
        for path in [&broken, &short] {
            for on_the_fly in [false, true] {
                let result = if on_the_fly {
                    Book::open_on_the_fly(path, true)
                } else {
                    Book::from_path(path, true)
                };
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
            }
        }
        // 拡張子が .ybb でなければテキストの定跡として読む。
        let text = dir.join("broken.db");
        std::fs::write(&text, &bytes).unwrap();
        assert_eq!(Book::from_path(&text, true).unwrap().position_count(), Some(0));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rejects_broken_header_and_truncated_index() {
        let mut unknown_flag = SMALL_YBB.to_vec();
        unknown_flag[24] = 2;
        let mut too_many = SMALL_YBB.to_vec();
        too_many[16] = 11;
        for bytes in [&unknown_flag[..], &too_many[..], &SMALL_YBB[..20]] {
            let err = Book::from_bytes(bytes, true).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn truncated_moves_are_a_read_error() {
        let ybb = Book::from_bytes(&SMALL_YBB[..SMALL_YBB.len() - 2], true).unwrap();
        let errors = db_sfens().iter().filter(|s| ybb.entry_moves(s).is_err()).count();
        assert_eq!(errors, 1);
    }

    #[test]
    fn pack_rejects_what_does_not_fit_256_bits() {
        // 玉が欠けると 1 bit 余り、YaneuraOu の PackedSfen にならない。
        assert_eq!(pack_sfen("4k4/9/9/9/9/9/9/9/9 b 2G 1"), None);
        assert_eq!(pack_sfen("4k4/9/9/9/9/9/9/9/4K4 b 19P 1"), None);
        assert!(pack_sfen("4k4/9/9/9/9/9/9/9/4K4 b 18P 1").is_some());
        // 手駒の枚数の和が i32 を超えても panic しない。
        assert_eq!(pack_sfen("4k4/9/9/9/9/9/9/9/4K4 b 2147483647P1P 1"), None);
        let book = Book::from_bytes(SMALL_YBB, true).unwrap();
        assert!(book.entry_moves("4k4/9/9/9/9/9/9/9/4K4 b 2147483647P1P 1").unwrap().is_empty());
    }

    #[test]
    fn move16_round_trip_of_special_values() {
        assert_eq!(move16_to_usi(0), "none");
        assert_eq!(move16_to_usi((2 << 7) + 2), "resign");
        // 7g7f: from = 6*9+6, to = 6*9+5。打ち: 歩 (1) を 5e (4*9+4) に。
        assert_eq!(move16_to_usi((60 << 7) | 59), "7g7f");
        assert_eq!(move16_to_usi((1 << 14) | (1 << 7) | 40), "P*5e");
        assert_eq!(move16_to_usi((1 << 15) | (70 << 7) | 10), "8h2b+");
    }
}
