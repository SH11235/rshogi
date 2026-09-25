//! YANEURAOU-DB2016 テキスト定跡 `.db` の共通読み書き。
//!
//! 局面は SFEN の ply を除いた 3 フィールド (盤面・手番・持ち駒) を key として集約する。
//! 先後反転 key (`rshogi_book::flipped_key`) での検索と、指し手の合法性検査付き適用も提供する。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use rshogi_core::position::{Position, SFEN_HIRATE};
use rshogi_core::types::Move;

use crate::common::io::write_atomic;

/// YANEURAOU-DB2016 テキスト定跡のヘッダ行。
pub const BOOK_HEADER: &str = "#YANEURAOU-DB2016 1.00";

/// 定跡の 1 候補手。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookMove {
    /// 指し手 (USI)。`none` / `resign` は `None`。
    pub move_usi: Option<String>,
    /// 予想応手 (USI)。`none` は `None`。
    pub ponder_usi: Option<String>,
    /// 手番側視点の評価値 (cp)。
    pub value: i32,
    /// 探索深さ。
    pub depth: i32,
    /// 採択回数。
    pub count: u64,
}

impl BookMove {
    /// 探索ラベルが付いているか。
    ///
    /// `book_from_csa` はラベル無しの手を `value=0 depth=0` で書き出すため、その組を未設定とみなす。
    pub fn is_labeled(&self) -> bool {
        self.value != 0 || self.depth != 0
    }
}

/// 1 局面分の定跡エントリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionEntry {
    /// ply 付き SFEN。同一 key の行が複数あれば最小 ply の行を代表にする。
    pub sfen: String,
    /// 候補手。
    pub moves: Vec<BookMove>,
}

/// ply 抜き key で集約した定跡。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BookDb {
    /// key (ply 抜き SFEN) → エントリ。
    pub entries: BTreeMap<String, PositionEntry>,
}

/// 定跡内で局面を引いた結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookHit {
    /// ヒットしたエントリの key。
    pub key: String,
    /// 先後反転 key でヒットしたか。`true` のときエントリ内の指し手は反転座標系。
    pub flipped: bool,
}

impl BookDb {
    /// `.db` ファイルを読み込む。
    pub fn read(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("定跡を開けません: {}", path.display()))?;
        Self::from_reader(BufReader::new(file))
            .with_context(|| format!("定跡を読めません: {}", path.display()))
    }

    /// テキストから読み込む。同一 key の行は最小 ply の sfen を代表にして手を連結する。
    pub fn from_reader(reader: impl BufRead) -> Result<Self> {
        let mut entries = BTreeMap::<String, PositionEntry>::new();
        let mut current_key: Option<String> = None;
        for line in reader.lines() {
            let line = line?;
            let line = line.trim_end_matches(['\r', '\n']);
            if line.trim().is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }
            if let Some(rest) = line.strip_prefix("sfen ") {
                let sfen = rest.trim().to_string();
                let key = strip_ply(&sfen).to_string();
                entries
                    .entry(key.clone())
                    .and_modify(|entry| {
                        if ply_of(&sfen) < ply_of(&entry.sfen) {
                            entry.sfen = sfen.clone();
                        }
                    })
                    .or_insert(PositionEntry {
                        sfen,
                        moves: Vec::new(),
                    });
                current_key = Some(key);
                continue;
            }
            let Some(key) = current_key.as_ref() else {
                continue;
            };
            if let Some(book_move) = parse_move_line(line) {
                entries
                    .get_mut(key)
                    .ok_or_else(|| anyhow!("内部エラー: current_key の entry がありません"))?
                    .moves
                    .push(book_move);
            }
        }
        Ok(Self { entries })
    }

    /// 局面を直接 key で引き、無ければ先後反転 key で引く。
    pub fn find(&self, sfen: &str) -> Option<BookHit> {
        let key = strip_ply(sfen);
        if self.entries.contains_key(key) {
            return Some(BookHit {
                key: key.to_string(),
                flipped: false,
            });
        }
        let flipped = rshogi_book::flipped_key(sfen)?;
        let flipped_key = strip_ply(&flipped);
        self.entries.contains_key(flipped_key).then(|| BookHit {
            key: flipped_key.to_string(),
            flipped: true,
        })
    }

    /// `.db` テキストへ直列化する。局面は key 昇順、手は value 降順 → USI 昇順。
    pub fn to_db_string(&self) -> String {
        let mut out = String::new();
        out.push_str(BOOK_HEADER);
        out.push('\n');
        for entry in self.entries.values() {
            // String への書き込みは失敗しない。
            let _ = writeln!(out, "sfen {}", entry.sfen);
            let mut moves: Vec<&BookMove> = entry.moves.iter().collect();
            moves.sort_by(|a, b| {
                b.value.cmp(&a.value).then_with(|| move_sort_key(a).cmp(move_sort_key(b)))
            });
            for book_move in moves {
                let _ = writeln!(
                    out,
                    "{} {} {} {} {}",
                    book_move.move_usi.as_deref().unwrap_or("none"),
                    book_move.ponder_usi.as_deref().unwrap_or("none"),
                    book_move.value,
                    book_move.depth,
                    book_move.count
                );
            }
        }
        out
    }

    /// `.db` ファイルへ atomic に書き出す。
    pub fn write(&self, path: &Path) -> Result<()> {
        write_atomic(path, &self.to_db_string())
            .with_context(|| format!("定跡を書けません: {}", path.display()))
    }
}

fn move_sort_key(book_move: &BookMove) -> &str {
    book_move.move_usi.as_deref().unwrap_or("none")
}

fn parse_move_line(line: &str) -> Option<BookMove> {
    let mut tokens = line.split_whitespace();
    let move_usi = tokens.next().map(parse_move_field)?;
    let ponder_usi = tokens.next().map(parse_move_field).unwrap_or(None);
    let value = tokens.next().map_or(0, |t| t.parse::<i32>().unwrap_or(0));
    let depth = tokens.next().map_or(0, |t| t.parse::<i32>().unwrap_or(0));
    let count = tokens.next().map_or(1, |t| t.parse::<u64>().unwrap_or(0));
    Some(BookMove {
        move_usi,
        ponder_usi,
        value,
        depth,
        count,
    })
}

fn parse_move_field(token: &str) -> Option<String> {
    match token {
        "none" | "None" | "resign" => None,
        other => Some(other.to_string()),
    }
}

/// SFEN 末尾の ply フィールドを除く。
pub fn strip_ply(sfen: &str) -> &str {
    match sfen.rsplit_once(' ') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => sfen,
    }
}

/// SFEN 末尾の ply。無ければ `u32::MAX`。
pub fn ply_of(sfen: &str) -> u32 {
    sfen.rsplit_once(' ')
        .and_then(|(_, tail)| tail.parse::<u32>().ok())
        .unwrap_or(u32::MAX)
}

/// 先後反転を同一視した正準 key (直接 key と反転 key の小さい方)。
pub fn canonical_key(sfen: &str) -> String {
    let key = strip_ply(sfen);
    match rshogi_book::flipped_key(sfen) {
        Some(flipped) if strip_ply(&flipped) < key => strip_ply(&flipped).to_string(),
        _ => key.to_string(),
    }
}

/// 局面に USI 指し手を合法性検査付きで適用する。
pub fn apply_usi_move(pos: &mut Position, move_usi: &str) -> Result<()> {
    let decoded =
        Move::from_usi(move_usi).ok_or_else(|| anyhow!("USI 指し手が不正です: {move_usi}"))?;
    let mv = pos
        .to_move(decoded)
        .ok_or_else(|| anyhow!("局面に適用できない指し手です: {move_usi}"))?;
    if mv == Move::NONE || !pos.pseudo_legal(mv) || !pos.is_legal(mv) {
        bail!("非合法手です: {move_usi}");
    }
    let gives_check = pos.gives_check(mv);
    pos.do_move(mv, gives_check);
    Ok(())
}

/// SFEN から局面を作る。
pub fn position_from_sfen(sfen: &str) -> Result<Position> {
    let mut pos = Position::new();
    pos.set_sfen(sfen).map_err(|e| anyhow!("SFEN が不正です: {sfen}: {e}"))?;
    Ok(pos)
}

/// 親局面に USI 指し手を合法性検査付きで適用した子局面を返す。
pub fn child_position_after_move(parent_sfen: &str, move_usi: &str) -> Result<Position> {
    let mut pos = position_from_sfen(parent_sfen)?;
    apply_usi_move(&mut pos, move_usi).with_context(|| format!("親局面: {parent_sfen}"))?;
    Ok(pos)
}

/// `startpos [moves ...]` / `sfen <sfen> [moves ...]` 形式 (先頭の `position` は省略可) を
/// 指し手適用後の局面 SFEN (ply 付き) にする。
pub fn parse_position_line(line: &str) -> Result<String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let tokens = match tokens.first() {
        Some(&"position") => &tokens[1..],
        _ => &tokens[..],
    };
    let moves_idx = tokens.iter().position(|t| *t == "moves").unwrap_or(tokens.len());
    let (base, moves) = tokens.split_at(moves_idx);
    let moves = moves.get(1..).unwrap_or(&[]);
    let base_sfen = match base {
        ["startpos"] => SFEN_HIRATE.to_string(),
        ["sfen", rest @ ..] if !rest.is_empty() => rest.join(" "),
        _ => bail!("局面行を解釈できません (startpos / sfen で始めてください): {line}"),
    };
    let mut pos = position_from_sfen(&base_sfen)?;
    for move_usi in moves {
        apply_usi_move(&mut pos, move_usi).with_context(|| format!("局面行: {line}"))?;
    }
    Ok(pos.to_sfen())
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

    #[test]
    fn read_merges_duplicate_keys_with_min_ply_and_roundtrips() {
        let text = format!(
            "{BOOK_HEADER}\nsfen {} 5\n7g7f none 10 3 2\nsfen {START}\n2g2f 8c8d 20 4 1\n",
            strip_ply(START)
        );
        let book = BookDb::from_reader(text.as_bytes()).unwrap();
        assert_eq!(book.entries.len(), 1);
        let entry = &book.entries[strip_ply(START)];
        assert_eq!(entry.sfen, START);
        assert_eq!(entry.moves.len(), 2);
        let out = book.to_db_string();
        // value 降順で並ぶ。
        assert_eq!(
            out,
            format!("{BOOK_HEADER}\nsfen {START}\n2g2f 8c8d 20 4 1\n7g7f none 10 3 2\n")
        );
        rshogi_book::Book::from_bytes(out.as_bytes(), true).unwrap();
    }

    #[test]
    fn find_uses_flipped_key_when_direct_key_misses() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let flipped = rshogi_book::flipped_key(&child).unwrap();
        let text = format!("{BOOK_HEADER}\nsfen {flipped}\n7g7f none 0 0 1\n");
        let book = BookDb::from_reader(text.as_bytes()).unwrap();
        let hit = book.find(&child).unwrap();
        assert!(hit.flipped);
        assert_eq!(hit.key, strip_ply(&flipped));
        assert_eq!(canonical_key(&child), canonical_key(&flipped));
    }

    #[test]
    fn unlabeled_moves_are_value_zero_depth_zero() {
        let line = parse_move_line("7g7f none 0 0 3").unwrap();
        assert!(!line.is_labeled());
        let line = parse_move_line("7g7f none 0 12 3").unwrap();
        assert!(line.is_labeled());
    }

    #[test]
    fn parse_position_line_accepts_startpos_and_sfen_with_moves() {
        let a = parse_position_line("startpos moves 7g7f 3c3d").unwrap();
        let b = parse_position_line(&format!("position sfen {START} moves 7g7f 3c3d")).unwrap();
        assert_eq!(a, b);
        assert!(a.ends_with(" 3"));
        assert_eq!(parse_position_line("startpos").unwrap(), START);
        assert!(parse_position_line("startpos moves 7g7e").is_err());
        assert!(parse_position_line("foo").is_err());
    }
}
