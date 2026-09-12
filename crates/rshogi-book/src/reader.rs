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

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Cursor, Read};
use std::path::Path;

/// 定跡 1 手ぶんの生データ(USI 文字列のまま保持し、合法性検証は probe 時に行う)。
#[derive(Debug, Clone)]
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

/// 1 局面ぶんの定跡エントリ。
#[derive(Debug, Clone, Default)]
pub(crate) struct PositionEntry {
    pub(crate) moves: Vec<RawBookMove>,
}

/// メモリ上に丸読みした定跡本体。
///
/// キーは SFEN 文字列。`ignore_ply` が `true` のときは末尾の手数(ply)を落とした
/// SFEN をキーとして格納・検索する(盤面が同じなら手数違いでもヒットする)。
#[derive(Debug, Clone)]
pub struct Book {
    entries: HashMap<String, PositionEntry>,
    ignore_ply: bool,
}

/// `move`/`ponder` フィールドを USI 文字列の `Option` に変換する。
/// `none` / `None` / `resign` は「指し手なし」= `None`。
fn parse_move_field(token: &str) -> Option<String> {
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
    /// ファイルパスから定跡を読み込む(NNUE ローダの `init_nnue` 相当の path 版 API)。
    ///
    /// バッファリングは `from_reader` 側で行う。
    pub fn from_path<P: AsRef<Path>>(path: P, ignore_ply: bool) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        Self::from_reader(file, ignore_ply)
    }

    /// バイト列から定跡を読み込む(NNUE ローダの `init_nnue_from_bytes` 相当。wasm 互換)。
    pub fn from_bytes(bytes: &[u8], ignore_ply: bool) -> io::Result<Self> {
        Self::from_reader(Cursor::new(bytes), ignore_ply)
    }

    /// 任意の `Read` から定跡を読み込む。
    pub fn from_reader<R: Read>(reader: R, ignore_ply: bool) -> io::Result<Self> {
        let mut entries: HashMap<String, PositionEntry> = HashMap::new();
        let mut current_key: Option<String> = None;
        let buf = BufReader::new(reader);

        for line in buf.lines() {
            let line = line?;
            let line = line.trim_end_matches(['\r', '\n']);

            // 空行はスキップ。
            if line.trim().is_empty() {
                continue;
            }
            // `#` 始まり(ヘッダ・バージョン識別・NOE 行)はスキップ。
            if line.starts_with('#') {
                continue;
            }
            // `//` 始まりのコメント行はスキップ。
            if line.starts_with("//") {
                continue;
            }

            if let Some(rest) = line.strip_prefix("sfen ") {
                // 局面区切り行。ignore_ply に応じてキーを正規化して以降の指し手を紐付ける。
                let sfen = rest.trim();
                current_key = Some(normalize_key(sfen, ignore_ply));
                continue;
            }

            // 指し手行。直前に sfen 行が無ければ(不正な並び)スキップ。
            let Some(key) = current_key.as_ref() else {
                continue;
            };

            if let Some(bm) = parse_move_line(line) {
                entries.entry(key.clone()).or_default().moves.push(bm);
            }
        }

        Ok(Self {
            entries,
            ignore_ply,
        })
    }

    /// 登録局面数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 空(登録局面ゼロ)か。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// この定跡が手数無視(IgnoreBookPly)で構築されたか。
    pub fn ignore_ply(&self) -> bool {
        self.ignore_ply
    }

    /// SFEN 文字列で局面エントリを検索する(内部の `ignore_ply` に従いキー正規化)。
    pub(crate) fn find_raw(&self, sfen: &str) -> Option<&PositionEntry> {
        self.entries.get(&normalize_key(sfen, self.ignore_ply))
    }
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
        assert_eq!(book.len(), 2);
        let entry = book
            .find_raw("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("hirate entry present");
        assert_eq!(entry.moves.len(), 2);
        assert_eq!(entry.moves[0].move_usi.as_deref(), Some("7g7f"));
        assert_eq!(entry.moves[1].move_usi.as_deref(), Some("2g2f"));
    }

    #[test]
    fn read_book_ignores_move_line_before_any_sfen() {
        let data = format!("{HEADER}\n7g7f 3c3d 0 0 1\n");
        let book = Book::from_reader(data.as_bytes(), false).unwrap();
        assert!(book.is_empty());
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
        assert_eq!(book.len(), 1);
        let entry = book
            .find_raw("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 5")
            .unwrap();
        assert_eq!(entry.moves.len(), 2);
    }

    #[test]
    fn strip_ply_only_when_numeric_tail() {
        assert_eq!(strip_ply("a b c 42"), "a b c");
        assert_eq!(strip_ply("a b -"), "a b -");
    }
}
