//! 電竜戦の公式 KIF 棋譜を、棋譜サーバーへ送る CSA に変換する。
//!
//! 電竜戦の KIF は移動元を括弧で書く (`７六歩(77)`) ので、盤面を追わずに 1 手ずつ
//! 変換できる。合法性はここでは確かめない (サーバーが取り込み時に全手を再生して確かめる)。
//!
//! # 出力
//!
//! ```text
//! V2.2
//! N+<先手>
//! N-<後手>
//! $EVENT:<棋戦>
//! $START_TIME:<開始日時>
//! $TIME_LIMIT:00:10+00        … `*時間設定：10分-1手2秒加算` のときだけ
//! 'Increment:2
//! '先手持ち時間420秒削減       … `*先手持ち時間…削減` / `*指定局面名：…` はコメントで残す
//! PI
//! +
//! +5958OU,T3
//! '** 122 -5251OU +4859OU    … `**対局 評価値 122 読み筋 △５一玉(52) ▲５九玉(48)`
//! %TORYO
//! 'まで84手で後手の勝ち        … 元の `まで` 行
//! ```
//!
//! 評価値は KIF に書かれた値のまま (先手から見た値)。読み筋は変換できない手が出たら
//! そこで切る。

use std::sync::LazyLock;

use regex::Regex;

use super::Rejection;
use super::csa_normalize::{REASON_NO_START_TIME, is_csa_datetime};

/// [`Rejection::Excluded`] の理由: 指し手が 1 つも無い。
pub const REASON_NO_MOVES: &str = "指し手なし";
/// [`Rejection::Excluded`] の理由: 平手でない。
pub const REASON_HANDICAP: &str = "平手以外";

const REASON_BAD_HEADER: &str = "対局者名が無い";
const REASON_BAD_MOVE: &str = "指し手を変換できない";
const REASON_MOVE_NUMBER: &str = "手数が連続していない";
const REASON_UNKNOWN_END: &str = "未対応の終局表記";
const REASON_NO_WINNER: &str = "反則負けの勝者が まで 行から分からない";
const REASON_RESULT_MISMATCH: &str = "勝敗が まで 行と食い違う";

/// 指し手行: `<手数> <指し手> ( m:ss/累計)`。累計は空のことがある (`( 0:00/)`)。
static MOVE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*([0-9]+)\s+(.*?)\s*(?:\(\s*([0-9]+):([0-9]+)\s*/[^)]*\))?\s*$")
        .expect("固定の正規表現")
});
static TIME_SETTING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\*時間設定：([0-9]+)分-1手([0-9]+)秒加算$").expect("固定の正規表現")
});
static TIME_CUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\*[先後]手持ち時間[0-9]+秒削減$").expect("固定の正規表現"));
static EVAL_COMMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\*\*対局\s+評価値\s+(\S+)(?:\s+読み筋(.*))?$").expect("固定の正規表現")
});

/// 駒の表記と CSA の駒種。2 文字の表記 (`成銀` など) を先に試す。
const PIECES: [(&str, &str); 19] = [
    ("成銀", "NG"),
    ("成桂", "NK"),
    ("成香", "NY"),
    ("歩", "FU"),
    ("香", "KY"),
    ("桂", "KE"),
    ("銀", "GI"),
    ("金", "KI"),
    ("角", "KA"),
    ("飛", "HI"),
    ("玉", "OU"),
    ("王", "OU"),
    ("と", "TO"),
    ("杏", "NY"),
    ("圭", "NK"),
    ("全", "NG"),
    ("馬", "UM"),
    ("龍", "RY"),
    ("竜", "RY"),
];

fn promoted(code: &str) -> Option<&'static str> {
    Some(match code {
        "FU" => "TO",
        "KY" => "NY",
        "KE" => "NK",
        "GI" => "NG",
        "KA" => "UM",
        "HI" => "RY",
        _ => return None,
    })
}

fn zenkaku_digit(c: char) -> Option<u8> {
    ('１'..='９').contains(&c).then(|| (u32::from(c) - u32::from('１') + 1) as u8)
}

fn kanji_digit(c: char) -> Option<u8> {
    "一二三四五六七八九".chars().position(|k| k == c).map(|index| index as u8 + 1)
}

/// 変換した 1 手。`dest` は次の `同` のために持つ。
struct KifMove {
    /// `<移動元 2 桁><移動先 2 桁><移動後の駒種>` (符号なし)。
    body: String,
    dest: (u8, u8),
}

/// KIF の指し手 1 つ (`７六歩(77)` / `同　銀(88)` / `５五角打` / `２二角成(88)`) を変換する。
/// `previous_dest` は `同` が指す升。
fn parse_kif_move(text: &str, previous_dest: Option<(u8, u8)>) -> Option<KifMove> {
    let text = text.trim_matches(|c: char| c.is_whitespace());
    let mut chars = text.chars();
    let first = chars.next()?;
    let (dest, rest) = if first == '同' {
        (previous_dest?, chars.as_str().trim_start_matches(|c: char| c.is_whitespace()))
    } else {
        let file = zenkaku_digit(first)?;
        let rank = kanji_digit(chars.next()?)?;
        ((file, rank), chars.as_str())
    };
    let (rest, code) = PIECES
        .iter()
        .find_map(|(name, code)| rest.strip_prefix(name).map(|rest| (rest, *code)))?;
    let (rest, code) = if let Some(rest) = rest.strip_prefix("不成") {
        (rest, code)
    } else if let Some(rest) = rest.strip_prefix('成') {
        (rest, promoted(code)?)
    } else {
        (rest, code)
    };
    let from = if rest == "打" {
        (0, 0)
    } else {
        let digits = rest.strip_prefix('(')?.strip_suffix(')')?;
        let mut digits = digits.chars();
        let (file, rank) = (digits.next()?.to_digit(10)?, digits.next()?.to_digit(10)?);
        if digits.next().is_some() || file == 0 || rank == 0 {
            return None;
        }
        (file as u8, rank as u8)
    };
    Some(KifMove {
        body: format!("{}{}{}{}{code}", from.0, from.1, dest.0, dest.1),
        dest,
    })
}

/// 読み筋 (`△５一玉(52) ▲５九玉(48) ...`) を CSA の手の並びにする。変換できない手が
/// 出たらそこで切る。先頭の `同` は `played_dest` (実際に指された手の移動先) を指す。
fn convert_pv(pv: &str, played_dest: (u8, u8)) -> Vec<String> {
    let mut moves = Vec::new();
    let mut previous_dest = played_dest;
    let Some(start) = pv.find(['▲', '△']) else {
        return moves;
    };
    if !pv[..start].trim().is_empty() {
        return moves;
    }
    let mut rest = &pv[start..];
    while let Some(mark) = rest.chars().next() {
        let after_mark = &rest[mark.len_utf8()..];
        let end = after_mark.find(['▲', '△']).unwrap_or(after_mark.len());
        let Some(mv) = parse_kif_move(&after_mark[..end], Some(previous_dest)) else {
            break;
        };
        let sign = if mark == '▲' { '+' } else { '-' };
        moves.push(format!("{sign}{}", mv.body));
        previous_dest = mv.dest;
        rest = &after_mark[end..];
    }
    moves
}

/// `まで` 行から読める勝敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    BlackWins,
    WhiteWins,
    Draw,
}

fn outcome_from_made_line(line: &str) -> Option<Outcome> {
    if line.contains("先手の勝ち") {
        Some(Outcome::BlackWins)
    } else if line.contains("後手の勝ち") {
        Some(Outcome::WhiteWins)
    } else if line.contains("引分け") || line.ends_with("千日手") {
        Some(Outcome::Draw)
    } else {
        // `まで318手で入玉宣言` のように勝者を書かない行。
        None
    }
}

/// `%` コマンドと終局時点の手番から決まる勝敗。サーバー
/// (`rshogi-csa-server-workers` の `kifu_import::result_from_special`) と同じ規則。
fn outcome_from_special(special: &str, black_to_move: bool) -> Option<Outcome> {
    let win = |black: bool| {
        if black {
            Outcome::BlackWins
        } else {
            Outcome::WhiteWins
        }
    };
    match special {
        "TORYO" | "TIME_UP" => Some(win(!black_to_move)),
        "KACHI" => Some(win(black_to_move)),
        "+ILLEGAL_ACTION" => Some(Outcome::WhiteWins),
        "-ILLEGAL_ACTION" => Some(Outcome::BlackWins),
        "JISHOGI" | "SENNICHITE" | "MAX_MOVES" => Some(Outcome::Draw),
        _ => None,
    }
}

fn failed(reason: &'static str, detail: impl Into<String>) -> Rejection {
    Rejection::Failed {
        reason,
        detail: detail.into(),
    }
}

/// KIF 1 局分を CSA に変換する。
///
/// - 指し手が 1 つも無い棋譜、平手でない棋譜、開始日時が無い棋譜は
///   [`Rejection::Excluded`]。
/// - 変換できない指し手・未対応の終局表記、`まで` 行の勝敗と変換結果の食い違いは
///   [`Rejection::Failed`]。
pub fn kif_to_csa(text: &str) -> Result<String, Rejection> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut event = None;
    let mut start_time = None;
    let mut black = None;
    let mut white = None;
    let mut handicap = None;
    let mut clock = None;
    let mut end_reason = "";
    let mut kept_comments: Vec<&str> = Vec::new();
    // `まで` 行は指し手より後ろにあるが、反則負けの勝者を決めるのに先に要る。
    let made_line = text.lines().map(str::trim_end).find(|line| line.starts_with("まで"));
    let made_outcome = made_line.and_then(outcome_from_made_line);

    let mut in_moves = false;
    // 指し手と、それに付く評価値コメント (CSA の行)。
    let mut body: Vec<String> = Vec::new();
    let mut moves_count = 0_u32;
    let mut last_dest = None;
    let mut terminal: Option<&str> = None;

    for line in text.lines() {
        let line = line.trim_end();
        if line.starts_with("まで") {
            continue;
        }
        if !in_moves {
            if line.starts_with("手数--") {
                in_moves = true;
            } else if let Some(value) = line.strip_prefix("棋戦：") {
                event = Some(value.trim());
            } else if let Some(value) = line.strip_prefix("開始日時：") {
                start_time = Some(value.trim());
            } else if let Some(value) = line.strip_prefix("手合割：") {
                handicap = Some(value.trim());
            } else if let Some(value) = line.strip_prefix("先手：") {
                black = Some(value.trim());
            } else if let Some(value) = line.strip_prefix("後手：") {
                white = Some(value.trim());
            } else if let Some(found) = TIME_SETTING.captures(line) {
                clock = found[1].parse::<u64>().ok().zip(found[2].parse::<u64>().ok());
            } else if let Some(value) = line.strip_prefix("*終局理由：") {
                end_reason = value.trim();
            } else if TIME_CUT.is_match(line) || line.starts_with("*指定局面名") {
                kept_comments.push(&line[1..]);
            }
            continue;
        }
        if terminal.is_some() {
            // 終局の手の後ろ (2 行目の `入玉宣言` など) は読まない。
            continue;
        }
        if let Some(found) = EVAL_COMMENT.captures(line) {
            // 指し手の前のコメント、数値でない評価値は落とす。
            if let (Some(dest), Ok(value)) = (last_dest, found[1].parse::<i64>()) {
                let pv = convert_pv(found.get(2).map_or("", |pv| pv.as_str()), dest);
                let mut comment = format!("'** {value}");
                for mv in pv {
                    comment.push(' ');
                    comment.push_str(&mv);
                }
                body.push(comment);
            }
            continue;
        }
        if line.starts_with(['*', '#', '&']) || line.is_empty() {
            continue;
        }
        let Some(found) = MOVE_LINE.captures(line) else {
            return Err(failed(REASON_BAD_MOVE, line));
        };
        let move_text = found.get(2).map_or("", |m| m.as_str());
        if found[1].parse::<u32>().ok() != Some(moves_count + 1) {
            return Err(failed(REASON_MOVE_NUMBER, line));
        }
        if let Some(mv) = parse_kif_move(move_text, last_dest) {
            let sign = if moves_count.is_multiple_of(2) {
                '+'
            } else {
                '-'
            };
            let mut csa_move = format!("{sign}{}", mv.body);
            if let (Some(minutes), Some(seconds)) = (found.get(3), found.get(4))
                && let (Ok(minutes), Ok(seconds)) =
                    (minutes.as_str().parse::<u64>(), seconds.as_str().parse::<u64>())
            {
                csa_move.push_str(&format!(",T{}", minutes * 60 + seconds));
            }
            body.push(csa_move);
            last_dest = Some(mv.dest);
            moves_count += 1;
            continue;
        }
        terminal = Some(match move_text {
            "投了" => "TORYO",
            "千日手" => "SENNICHITE",
            "切れ負け" => "TIME_UP",
            "入玉宣言" | "入玉勝ち" => "KACHI",
            "持将棋" if end_reason == "最大手数" => "MAX_MOVES",
            "持将棋" => "JISHOGI",
            // 反則の内容 (非合法手・王手千日手) は問わず、負けた側の反則として記録する。
            "反則負け" => match made_outcome {
                Some(Outcome::BlackWins) => "-ILLEGAL_ACTION",
                Some(Outcome::WhiteWins) => "+ILLEGAL_ACTION",
                _ => return Err(failed(REASON_NO_WINNER, line)),
            },
            _ if move_text
                .chars()
                .next()
                .is_some_and(|c| c == '同' || zenkaku_digit(c).is_some()) =>
            {
                return Err(failed(REASON_BAD_MOVE, line));
            }
            _ => return Err(failed(REASON_UNKNOWN_END, move_text)),
        });
    }

    if handicap.is_some_and(|handicap| handicap != "平手") {
        return Err(Rejection::Excluded(REASON_HANDICAP));
    }
    if moves_count == 0 {
        return Err(Rejection::Excluded(REASON_NO_MOVES));
    }
    let Some(start_time) = start_time.filter(|value| is_csa_datetime(value)) else {
        return Err(Rejection::Excluded(REASON_NO_START_TIME));
    };
    let (Some(black), Some(white)) =
        (black.filter(|name| !name.is_empty()), white.filter(|name| !name.is_empty()))
    else {
        return Err(failed(REASON_BAD_HEADER, ""));
    };
    // 終局の手が無い棋譜 (接続切れなど) は中断として記録する。
    let special = terminal.unwrap_or("CHUDAN");
    // 平手の先手番から始まるので、偶数手を指した後は先手番。
    if let (Some(expected), Some(converted)) =
        (made_outcome, outcome_from_special(special, moves_count.is_multiple_of(2)))
        && expected != converted
    {
        return Err(failed(
            REASON_RESULT_MISMATCH,
            format!("%{special} ({moves_count} 手) / {}", made_line.unwrap_or("")),
        ));
    }

    let mut csa = format!("V2.2\nN+{black}\nN-{white}\n");
    if let Some(event) = event.filter(|event| !event.is_empty()) {
        csa.push_str(&format!("$EVENT:{event}\n"));
    }
    csa.push_str(&format!("$START_TIME:{start_time}\n"));
    if let Some((minutes, increment)) = clock {
        csa.push_str(&format!(
            "$TIME_LIMIT:{:02}:{:02}+00\n'Increment:{increment}\n",
            minutes / 60,
            minutes % 60
        ));
    }
    for comment in kept_comments {
        csa.push_str(&format!("'{comment}\n"));
    }
    csa.push_str("PI\n+\n");
    for line in body {
        csa.push_str(&line);
        csa.push('\n');
    }
    csa.push_str(&format!("%{special}\n"));
    if let Some(made_line) = made_line {
        csa.push_str(&format!("'{made_line}\n"));
    }
    Ok(csa)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kifu_publish::cp932::decode_kifu_bytes;

    fn body(text: &str, previous_dest: Option<(u8, u8)>) -> Option<String> {
        parse_kif_move(text, previous_dest).map(|mv| mv.body)
    }

    #[test]
    fn moves_are_converted_without_tracking_the_board() {
        for (kif, csa) in [
            ("７六歩(77)", "7776FU"),
            ("８七歩打", "0087FU"),
            ("５二金打", "0052KI"),
            ("３三角成(88)", "8833UM"),
            ("３三角不成(88)", "8833KA"),
            ("８七歩成(86)", "8687TO"),
            ("１七桂成(25)", "2517NK"),
            ("４八香成(42)", "4248NY"),
            ("５二銀成(41)", "4152NG"),
            ("６九飛成(39)", "3969RY"),
            // 既に成っている駒の移動。
            ("２二成香(32)", "3222NY"),
            ("７二成桂(63)", "6372NK"),
            ("３七成銀(47)", "4737NG"),
            ("２二杏(32)", "3222NY"),
            ("７二圭(63)", "6372NK"),
            ("３七全(47)", "4737NG"),
            ("６三と(73)", "7363TO"),
            ("６五馬(56)", "5665UM"),
            ("６七龍(69)", "6967RY"),
            ("６七竜(69)", "6967RY"),
            ("５一玉(52)", "5251OU"),
            ("５一王(52)", "5251OU"),
            ("９九香(98)", "9899KY"),
        ] {
            assert_eq!(body(kif, None).as_deref(), Some(csa), "{kif}");
        }
        // `同` は直前の移動先。全角空白の有無は問わない。
        assert_eq!(body("同　歩(87)", Some((8, 6))).as_deref(), Some("8786FU"));
        assert_eq!(body("同歩成(87)", Some((8, 6))).as_deref(), Some("8786TO"));
        assert_eq!(body("同　龍(77)", Some((7, 8))).as_deref(), Some("7778RY"));
        assert_eq!(body("同　歩(87)", None), None);
    }

    #[test]
    fn malformed_moves_are_rejected() {
        for kif in [
            "",
            "投了",
            "７六歩",
            "７六歩(7)",
            "７六歩(777)",
            "７六歩(07)",
            "７六歩(77)x",
            "７六金成(77)",
            "７六と成(77)",
            "７十歩(77)",
            "76歩(77)",
            "７六駒(77)",
            "７六歩打打",
        ] {
            assert!(parse_kif_move(kif, Some((1, 1))).is_none(), "{kif}");
        }
    }

    const TWO_MOVES: &str =
        " 1 ７六歩(77)           ( 0:03/00:00:03)\n 2 ３四歩(33)           ( 1:01/00:01:01)\n";

    fn kif(header_extra: &str, moves: &str, made: &str) -> String {
        format!(
            "棋戦：テスト棋戦 1回戦\n開始日時：2025/12/07 09:00:21\n手合割：平手\n先手：黒\n後手：白\n{header_extra}手数----指手---------消費時間--\n{moves}{made}"
        )
    }

    fn special(csa: &str) -> &str {
        csa.lines().find(|line| line.starts_with('%')).unwrap()
    }

    fn reason(result: Result<String, Rejection>) -> &'static str {
        match result {
            Err(Rejection::Failed { reason, .. }) => reason,
            other => panic!("変換失敗になるはず: {other:?}"),
        }
    }

    #[test]
    fn header_moves_and_times_are_written_as_csa() {
        let text = kif(
            "*先手持ち時間420秒削減\n*棋戦名：テスト棋戦 1回戦\n*指定局面名：先手3分\n*終局理由：投了\n*時間設定：10分-1手2秒加算\n*対局時間：1分\n",
            &format!("{TWO_MOVES} 3 投了           ( 0:00/00:00:03)\n"),
            "まで2手で後手の勝ち\n",
        );
        let expected = "V2.2\nN+黒\nN-白\n$EVENT:テスト棋戦 1回戦\n$START_TIME:2025/12/07 09:00:21\n$TIME_LIMIT:00:10+00\n'Increment:2\n'先手持ち時間420秒削減\n'指定局面名：先手3分\nPI\n+\n+7776FU,T3\n-3334FU,T61\n%TORYO\n'まで2手で後手の勝ち\n";
        assert_eq!(kif_to_csa(&text).unwrap(), expected);
        // CRLF、対局者名の末尾のタブ・空白も同じ結果になる。
        let crlf = text.replace("先手：黒\n", "先手：黒\t \n").replace('\n', "\r\n");
        assert_eq!(kif_to_csa(&crlf).unwrap(), expected);
    }

    #[test]
    fn clock_is_written_only_for_the_known_format() {
        for (setting, expect_clock) in [
            ("*時間設定：10分-1手2秒加算\n", Some("$TIME_LIMIT:00:10+00\n'Increment:2\n")),
            ("*時間設定：90分-1手5秒加算\n", Some("$TIME_LIMIT:01:30+00\n'Increment:5\n")),
            ("*時間設定：10分-秒読み10秒\n", None),
            ("*時間設定：10分\n", None),
            ("", None),
        ] {
            let csa = kif_to_csa(&kif(setting, TWO_MOVES, "")).unwrap();
            match expect_clock {
                Some(clock) => assert!(csa.contains(clock), "{setting}"),
                None => {
                    assert!(!csa.contains("$TIME_LIMIT"), "{setting}");
                    assert!(!csa.contains("'Increment"), "{setting}");
                }
            }
        }
    }

    #[test]
    fn time_with_empty_total_is_read() {
        let csa = kif_to_csa(&kif(
            "",
            " 1 ２六歩(27)           ( 0:07/)\n 2 反則負け           ( 0:00/)\n",
            "まで1手で非合法手により先手の勝ち\n",
        ))
        .unwrap();
        assert!(csa.contains("+2726FU,T7\n%-ILLEGAL_ACTION\n"), "{csa}");
    }

    #[test]
    fn eval_comments_become_floodgate_style_comments() {
        let moves = concat!(
            "**対局 評価値 5 読み筋 ▲７六歩(77)\n",
            " 1 ７六歩(77)           ( 0:03/00:00:03)\n",
            "**対局 評価値 122 読み筋 △３四歩(33) ▲２二角成(88) △同　銀(31) ▲４五角打\n",
            " 2 ３四歩(33)           ( 0:01/00:00:01)\n",
            "**対局 評価値 -30 読み筋 ▲同　歩(35) △８四歩(83) ▲壊れた手 △８五歩(84)\n",
            " 3 ２二角成(88)           ( 0:01/00:00:04)\n",
            "**対局 評価値 0 読み筋 \n",
            " 4 同　銀(31)           ( 0:01/00:00:02)\n",
            "**対局 評価値 -100000 読み筋 +1725KE\n",
            " 5 ４五角打           ( 0:01/00:00:05)\n",
            "**対局 評価値 詰み 読み筋 △８四歩(83)\n",
            " 6 ８四歩(83)           ( 0:01/00:00:03)\n",
            "**対局 評価値 77\n",
            "*一般のコメント\n",
        );
        let csa = kif_to_csa(&kif("", moves, "")).unwrap();
        let moves_part = csa.split_once("PI\n+\n").unwrap().1;
        assert_eq!(
            moves_part,
            concat!(
                // 指し手より前のコメントは付け先が無いので落とす。
                "+7776FU,T3\n",
                "'** 122 -3334FU +8822UM -3122GI +0045KA\n",
                "-3334FU,T1\n",
                // 先頭の `同` は実際に指された手 (３四) を指す。変換できない手で切る。
                "'** -30 +3534FU -8384FU\n",
                "+8822UM,T1\n",
                "'** 0\n",
                "-3122GI,T1\n",
                // KIF でない読み筋は捨て、評価値だけ残す。
                "'** -100000\n",
                "+0045KA,T1\n",
                // 数値でない評価値はコメントごと落とす。
                "-8384FU,T1\n",
                "'** 77\n",
                "%CHUDAN\n",
            )
        );
    }

    #[test]
    fn endings_map_to_csa_special_moves() {
        let one = " 1 ７六歩(77)           ( 0:03/00:00:03)\n";
        for (moves, end_reason, made, expected) in [
            (
                format!("{one} 2 投了 ( 0:00/00:00:00)\n"),
                "投了",
                "まで1手で先手の勝ち",
                "%TORYO",
            ),
            (
                format!("{TWO_MOVES} 3 投了 ( 0:00/00:00:00)\n"),
                "接続切れ",
                "まで2手で後手の勝ち",
                "%TORYO",
            ),
            (
                format!("{TWO_MOVES} 3 千日手 ( 0:00/00:00:00)\n"),
                "千日手",
                "まで2手で千日手",
                "%SENNICHITE",
            ),
            (
                format!("{one} 2 切れ負け ( 0:00/00:00:00)\n"),
                "切れ負け",
                "まで1手で時間切れにより先手の勝ち",
                "%TIME_UP",
            ),
            (
                format!("{TWO_MOVES} 3 切れ負け ( 0:00/00:00:00)\n"),
                "切れ負け",
                "まで2手で時間切れにより後手の勝ち",
                "%TIME_UP",
            ),
            // 宣言の行は 2 つ並ぶ。`まで` 行に勝者は書かれない。
            (
                format!("{one} 2 入玉宣言 ( 0:00/00:00:00)\n 3 入玉宣言 ( 0:00/00:00:00)\n"),
                "宣言勝ち",
                "まで2手で入玉宣言",
                "%KACHI",
            ),
            (
                format!("{one} 2 入玉勝ち ( 0:00/00:00:00)\n 3 入玉勝ち ( 0:00/00:00:00)\n"),
                "宣言勝ち",
                "まで2手で入玉勝ち",
                "%KACHI",
            ),
            (
                format!("{TWO_MOVES} 3 持将棋 ( 0:00/00:00:00)\n"),
                "持将棋",
                "まで2手で持将棋により引分け",
                "%JISHOGI",
            ),
            (
                format!("{TWO_MOVES} 3 持将棋 ( 0:00/00:00:00)\n"),
                "最大手数",
                "まで2手で持将棋により引分け",
                "%MAX_MOVES",
            ),
            // 反則負けは `まで` 行の勝者の反対側の反則として書く。
            (
                format!("{one} 2 反則負け ( 0:00/00:00:00)\n"),
                "非合法手",
                "まで1手で非合法手により先手の勝ち",
                "%-ILLEGAL_ACTION",
            ),
            (
                format!("{TWO_MOVES} 3 反則負け ( 0:00/00:00:00)\n"),
                "非合法手",
                "まで2手で非合法手により後手の勝ち",
                "%+ILLEGAL_ACTION",
            ),
            (
                format!("{TWO_MOVES} 3 反則負け ( 0:00/00:00:00)\n"),
                "oute_千日手",
                "まで2手で王手千日手により先手の勝ち",
                "%-ILLEGAL_ACTION",
            ),
            // 終局の手が無い棋譜は中断。
            (TWO_MOVES.to_owned(), "接続切れ", "", "%CHUDAN"),
            (TWO_MOVES.to_owned(), "", "", "%CHUDAN"),
        ] {
            let header = format!("*終局理由：{end_reason}\n");
            let made_line = if made.is_empty() {
                String::new()
            } else {
                format!("{made}\n")
            };
            let csa = kif_to_csa(&kif(&header, &moves, &made_line)).unwrap();
            assert_eq!(special(&csa), expected, "{end_reason} / {made}");
            assert_eq!(csa.matches('%').count(), 1, "{end_reason} / {made}");
            if !made.is_empty() {
                assert!(csa.ends_with(&format!("{expected}\n'{made}\n")), "{csa}");
            }
        }
    }

    #[test]
    fn unconvertible_games_are_reported_not_guessed() {
        let one = " 1 ７六歩(77)           ( 0:03/00:00:03)\n";
        // 反則負けなのに `まで` 行から勝者が読めない。
        assert_eq!(
            reason(kif_to_csa(&kif("", &format!("{one} 2 反則負け ( 0:00/)\n"), ""))),
            REASON_NO_WINNER
        );
        assert_eq!(
            reason(kif_to_csa(&kif("", &format!("{one} 2 詰み ( 0:00/)\n"), ""))),
            REASON_UNKNOWN_END
        );
        assert_eq!(
            reason(kif_to_csa(&kif("", &format!("{one} 2 ７六金成(77) ( 0:00/)\n"), ""))),
            REASON_BAD_MOVE
        );
        assert_eq!(
            reason(kif_to_csa(&kif("", &format!("{one} 3 ３四歩(33) ( 0:00/)\n"), ""))),
            REASON_MOVE_NUMBER
        );
        // 1 手で投了なら先手の勝ちのはず。
        assert_eq!(
            reason(kif_to_csa(&kif(
                "",
                &format!("{one} 2 投了 ( 0:00/)\n"),
                "まで1手で後手の勝ち\n"
            ))),
            REASON_RESULT_MISMATCH
        );
        assert_eq!(
            reason(kif_to_csa(&kif(
                "",
                &format!("{TWO_MOVES} 3 千日手 ( 0:00/)\n"),
                "まで2手で先手の勝ち\n"
            ))),
            REASON_RESULT_MISMATCH
        );
        let no_name = kif("", TWO_MOVES, "").replace("後手：白\n", "後手：\n");
        assert_eq!(reason(kif_to_csa(&no_name)), REASON_BAD_HEADER);
    }

    #[test]
    fn games_outside_the_scope_are_excluded() {
        assert_eq!(
            kif_to_csa(&kif("*終局理由：\n", "", "")),
            Err(Rejection::Excluded(REASON_NO_MOVES))
        );
        assert_eq!(
            kif_to_csa(&kif("", " 1 投了 ( 0:00/)\n", "まで0手で後手の勝ち\n")),
            Err(Rejection::Excluded(REASON_NO_MOVES))
        );
        let handicap = kif("", TWO_MOVES, "").replace("手合割：平手", "手合割：香落ち");
        assert_eq!(kif_to_csa(&handicap), Err(Rejection::Excluded(REASON_HANDICAP)));
        let no_start = kif("", TWO_MOVES, "").replace("開始日時：2025/12/07 09:00:21\n", "");
        assert_eq!(kif_to_csa(&no_start), Err(Rejection::Excluded(REASON_NO_START_TIME)));
    }

    #[test]
    fn official_samples_convert_to_the_golden_csa() {
        for (kif, golden) in [
            (
                &include_bytes!("../../tests/fixtures/kifu_publish/dr2.kif")[..],
                include_str!("../../tests/golden/kifu_publish/dr2.csa"),
            ),
            (
                &include_bytes!("../../tests/fixtures/kifu_publish/dr6.kif")[..],
                include_str!("../../tests/golden/kifu_publish/dr6.csa"),
            ),
        ] {
            let text = decode_kifu_bytes(kif).unwrap();
            let csa = kif_to_csa(&text).unwrap();
            assert_eq!(csa, golden);
            // 同じ入力からは必ず同じ本文 (対局 ID は本文の SHA-256)。
            assert_eq!(kif_to_csa(&text).unwrap(), csa);
        }
    }
}
