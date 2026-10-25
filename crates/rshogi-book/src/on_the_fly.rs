//! 定跡ファイルを丸読みせず、probe のたびにファイル上の二分探索で局面を引く(BookOnTheFly)。
//!
//! 前提はファイルの `sfen` 行が手数を除いた SFEN のバイト順に並んでいること。手数込みの
//! SFEN で並べたファイル(YaneuraOu の定跡書き出し)もこの順になる: 手数の直前の空白は
//! 盤面・手番・持ち駒に現れるどの文字よりも小さいので、手数を除いても前後関係は変わらず、
//! 手数違いの同一局面は隣り合う。
//!
//! 引き方は「手数を除いたキーが目的以上になる最初の `sfen` 行」をバイト位置の二分探索で
//! 求め、そこから手数を除いたキーが一致する間の行を読む。手数込みで引くときは、その中から
//! 手数込みのキーが一致する局面だけを拾う。丸読み([`crate::Book::from_reader`])は同じキーの
//! 局面をファイル順に連結するので、隣り合った範囲をファイル順に読めば結果が一致する。

use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use crate::reader::{Line, PositionEntry, classify_line, normalize_key, strip_ply};

/// 開くときに並びを確かめる抜き取り点の数。
const ORDER_CHECK_SAMPLES: u64 = 1024;

/// on the fly で引く定跡ファイル。ファイルハンドルは開いたまま保持する。
#[derive(Debug)]
pub(crate) struct OnTheFlyFile {
    file: Mutex<File>,
    len: u64,
}

/// ファイル上の読み出し位置と、読んだ行のバッファ。
struct LineReader<'a> {
    reader: BufReader<&'a File>,
    pos: u64,
    line: Vec<u8>,
}

impl<'a> LineReader<'a> {
    fn new(file: &'a File) -> Self {
        Self {
            reader: BufReader::new(file),
            pos: 0,
            line: Vec::new(),
        }
    }

    /// `offset` 以降で最初に始まる行の先頭へ移る。
    fn seek_line_start(&mut self, offset: u64) -> io::Result<()> {
        if offset == 0 {
            self.reader.seek(SeekFrom::Start(0))?;
            self.pos = 0;
            return Ok(());
        }
        // offset - 1 から改行までを読み捨てる。offset ちょうどに行頭があれば改行 1 バイトだけ捨てる。
        self.reader.seek(SeekFrom::Start(offset - 1))?;
        self.pos = offset - 1;
        self.line.clear();
        self.pos += self.reader.read_until(b'\n', &mut self.line)? as u64;
        Ok(())
    }

    /// 次の 1 行を読む。行の先頭位置を返し、ファイル末尾なら `None`。
    fn next_line(&mut self) -> io::Result<Option<u64>> {
        let start = self.pos;
        self.line.clear();
        let n = self.reader.read_until(b'\n', &mut self.line)?;
        if n == 0 {
            return Ok(None);
        }
        self.pos += n as u64;
        Ok(Some(start))
    }

    fn line_str(&self) -> io::Result<&str> {
        std::str::from_utf8(&self.line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// 現在位置から最初の `sfen` 行を読み、その先頭位置と手数を除いたキーを返す。
    fn next_sfen_key(&mut self) -> io::Result<Option<(u64, String)>> {
        while let Some(start) = self.next_line()? {
            if let Line::Sfen(sfen) = classify_line(self.line_str()?) {
                return Ok(Some((start, strip_ply(sfen).to_string())));
            }
        }
        Ok(None)
    }

    /// `offset` 以降で最初に始まる `sfen` 行の先頭位置と、手数を除いたキー。
    fn sfen_key_at(&mut self, offset: u64) -> io::Result<Option<(u64, String)>> {
        self.seek_line_start(offset)?;
        self.next_sfen_key()
    }
}

impl OnTheFlyFile {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let this = Self {
            file: Mutex::new(file),
            len,
        };
        this.check_order()?;
        Ok(this)
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// 等間隔の抜き取りで `sfen` 行が昇順に並んでいることを確かめる。全行は読まないので
    /// 局所的な乱れは見逃し得るが、並べていないファイルを渡す誤りはここで弾ける。
    fn check_order(&self) -> io::Result<()> {
        let file = self.lock();
        let mut reader = LineReader::new(&file);
        let mut prev: Option<(u64, String)> = None;
        for i in 0..ORDER_CHECK_SAMPLES {
            let offset =
                (u128::from(self.len) * u128::from(i) / u128::from(ORDER_CHECK_SAMPLES)) as u64;
            let Some((start, key)) = reader.sfen_key_at(offset)? else {
                break;
            };
            // 抜き取り点の直後の行も比べ、隣り合う 2 局面の逆順も拾う。
            let next = reader.next_sfen_key()?;
            for (start, key) in std::iter::once((start, key)).chain(next) {
                if let Some((prev_start, prev_key)) = &prev
                    && *prev_start < start
                    && prev_key.as_str() > key.as_str()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "book file is not sorted by SFEN (required by BookOnTheFly): \
                             sfen line at byte {start} sorts before the one at byte {prev_start}"
                        ),
                    ));
                }
                prev = Some((start, key));
            }
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, File> {
        // ロック中に panic してもファイルハンドル自体は壊れない(読み出し位置は毎回 seek し直す)。
        self.file.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 局面を引く。丸読みの `Book::find_raw` と同じく、指し手が 1 つも無ければ `None`。
    pub(crate) fn find(&self, sfen: &str, ignore_ply: bool) -> io::Result<Option<PositionEntry>> {
        let target = strip_ply(sfen);
        let full_key = normalize_key(sfen, ignore_ply);
        let file = self.lock();
        let mut reader = LineReader::new(&file);

        // 不変条件: 先頭位置が lo 未満の sfen 行はすべて target より小さい。
        // 位置 hi 以降で最初の sfen 行は、無いか target 以上。
        let (mut lo, mut hi) = (0u64, self.len);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match reader.sfen_key_at(mid)? {
                Some((start, key)) if key.as_str() < target => lo = start + 1,
                _ => hi = mid,
            }
        }

        reader.seek_line_start(lo)?;
        let mut entry = PositionEntry::default();
        let mut in_block = false;
        while reader.next_line()?.is_some() {
            match classify_line(reader.line_str()?) {
                Line::Skip => {}
                Line::Sfen(line_sfen) => {
                    if strip_ply(line_sfen) != target {
                        break;
                    }
                    in_block = normalize_key(line_sfen, ignore_ply) == full_key;
                }
                Line::Move(bm) => {
                    if in_block {
                        entry.moves.push(bm);
                    }
                }
            }
        }
        Ok((!entry.moves.is_empty()).then_some(entry))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Book;
    use std::io::Write;

    fn write_temp(name: &str, text: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "rshogi_book_otf_{}_{}_{name}.db",
            std::process::id(),
            std::thread::current().name().unwrap_or("t").replace("::", "_")
        ));
        std::fs::File::create(&path).unwrap().write_all(text.as_bytes()).unwrap();
        path
    }

    const SORTED: &str = "#YANEURAOU-DB2016 1.00\n\
        // comment\n\
        sfen 4k4/9/9/9/9/9/9/9/4K4 b - 1\n\
        5i5h none 1 2 3\n\
        sfen 4k4/9/9/9/9/9/9/9/4K4 b - 10\n\
        5i4h 5a5b 4 5 6\n\
        sfen 4k4/9/9/9/9/9/9/9/4K4 b - 3\n\
        5i6h\n\
        \n\
        sfen 4k4/9/9/9/9/9/9/9/4K4 b P 7\r\n\
        P*5e none 7 8 9\r\n\
        sfen 4k4/9/9/9/9/9/9/9/4K4 w - 2\n\
        sfen 4k4/9/9/9/9/9/9/9/4K4 w - 4\n\
        5a5b\n";

    fn assert_same(text: &str, queries: &[&str]) {
        let path = write_temp("same", text);
        for ignore_ply in [false, true] {
            let memory = Book::from_bytes(text.as_bytes(), ignore_ply).unwrap();
            let otf = Book::open_on_the_fly(&path, ignore_ply).unwrap();
            for q in queries {
                assert_eq!(
                    otf.find_raw(q).unwrap(),
                    memory.find_raw(q).unwrap(),
                    "query {q} ignore_ply={ignore_ply}"
                );
            }
        }
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn matches_memory_book_with_and_without_ply() {
        assert_same(
            SORTED,
            &[
                "4k4/9/9/9/9/9/9/9/4K4 b - 1",
                "4k4/9/9/9/9/9/9/9/4K4 b - 3",
                "4k4/9/9/9/9/9/9/9/4K4 b - 10",
                "4k4/9/9/9/9/9/9/9/4K4 b - 99",
                "4k4/9/9/9/9/9/9/9/4K4 b P 7",
                "4k4/9/9/9/9/9/9/9/4K4 b P 1",
                "4k4/9/9/9/9/9/9/9/4K4 w - 2",
                "4k4/9/9/9/9/9/9/9/4K4 w - 4",
                "4k4/9/9/9/9/9/9/9/4K4 b",
                "0",
                "~",
            ],
        );
    }

    #[test]
    fn finds_every_position_of_a_larger_sorted_book() {
        // 局面ごとの指し手行の数を変え、二分探索がブロックの途中に落ちる場合を作る。
        let mut keys: Vec<String> = (1..=200)
            .map(|i| format!("{}k{}/9/9/9/9/9/9/9/4K4 b - {i}", i % 5, 8 - i % 5))
            .collect();
        keys.sort_by(|a, b| strip_ply(a).cmp(strip_ply(b)).then(a.cmp(b)));
        let mut text = String::from("#YANEURAOU-DB2016 1.00\n");
        for (i, key) in keys.iter().enumerate() {
            text.push_str(&format!("sfen {key}\n"));
            for j in 0..(i % 7) {
                text.push_str(&format!("5i5h none {i} {j} 1\n"));
            }
        }
        let queries: Vec<&str> = keys.iter().map(String::as_str).collect();
        assert_same(&text, &queries);
    }

    #[test]
    fn rejects_unsorted_file() {
        let text = "#YANEURAOU-DB2016 1.00\n\
            sfen 4k4/9/9/9/9/9/9/9/4K4 w - 2\n5a5b\n\
            sfen 4k4/9/9/9/9/9/9/9/4K4 b - 1\n5i5h\n";
        let path = write_temp("unsorted", text);
        let err = Book::open_on_the_fly(&path, true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        std::fs::remove_file(path).ok();
    }

    /// 実際の定跡ファイルで、全局面(手数違い・先後反転を含む)の引いた結果と probe の選択が
    /// 丸読みと一致することを確かめる。
    /// `RSHOGI_BOOK_EQUIV_PATH=<.db> cargo test --release -p rshogi-book -- --ignored`
    #[test]
    #[ignore = "RSHOGI_BOOK_EQUIV_PATH の定跡ファイルが要る"]
    fn real_book_matches_memory_book() {
        use crate::{BookOptions, DefaultBookRng, flipped_key, probe};
        use rshogi_core::position::Position;

        let path = std::env::var("RSHOGI_BOOK_EQUIV_PATH").expect("RSHOGI_BOOK_EQUIV_PATH");
        let sfens: Vec<String> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter_map(|line| match classify_line(line) {
                Line::Sfen(sfen) => Some(sfen.to_string()),
                _ => None,
            })
            .collect();
        let option_sets = [
            BookOptions {
                book_moves: 10000,
                select_value: true,
                flipped_book: false,
                ..Default::default()
            },
            BookOptions {
                book_moves: 10000,
                consider_move_count: true,
                eval_diff: 100,
                ..Default::default()
            },
        ];
        for ignore_ply in [true, false] {
            let memory = Book::from_path(&path, ignore_ply).unwrap();
            let otf = Book::open_on_the_fly(&path, ignore_ply).unwrap();
            let (mut queries, mut hits) = (0usize, 0usize);
            for sfen in &sfens {
                let mut position = Position::new();
                if position.set_sfen(sfen).is_err() {
                    continue;
                }
                let canonical = position.to_sfen();
                let other_ply = format!("{} 999", strip_ply(&canonical));
                let Some(flipped) = flipped_key(&canonical) else {
                    continue;
                };
                for query in [&canonical, &other_ply, &flipped] {
                    let expected = memory.find_raw(query).unwrap();
                    assert_eq!(otf.find_raw(query).unwrap(), expected, "{query}");
                    queries += 1;
                    hits += usize::from(expected.is_some());
                }
                for query in [&canonical, &flipped] {
                    let mut position = Position::new();
                    position.set_sfen(query).unwrap();
                    for (i, options) in option_sets.iter().enumerate() {
                        let seed = queries as u64 ^ i as u64;
                        let expected = probe(
                            &memory,
                            &position,
                            options,
                            &mut DefaultBookRng::from_seed(seed),
                            |_| {},
                        );
                        let actual = probe(
                            &otf,
                            &position,
                            options,
                            &mut DefaultBookRng::from_seed(seed),
                            |_| {},
                        );
                        assert_eq!(actual, expected, "{query} options #{i}");
                    }
                }
            }
            eprintln!(
                "ignore_ply={ignore_ply}: {} sfen lines, {queries} queries, {hits} hits",
                sfens.len()
            );
        }
    }

    #[test]
    fn empty_file_finds_nothing() {
        let path = write_temp("empty", "");
        let book = Book::open_on_the_fly(&path, false).unwrap();
        assert_eq!(book.find_raw("4k4/9/9/9/9/9/9/9/4K4 b - 1").unwrap(), None);
        std::fs::remove_file(path).ok();
    }
}
