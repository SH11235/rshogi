//! 定跡の局面・指し手と、YANEURAOU-DB2016 テキスト形式の解釈。
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

/// 1 局面ぶんの定跡エントリ。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PositionEntry {
    pub(crate) moves: Vec<RawBookMove>,
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
    fn strip_ply_only_when_numeric_tail() {
        assert_eq!(strip_ply("a b c 42"), "a b c");
        assert_eq!(strip_ply("a b -"), "a b -");
    }
}
