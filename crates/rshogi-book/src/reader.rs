//! YANEURAOU-DB2016 テキスト `.db` 定跡ファイルのリーダ。
//!
//! フォーマット(寛容パース仕様):
//! - 1 行目ヘッダ `#YANEURAOU-DB2016 1.00`(`#` 行は無条件スキップ)
//! - `//` 始まりの行はコメントとしてスキップ
//! - `sfen <SFEN(ply 込み)>` 行が局面区切り。以降の指し手行はその局面に属する
//! - 指し手行 `move ponder value depth move_count`(空白区切り)。必須は `move`/`ponder`
//!   のみで、`value`/`depth`/`move_count` は省略可。省略時の既定は
//!   `value=0, depth=0, move_count=1`。数値化できないフィールドは 0 とみなす
//!   (`move_count` は「省略=1」だが「不正=0」)
//! - `move`/`ponder` が `none` / `None` / `resign` のいずれかなら「指し手なし」
//!
//! 先頭が `.ybb` の magic なら YaneuraOu のバイナリ定跡として読む ([`crate::ybb`])。

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use crate::on_the_fly::OnTheFlyFile;
use crate::ybb::{self, YbbBook};

/// 定跡 1 手ぶんの生データ(USI 文字列のまま保持し、合法性検証は probe 時に行う)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawBookMove {
    /// 指し手(USI 文字列)。`none`/`resign` 等の「指し手なし」は `None`。
    pub(crate) move_usi: Option<String>,
    /// 相手の予想手(USI 文字列)。「指し手なし」は `None`。
    pub(crate) ponder_usi: Option<String>,
    /// 評価値。
    pub(crate) value: i32,
    /// 探索深さ。
    pub(crate) depth: i32,
    /// 採択回数。
    pub(crate) move_count: u64,
}

/// 定跡に書かれた 1 手 ([`Book::entry_moves`] の結果)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookEntryMove {
    /// 指し手 (USI 文字列。合法性は検証していない)。
    pub move_usi: String,
    /// 手番の側から見た評価値。
    pub value: i32,
    /// 探索深さ。
    pub depth: i32,
    /// 採択回数 (テキスト `.db` の 5 列目、省略時 1)。`.ybb` は持たないので 0。
    pub move_count: u64,
}

/// 1 局面ぶんの定跡エントリ。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PositionEntry {
    pub(crate) moves: Vec<RawBookMove>,
}

/// 定跡本体。メモリに丸読みしたもの([`Book::from_path`] など)と、ファイルを開いたまま
/// probe のたびに引くもの([`Book::open_on_the_fly`])の 2 通りがあり、引いた結果は同じになる。
/// テキスト `.db` と YaneuraOu のバイナリ定跡 `.ybb` を読める (ファイル先頭の magic で見分ける)。
///
/// キーは SFEN 文字列。`ignore_ply` が `true` のときは末尾の手数(ply)を落とした
/// SFEN をキーとして格納・検索する(盤面が同じなら手数違いでもヒットする)。
#[derive(Debug)]
pub struct Book {
    store: Store,
    ignore_ply: bool,
}

#[derive(Debug)]
enum Store {
    Memory(HashMap<String, PositionEntry>),
    OnTheFly(OnTheFlyFile),
    Ybb(YbbBook),
}

/// `.db` の 1 行の種類。丸読みと on the fly で同じ解釈をするために共通化している。
pub(crate) enum Line<'a> {
    /// 空行・`#` 行・`//` コメント行。
    Skip,
    /// `sfen ` 行。中身は前後の空白を除いた SFEN。
    Sfen(&'a str),
    /// 指し手行。
    Move(RawBookMove),
}

/// 1 行を解釈する。行末の改行(`\r` / `\n`)は除いて渡しても付けたままでもよい。
pub(crate) fn classify_line(line: &str) -> Line<'_> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() || line.starts_with('#') || line.starts_with("//") {
        return Line::Skip;
    }
    if let Some(rest) = line.strip_prefix("sfen ") {
        return Line::Sfen(rest.trim());
    }
    match parse_move_line(line) {
        Some(bm) => Line::Move(bm),
        None => Line::Skip,
    }
}

/// `move`/`ponder` フィールドを USI 文字列の `Option` に変換する。
/// `none` / `None` / `resign` は「指し手なし」= `None`。
pub(crate) fn parse_move_field(token: &str) -> Option<String> {
    match token {
        "none" | "None" | "resign" => None,
        other => Some(other.to_string()),
    }
}

/// SFEN 文字列から末尾の手数(ply)を落とす。末尾トークンが数値でない場合はそのまま返す。
pub(crate) fn strip_ply(sfen: &str) -> &str {
    match sfen.rsplit_once(' ') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => sfen,
    }
}

/// `ignore_ply` 設定に応じて検索キーを正規化する。
pub(crate) fn normalize_key(sfen: &str, ignore_ply: bool) -> String {
    if ignore_ply {
        strip_ply(sfen).to_string()
    } else {
        sfen.to_string()
    }
}

impl Book {
    /// ファイルパスから定跡を丸読みする(NNUE ローダの `init_nnue` 相当の path 版 API)。
    ///
    /// バッファリングは `from_reader` 側で行う。
    pub fn from_path<P: AsRef<Path>>(path: P, ignore_ply: bool) -> io::Result<Self> {
        let path = path.as_ref();
        if is_ybb_file(path)? {
            return Ok(Self {
                store: Store::Ybb(YbbBook::read_file(path)?),
                ignore_ply,
            });
        }
        Self::from_text_reader(std::fs::File::open(path)?, ignore_ply)
    }

    /// バイト列から定跡を読み込む(NNUE ローダの `init_nnue_from_bytes` 相当。wasm 互換)。
    pub fn from_bytes(bytes: &[u8], ignore_ply: bool) -> io::Result<Self> {
        Self::from_reader(bytes, ignore_ply)
    }

    /// 任意の `Read` から定跡を読み込む。`.ybb` はファイル全体をそのまま保持する。
    pub fn from_reader<R: Read>(mut reader: R, ignore_ply: bool) -> io::Result<Self> {
        let mut head = Vec::with_capacity(ybb::MAGIC.len());
        (&mut reader).take(ybb::MAGIC.len() as u64).read_to_end(&mut head)?;
        if head == ybb::MAGIC {
            return Ok(Self {
                store: Store::Ybb(YbbBook::from_reader(head, reader)?),
                ignore_ply,
            });
        }
        Self::from_text_reader(head.as_slice().chain(reader), ignore_ply)
    }

    fn from_text_reader<R: Read>(reader: R, ignore_ply: bool) -> io::Result<Self> {
        let mut entries: HashMap<String, PositionEntry> = HashMap::new();
        let mut current_key: Option<String> = None;
        let buf = BufReader::new(reader);

        for line in buf.lines() {
            let line = line?;
            match classify_line(&line) {
                Line::Skip => {}
                Line::Sfen(sfen) => {
                    // 局面区切り行。ignore_ply に応じてキーを正規化して以降の指し手を紐付ける。
                    current_key = Some(normalize_key(sfen, ignore_ply));
                }
                Line::Move(bm) => {
                    // 直前に sfen 行が無い指し手行(不正な並び)はスキップ。
                    if let Some(key) = current_key.as_ref() {
                        entries.entry(key.clone()).or_default().moves.push(bm);
                    }
                }
            }
        }

        Ok(Self {
            store: Store::Memory(entries),
            ignore_ply,
        })
    }

    /// 定跡ファイルを開くだけで丸読みせず、probe のたびにファイルから引く(BookOnTheFly)。
    ///
    /// テキスト `.db` は `sfen` 行が手数を除いた SFEN のバイト順に並んでいる必要がある
    /// (YaneuraOu の定跡書き出しの順)。開くときに等間隔の抜き取りで並びを確かめ、
    /// 逆順の箇所を見つけたら `InvalidData` を返す。`.ybb` は header だけを確かめ、
    /// index をファイル上で二分探索する。ファイルは開いたまま保持する。
    pub fn open_on_the_fly<P: AsRef<Path>>(path: P, ignore_ply: bool) -> io::Result<Self> {
        let path = path.as_ref();
        let store = if is_ybb_file(path)? {
            Store::Ybb(YbbBook::open(path)?)
        } else {
            Store::OnTheFly(OnTheFlyFile::open(path)?)
        };
        Ok(Self { store, ignore_ply })
    }

    /// 定跡の局面数。テキスト `.db` の丸読みは `IgnoreBookPly` で手数違いをまとめた後の数、
    /// `.ybb` は header の局面数。テキスト `.db` の on the fly では数えないので `None`。
    pub fn position_count(&self) -> Option<usize> {
        match &self.store {
            Store::Memory(entries) => Some(entries.len()),
            Store::OnTheFly(_) => None,
            Store::Ybb(book) => usize::try_from(book.record_count()).ok(),
        }
    }

    /// on the fly で引く定跡なら、開いているファイルのバイト数。
    pub fn on_the_fly_file_len(&self) -> Option<u64> {
        match &self.store {
            Store::Memory(_) => None,
            Store::OnTheFly(file) => Some(file.len()),
            Store::Ybb(book) => book.file_len(),
        }
    }

    /// この定跡が手数無視(IgnoreBookPly)で構築されたか。
    pub fn ignore_ply(&self) -> bool {
        self.ignore_ply
    }

    /// 局面の定跡手を定跡ファイルに書かれた順に返す。`none` などの指し手なしの行は除く。
    /// 局面が無ければ空。on the fly の読み出しに失敗したときだけ `Err` になる。
    pub fn entry_moves(&self, sfen: &str) -> io::Result<Vec<BookEntryMove>> {
        let Some(entry) = self.find_raw(sfen)? else {
            return Ok(Vec::new());
        };
        Ok(entry
            .moves
            .iter()
            .filter_map(|m| {
                Some(BookEntryMove {
                    move_usi: m.move_usi.clone()?,
                    value: m.value,
                    depth: m.depth,
                    move_count: m.move_count,
                })
            })
            .collect())
    }

    /// SFEN 文字列で局面エントリを検索する(内部の `ignore_ply` に従いキー正規化)。
    /// on the fly の読み出しに失敗したときだけ `Err` になる。
    pub(crate) fn find_raw(&self, sfen: &str) -> io::Result<Option<Cow<'_, PositionEntry>>> {
        match &self.store {
            Store::Memory(entries) => {
                Ok(entries.get(&normalize_key(sfen, self.ignore_ply)).map(Cow::Borrowed))
            }
            Store::OnTheFly(file) => Ok(file.find(sfen, self.ignore_ply)?.map(Cow::Owned)),
            Store::Ybb(book) => Ok(book.find(sfen, self.ignore_ply)?.map(Cow::Owned)),
        }
    }
}

/// 定跡ファイルのパスを解決する。`path` が無く、`.db` で終わり、拡張子を `.ybb` にした
/// ファイルがあればそちらを返す。それ以外は `path` のまま (YaneuraOu の `BookFile` と同じ)。
pub fn resolve_book_path(path: &Path) -> PathBuf {
    if !path.exists() && path.extension().is_some_and(|ext| ext == "db") {
        let ybb = path.with_extension("ybb");
        if ybb.exists() {
            return ybb;
        }
    }
    path.to_path_buf()
}

/// 優先定跡を含めて、引く順に定跡ファイルのパスを返す。末尾は `path` を
/// [`resolve_book_path`] で解決したもの。
///
/// `path` が `user_book1.db` なら、同じディレクトリの `user_book1-000.db`、`user_book1-001.db`、…
/// を番号順に並べ、最初に欠けた番号で止める (番号は 3 桁未満なら 0 で埋める。1000 以降は 4 桁)。
/// 各番号は `path` と同じ拡張子を先に探し、無ければもう一方 (`.db` と `.ybb`) を探す。両方あれば
/// `path` と同じ拡張子の方を使い、`info` で知らせる。`path` が `.db` / `.ybb` で終わらなければ
/// 優先定跡は探さない。YaneuraOu の優先定跡と同じ規則。
pub fn priority_book_paths(path: &Path, mut info: impl FnMut(&str)) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    let extensions = (file_name.strip_suffix(".ybb").map(|stem| (stem, "ybb", "db")))
        .or_else(|| file_name.strip_suffix(".db").map(|stem| (stem, "db", "ybb")))
        .filter(|(stem, _, _)| !stem.is_empty());
    if let Some((stem, primary, secondary)) = extensions {
        for index in 0.. {
            let numbered = |ext: &str| path.with_file_name(format!("{stem}-{index:03}.{ext}"));
            let (primary, secondary) = (numbered(primary), numbered(secondary));
            if primary.exists() {
                if secondary.exists() {
                    info(&format!("priority book file exists twice. use : {}", primary.display()));
                }
                paths.push(primary);
            } else if secondary.exists() {
                paths.push(secondary);
            } else {
                break;
            }
        }
    }
    paths.push(resolve_book_path(path));
    paths
}

/// ファイル先頭が `.ybb` の magic か。拡張子が `.ybb` なのに magic が違うファイルは、
/// テキストとして読むと 0 局面の定跡として開けてしまうので `InvalidData` にする。
fn is_ybb_file(path: &Path) -> io::Result<bool> {
    let mut head = Vec::with_capacity(ybb::MAGIC.len());
    std::fs::File::open(path)?
        .take(ybb::MAGIC.len() as u64)
        .read_to_end(&mut head)?;
    if head == ybb::MAGIC {
        return Ok(true);
    }
    if path.extension().is_some_and(|ext| ext == "ybb") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a ybb file (magic mismatch)", path.display()),
        ));
    }
    Ok(false)
}

/// 指し手行 1 行を `RawBookMove` にパースする。`move` フィールドが空なら `None`。
fn parse_move_line(line: &str) -> Option<RawBookMove> {
    let mut tokens = line.split_whitespace();
    let move_token = tokens.next()?; // move が無い行は無視
    let move_usi = parse_move_field(move_token);
    // ponder は省略時 "none" 相当(= None)。
    let ponder_usi = tokens.next().and_then(parse_move_field);
    // value/depth: 省略時 0、数値化不能も 0。
    let value = tokens.next().map_or(0, |t| t.parse::<i32>().unwrap_or(0));
    let depth = tokens.next().map_or(0, |t| t.parse::<i32>().unwrap_or(0));
    // move_count: 省略時 1、数値化不能は 0。
    let move_count = tokens.next().map_or(1, |t| t.parse::<u64>().unwrap_or(0));

    Some(RawBookMove {
        move_usi,
        ponder_usi,
        value,
        depth,
        move_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "#YANEURAOU-DB2016 1.00";

    #[test]
    fn parse_full_fields() {
        let bm = parse_move_line("7g7f 3c3d 42 16 123").unwrap();
        assert_eq!(bm.move_usi.as_deref(), Some("7g7f"));
        assert_eq!(bm.ponder_usi.as_deref(), Some("3c3d"));
        assert_eq!(bm.value, 42);
        assert_eq!(bm.depth, 16);
        assert_eq!(bm.move_count, 123);
    }

    #[test]
    fn parse_omitted_fields_uses_defaults() {
        let bm = parse_move_line("7g7f").unwrap();
        assert_eq!(bm.move_usi.as_deref(), Some("7g7f"));
        assert!(bm.ponder_usi.is_none());
        // value/depth/move_count 省略 → 0/0/1。
        let bm = parse_move_line("7g7f 3c3d").unwrap();
        assert_eq!(bm.value, 0);
        assert_eq!(bm.depth, 0);
        assert_eq!(bm.move_count, 1);
        // value のみ指定 → depth=0, move_count=1。
        let bm = parse_move_line("7g7f 3c3d 55").unwrap();
        assert_eq!(bm.value, 55);
        assert_eq!(bm.depth, 0);
        assert_eq!(bm.move_count, 1);
    }

    #[test]
    fn parse_non_numeric_fields_become_zero() {
        // 数値化できないフィールドは 0(move_count も存在するので 1 でなく 0)。
        let bm = parse_move_line("7g7f 3c3d abc def ghi").unwrap();
        assert_eq!(bm.value, 0);
        assert_eq!(bm.depth, 0);
        assert_eq!(bm.move_count, 0);
    }

    #[test]
    fn parse_none_variants_are_no_move() {
        for token in ["none", "None", "resign"] {
            let line = format!("{token} {token} 0 0 1");
            let bm = parse_move_line(&line).unwrap();
            assert!(bm.move_usi.is_none());
            assert!(bm.ponder_usi.is_none());
        }
    }

    #[test]
    fn read_book_skips_comments_and_headers() {
        let data = format!(
            "{HEADER}\n\
             // これはコメント\n\
             sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\n\
             7g7f 3c3d 30 16 100\n\
             2g2f 8c8d 25 16 40\n\
             \n\
             sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2\n\
             3c3d none 20 16 10\n"
        );
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        assert_eq!(book.position_count(), Some(2));
        let entry = book
            .find_raw("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap()
            .expect("hirate entry present");
        assert_eq!(entry.moves.len(), 2);
        assert_eq!(entry.moves[0].move_usi.as_deref(), Some("7g7f"));
        assert_eq!(entry.moves[1].move_usi.as_deref(), Some("2g2f"));
    }

    #[test]
    fn entry_moves_keep_file_order_and_drop_no_move_rows() {
        let data = format!(
            "{HEADER}\n\
             sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\n\
             2g2f 8c8d 25 16 40\n\
             none none 99 0 1\n\
             7g7f none 30\n"
        );
        let book = Book::from_reader(data.as_bytes(), true).unwrap();
        let moves = book
            .entry_moves("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 9")
            .unwrap();
        let got: Vec<_> = moves
            .iter()
            .map(|m| (m.move_usi.as_str(), m.value, m.depth, m.move_count))
            .collect();
        assert_eq!(got, [("2g2f", 25, 16, 40), ("7g7f", 30, 0, 1)]);
        assert!(book.entry_moves("4k4/9/9/9/9/9/9/9/4K4 b - 1").unwrap().is_empty());
    }

    #[test]
    fn read_book_ignores_move_line_before_any_sfen() {
        let data = format!("{HEADER}\n7g7f 3c3d 0 0 1\n");
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        assert_eq!(book.position_count(), Some(0));
    }

    #[test]
    fn ignore_ply_merges_duplicate_boards() {
        // 同一盤面・手数違いの 2 エントリが 1 キーにマージされる。
        let data = format!(
            "{HEADER}\n\
             sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\n\
             7g7f 3c3d 30 16 100\n\
             sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 3\n\
             2g2f 8c8d 25 16 40\n"
        );
        let book = Book::from_bytes(data.as_bytes(), true).unwrap();
        assert_eq!(book.position_count(), Some(1));
        let entry = book
            .find_raw("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 5")
            .unwrap()
            .unwrap();
        assert_eq!(entry.moves.len(), 2);
    }

    #[test]
    fn strip_ply_only_when_numeric_tail() {
        assert_eq!(strip_ply("a b c 42"), "a b c");
        assert_eq!(strip_ply("a b -"), "a b -");
    }

    /// 並行に流れるテストやプロセスと衝突しない空のディレクトリを作る。
    fn unique_temp_dir(tag: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("rshogi_book_priority_{tag}_{}_{n}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn priority_book_paths_follow_numbers_until_a_gap() {
        let dir = unique_temp_dir("paths");
        for name in [
            "user.db",
            "user-000.db",
            "user-001.ybb",
            "user-002.db",
            "user-002.ybb",
            "user-004.db",
        ] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        let mut infos = Vec::new();
        let paths = priority_book_paths(&dir.join("user.db"), |m| infos.push(m.to_string()));
        assert_eq!(file_names(&paths), ["user-000.db", "user-001.ybb", "user-002.db", "user.db"]);
        assert_eq!(
            infos,
            [format!(
                "priority book file exists twice. use : {}",
                dir.join("user-002.db").display()
            )]
        );
        // BookFile が .ybb なら同じ番号の .ybb を優先する。BookFile 自体は存在しなくてよい。
        let paths = priority_book_paths(&dir.join("user.ybb"), |_| {});
        assert_eq!(file_names(&paths), ["user-000.db", "user-001.ybb", "user-002.ybb", "user.ybb"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn priority_book_paths_without_priority_books() {
        let dir = unique_temp_dir("none");
        for name in ["book.ybb", "book.bin", "book.bin-000.db", "other-000.db"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        // 優先定跡が無ければ BookFile の解決 (.db → .ybb) だけ。
        assert_eq!(file_names(&priority_book_paths(&dir.join("book.db"), |_| {})), ["book.ybb"]);
        // .db / .ybb 以外の BookFile では優先定跡を探さない。
        assert_eq!(file_names(&priority_book_paths(&dir.join("book.bin"), |_| {})), ["book.bin"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn priority_book_paths_use_four_digits_from_1000() {
        let dir = unique_temp_dir("digits");
        for index in 0..=1000 {
            std::fs::write(dir.join(format!("b-{index:03}.db")), "").unwrap();
        }
        let paths = priority_book_paths(&dir.join("b.db"), |_| {});
        assert_eq!(paths.len(), 1002);
        assert_eq!(file_names(&paths[999..]), ["b-999.db", "b-1000.db", "b.db"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 優先定跡のファイル群を、丸読みと on the fly の両方、`.db` / `.ybb` 混在で引く。
    #[test]
    fn priority_books_from_files_in_memory_and_on_the_fly() {
        use crate::{BookOptions, DefaultBookRng, probe};
        use rshogi_core::position::Position;

        let hirate = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let after_26 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/7P1/PPPPPPP1P/1B5R1/LNSGKGSNL w - 2";
        let after_26_ply10 = "lnsgkgsnl/1r5b1/ppppppppp/9/9/7P1/PPPPPPP1P/1B5R1/LNSGKGSNL w - 10";
        let dir = unique_temp_dir("files");
        std::fs::write(dir.join("user-000.ybb"), include_bytes!("../testdata/small.ybb")).unwrap();
        std::fs::write(
            dir.join("user-001.db"),
            format!("{HEADER}\nsfen {after_26}\n8c8d none 0 0 1\n"),
        )
        .unwrap();
        // on the fly の .db は手数を除いた SFEN 順に並べる。
        std::fs::write(
            dir.join("user.db"),
            format!(
                "{HEADER}\nsfen {after_26_ply10}\n1c1d none 0 0 1\nsfen {hirate}\n1g1f none 0 0 1\n"
            ),
        )
        .unwrap();
        let paths = priority_book_paths(&dir.join("user.db"), |_| {});
        assert_eq!(file_names(&paths), ["user-000.ybb", "user-001.db", "user.db"]);

        for on_the_fly in [false, true] {
            for ignore_ply in [false, true] {
                let books: Vec<Book> = paths
                    .iter()
                    .map(|path| {
                        if on_the_fly {
                            Book::open_on_the_fly(path, ignore_ply)
                        } else {
                            Book::from_path(path, ignore_ply)
                        }
                        .unwrap()
                    })
                    .collect();
                let best = |sfen: &str| {
                    let mut position = Position::new();
                    position.set_sfen(sfen).unwrap();
                    let options = BookOptions::default();
                    probe(&books, &position, &options, &mut DefaultBookRng::from_seed(0), |_| {})
                        .map(|r| r.best_move.to_usi())
                        .unwrap()
                };
                let case = format!("on_the_fly={on_the_fly} ignore_ply={ignore_ply}");
                assert!(["7g7f", "2g2f"].contains(&best(hirate).as_str()), "{case}");
                assert_eq!(best(after_26), "8c8d", "{case}");
                let expected = if ignore_ply { "8c8d" } else { "1c1d" };
                assert_eq!(best(after_26_ply10), expected, "{case}");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
