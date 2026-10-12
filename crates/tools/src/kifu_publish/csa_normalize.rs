//! 大会の公式 CSA 棋譜を、棋譜サーバーが読める形に正規化する。
//!
//! 対象は WCSC / WCSO の公式アーカイブで見つかった書式のゆれだけ。サーバーが元々読める
//! 棋譜は何も変えない。出力は入力だけで決まる (サーバーの対局 ID は本文の SHA-256 なので、
//! 同じ入力からは必ず同じ本文を作る)。
//!
//! 1. 先頭の `'CSA encoding=...` 行を落とす (UTF-8 で送るので嘘になる)。
//! 2. 指し手・`%` 行の次の行に単独である `T<秒>` を `,T<秒>` として連結する。
//! 3. 持ち時間を `$TIME_LIMIT:HH:MM+SS` と `'Increment:<秒>` で書き足す
//!    (`$TIME:<総秒>+<秒読み>+<加算>` と `$TIME_LIMIT:<分>分＋手番×<秒>秒` から)。
//! 4. `$START_TIME` が無ければ、与えられた代わりの開始時刻を入れる。

use std::sync::LazyLock;

use regex::Regex;

use super::Rejection;

/// [`Rejection::Excluded`] の理由: `$START_TIME` が無く、代わりの値も無い。
pub const REASON_NO_START_TIME: &str = "開始時刻なし";

static CSA_DATETIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9]{4}/[0-9]{2}/[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2}$").expect("固定の正規表現")
});
/// サーバーが読める `$TIME_LIMIT:` の値 (`HH:MM+SS`)。
static SERVER_TIME_LIMIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]+:[0-9]+\+[0-9]+$").expect("固定の正規表現"));
/// CSA V3.0 の `$TIME:<総秒>+<秒読み>+<加算>`。
static V3_TIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\$TIME:([0-9]+)\+([0-9]+)\+([0-9]+)$").expect("固定の正規表現"));
/// WCSC33 / 34 の `$TIME_LIMIT:15分＋手番×5秒` (34 は行頭に `'` が付く)。
static MINUTES_PLUS_PER_MOVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^('?)\$TIME_LIMIT:([0-9]+)分[＋+]手番×([0-9]+)秒$").expect("固定の正規表現")
});
static TIME_ONLY_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^T[0-9]+$").expect("固定の正規表現"));

/// `YYYY/MM/DD HH:MM:SS` (CSA の `$START_TIME` の形) かどうか。桁数まで固定なので、
/// この形どうしは文字列の比較で時刻の前後が決まる。
pub fn is_csa_datetime(value: &str) -> bool {
    CSA_DATETIME.is_match(value)
}

/// CSA 本文の `$START_TIME:` の値。[`is_csa_datetime`] の形でなければ `None`。
pub fn csa_start_time(csa: &str) -> Option<&str> {
    csa.lines()
        .find_map(|line| line.trim_end().strip_prefix("$START_TIME:"))
        .filter(|value| is_csa_datetime(value))
}

/// ファイル名の「大会_回戦」部分。`WCSC31_L8_AUA_ABZ.csa` → `WCSC31_L8`。
///
/// `_` 区切りで 3 要素以上ある名前だけが対象 (回戦の後ろに対局者が続く形)。
pub fn round_key(file_name: &str) -> Option<&str> {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(stem, _)| stem);
    let mut separators = stem.match_indices('_').map(|(index, _)| index);
    let (_, second) = (separators.next()?, separators.next()?);
    Some(&stem[..second])
}

/// `$START_TIME` が無い棋譜に入れる開始時刻の候補。どちらも `YYYY/MM/DD HH:MM:SS`。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StartTimeFallback<'a> {
    /// 同じ回戦の他の棋譜のうち最も早い `$START_TIME`。あればこちらを使う。
    pub same_round: Option<&'a str>,
    /// 利用者が指定した仮の開始時刻 (`--fallback-start-time`)。
    pub given: Option<&'a str>,
}

fn is_move_or_special_line(line: &str) -> bool {
    (line.starts_with(['+', '-']) && line.len() >= 7) || line.starts_with('%')
}

fn push_server_clock(out: &mut Vec<String>, total_minutes: u64, byoyomi_sec: u64, increment: u64) {
    out.push(format!(
        "$TIME_LIMIT:{:02}:{:02}+{byoyomi_sec:02}",
        total_minutes / 60,
        total_minutes % 60
    ));
    if increment > 0 {
        out.push(format!("'Increment:{increment}"));
    }
}

/// CSA 本文を正規化する。改行は `\n` に揃え、末尾に改行を 1 つ付ける。
///
/// `$START_TIME` が無く `start` にも候補が無いときは
/// [`Rejection::Excluded`] ([`REASON_NO_START_TIME`])。持ち時間の記載が無い棋譜や
/// 読めない書式の持ち時間には何も足さない。
pub fn normalize_csa(text: &str, start: &StartTimeFallback<'_>) -> Result<String, Rejection> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let mut needs_start_time = !lines.iter().any(|line| line.starts_with("$START_TIME:"));
    let mut needs_clock = !lines.iter().any(|line| {
        line.strip_prefix("$TIME_LIMIT:")
            .is_some_and(|value| SERVER_TIME_LIMIT.is_match(value))
    });

    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 4);
    for line in lines {
        if out.is_empty() && line.starts_with("'CSA encoding=") {
            continue;
        }
        if TIME_ONLY_LINE.is_match(line)
            && let Some(previous) = out.last_mut()
            && is_move_or_special_line(previous)
            && !previous.contains(',')
        {
            previous.push(',');
            previous.push_str(line);
            continue;
        }
        // 開始時刻は局面 (`PI` / `P1`...) の直前に入れる。
        if needs_start_time && line.starts_with('P') {
            needs_start_time = false;
            match (start.same_round, start.given) {
                (Some(value), _) => {
                    out.push(format!("$START_TIME:{value}"));
                    out.push(
                        "'開始時刻は記録が無いため、同じ回戦の他の対局の最も早い開始時刻"
                            .to_owned(),
                    );
                }
                (None, Some(value)) => {
                    out.push(format!("$START_TIME:{value}"));
                    out.push("'開始時刻は記録が無いため大会日程からの仮の値".to_owned());
                }
                (None, None) => return Err(Rejection::Excluded(REASON_NO_START_TIME)),
            }
        }
        if needs_clock {
            if let Some(found) = V3_TIME.captures(line)
                && let (Ok(total_sec), Ok(byoyomi), Ok(increment)) =
                    (found[1].parse::<u64>(), found[2].parse::<u64>(), found[3].parse::<u64>())
                && total_sec % 60 == 0
            {
                needs_clock = false;
                out.push(line.to_owned());
                push_server_clock(&mut out, total_sec / 60, byoyomi, increment);
                continue;
            }
            if let Some(found) = MINUTES_PLUS_PER_MOVE.captures(line)
                && let (Ok(minutes), Ok(increment)) =
                    (found[2].parse::<u64>(), found[3].parse::<u64>())
            {
                needs_clock = false;
                // 元の行はコメントとして残す (既にコメントならそのまま)。
                let comment_mark = if found[1].is_empty() { "'" } else { "" };
                out.push(format!("{comment_mark}{line}"));
                push_server_clock(&mut out, minutes, 0, increment);
                continue;
            }
        }
        out.push(line.to_owned());
    }
    if needs_start_time {
        return Err(Rejection::Excluded(REASON_NO_START_TIME));
    }
    let mut normalized = out.join("\n");
    normalized.push('\n');
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_FALLBACK: StartTimeFallback<'static> = StartTimeFallback {
        same_round: None,
        given: None,
    };

    fn normalize(text: &str) -> String {
        normalize_csa(text, &NO_FALLBACK).unwrap()
    }

    #[test]
    fn server_readable_csa_is_left_alone() {
        let csa = "V2.2\nN+a\nN-b\n$EVENT:x\n$START_TIME:2026/10/09 12:00:00\n$TIME_LIMIT:00:15+00\n'Increment:5\nPI\n+\n+7776FU,T3\n'** 30 -3334FU\n-3334FU,T4\n%TORYO,T1\n";
        assert_eq!(normalize(csa), csa);
        // CRLF・行末の空白・BOM・末尾の改行なしは揃える。
        let crlf = format!("\u{feff}{}", csa.replace('\n', " \r\n"));
        assert_eq!(normalize(crlf.trim_end()), csa);
    }

    #[test]
    fn standalone_time_lines_are_joined_to_the_previous_move() {
        // WCSC31 / 32・WCSO1 の形式。
        let csa = "V2.2\nN+a\nN-b\n$START_TIME:2021/05/03 10:00:00\nPI\n+\n+2726FU\nT18\n-8384FU\nT0\n%TORYO\nT6\n";
        assert_eq!(
            normalize(csa),
            "V2.2\nN+a\nN-b\n$START_TIME:2021/05/03 10:00:00\nPI\n+\n+2726FU,T18\n-8384FU,T0\n%TORYO,T6\n"
        );
        // 既に時間が付いている手、指し手の直後でない T 行には触らない。
        let odd = "V2.2\nN+a\nN-b\n$START_TIME:2021/05/03 10:00:00\nT5\nPI\n+\n+2726FU,T1\nT18\n'comment\nT3\n";
        assert_eq!(normalize(odd), odd);
    }

    #[test]
    fn encoding_comment_is_dropped_only_at_the_top() {
        let csa = "'CSA encoding=SHIFT_JIS\nV3.0\nN+a\nN-b\n$START_TIME:2025/05/03 10:00:00\nPI\n+\n'CSA encoding=SHIFT_JIS\n%CHUDAN\n";
        assert_eq!(
            normalize(csa),
            "V3.0\nN+a\nN-b\n$START_TIME:2025/05/03 10:00:00\nPI\n+\n'CSA encoding=SHIFT_JIS\n%CHUDAN\n"
        );
    }

    fn header_lines(time_line: &str) -> Vec<String> {
        let csa = format!("V2.2\nN+a\nN-b\n$START_TIME:2025/05/03 10:00:00\n{time_line}\nPI\n+\n");
        normalize(&csa).lines().skip(4).map(str::to_owned).collect()
    }

    #[test]
    fn clock_is_rewritten_into_the_form_the_server_reads() {
        // WCSC35 (V3.0): 総秒 + 秒読み + 加算。元の行は残す。
        assert_eq!(
            header_lines("$TIME:900+0+5"),
            [
                "$TIME:900+0+5",
                "$TIME_LIMIT:00:15+00",
                "'Increment:5",
                "PI",
                "+"
            ]
        );
        assert_eq!(
            header_lines("$TIME:7200+30+0"),
            ["$TIME:7200+30+0", "$TIME_LIMIT:02:00+30", "PI", "+"]
        );
        // WCSC33: 元の行はコメントにする。
        assert_eq!(
            header_lines("$TIME_LIMIT:15分＋手番×5秒"),
            [
                "'$TIME_LIMIT:15分＋手番×5秒",
                "$TIME_LIMIT:00:15+00",
                "'Increment:5",
                "PI",
                "+"
            ]
        );
        // WCSC34: 元からコメント。
        assert_eq!(
            header_lines("'$TIME_LIMIT:15分＋手番×5秒"),
            [
                "'$TIME_LIMIT:15分＋手番×5秒",
                "$TIME_LIMIT:00:15+00",
                "'Increment:5",
                "PI",
                "+"
            ]
        );
    }

    #[test]
    fn unreadable_or_absent_clock_is_not_guessed() {
        for line in [
            "$TIME:910+0+5",
            "$TIME:900+0",
            "$TIME+:900+0+5",
            "$TIME_LIMIT:15分",
            "$TIME_LIMIT:15分 秒読み10秒",
            "'持ち時間 15分",
        ] {
            assert_eq!(header_lines(line), [line, "PI", "+"], "{line}");
        }
        let no_clock = "V2.2\nN+a\nN-b\n$START_TIME:2025/05/03 10:00:00\nPI\n+\n";
        assert_eq!(normalize(no_clock), no_clock);
    }

    #[test]
    fn normalizing_twice_changes_nothing() {
        for csa in [
            "'CSA encoding=SHIFT_JIS\nV3.0\nN+a\nN-b\n$START_TIME:2025/05/03 10:00:00\n$TIME:900+0+5\nPI\n+\n+2726FU\nT1\n%TORYO\nT0\n",
            "V2.2\nN+a\nN-b\n$START_TIME:2024/05/03 10:00:00\n'$TIME_LIMIT:15分＋手番×5秒\nP1-KY\n+\n",
            "V2.2\nN+a\nN-b\n$START_TIME:2023/05/03 10:00:00\n$TIME_LIMIT:15分＋手番×5秒\nPI\n+\n",
        ] {
            let once = normalize(csa);
            assert_ne!(once, csa);
            assert_eq!(normalize(&once), once);
        }
    }

    #[test]
    fn missing_start_time_uses_round_then_given_fallback_or_is_excluded() {
        let csa = "V2.2\nN+a\nN-b\nPI\n+\n+2726FU\nT1\n%TORYO\n";
        assert_eq!(
            normalize_csa(csa, &NO_FALLBACK),
            Err(Rejection::Excluded(REASON_NO_START_TIME))
        );
        let given = StartTimeFallback {
            same_round: None,
            given: Some("2021/05/04 09:00:00"),
        };
        assert_eq!(
            normalize_csa(csa, &given).unwrap(),
            "V2.2\nN+a\nN-b\n$START_TIME:2021/05/04 09:00:00\n'開始時刻は記録が無いため大会日程からの仮の値\nPI\n+\n+2726FU,T1\n%TORYO\n"
        );
        let both = StartTimeFallback {
            same_round: Some("2021/05/04 16:30:12"),
            given: Some("2021/05/04 09:00:00"),
        };
        assert_eq!(
            normalize_csa(csa, &both).unwrap(),
            "V2.2\nN+a\nN-b\n$START_TIME:2021/05/04 16:30:12\n'開始時刻は記録が無いため、同じ回戦の他の対局の最も早い開始時刻\nPI\n+\n+2726FU,T1\n%TORYO\n"
        );
        // 開始時刻がある棋譜には候補を使わない。局面行が無い棋譜は対象外のまま。
        let with_start = "V2.2\nN+a\nN-b\n$START_TIME:2021/05/03 10:00:00\nPI\n+\n";
        assert_eq!(normalize_csa(with_start, &both).unwrap(), with_start);
        assert_eq!(
            normalize_csa("V2.2\nN+a\nN-b\n", &both),
            Err(Rejection::Excluded(REASON_NO_START_TIME))
        );
    }

    #[test]
    fn round_key_and_start_time_helpers() {
        assert_eq!(round_key("WCSC31_L8_AUA_ABZ.csa"), Some("WCSC31_L8"));
        assert_eq!(round_key("WCSO1_F2_ACI_KAT.csa"), Some("WCSO1_F2"));
        assert_eq!(round_key("WCSC31_L8.csa"), None);
        assert_eq!(round_key("WCSC34-L1-argo-qinoa.csa"), None);
        assert_eq!(
            csa_start_time("V2.2\n$START_TIME:2021/05/03 10:00:00\r\nPI\n"),
            Some("2021/05/03 10:00:00")
        );
        assert_eq!(csa_start_time("V2.2\n$START_TIME:2021/5/3 10:00:00\nPI\n"), None);
        assert_eq!(csa_start_time("V2.2\nPI\n"), None);
        assert!(is_csa_datetime("2021/05/03 10:00:00"));
        assert!(!is_csa_datetime("2021-05-03 10:00:00"));
        assert!(!is_csa_datetime("2021/05/03 10:00"));
    }

    #[test]
    fn official_samples_normalize_to_the_golden_csa() {
        use crate::kifu_publish::cp932::decode_kifu_bytes;
        for (raw, golden) in [
            (
                &include_bytes!("../../tests/fixtures/kifu_publish/wcsc31.csa")[..],
                include_str!("../../tests/golden/kifu_publish/wcsc31.csa"),
            ),
            (
                &include_bytes!("../../tests/fixtures/kifu_publish/wcsc33.csa")[..],
                include_str!("../../tests/golden/kifu_publish/wcsc33.csa"),
            ),
            (
                &include_bytes!("../../tests/fixtures/kifu_publish/wcsc34.csa")[..],
                include_str!("../../tests/golden/kifu_publish/wcsc34.csa"),
            ),
            (
                &include_bytes!("../../tests/fixtures/kifu_publish/wcsc35.csa")[..],
                include_str!("../../tests/golden/kifu_publish/wcsc35.csa"),
            ),
        ] {
            let text = decode_kifu_bytes(raw).unwrap();
            let normalized = normalize(&text);
            assert_eq!(normalized, golden);
            // 同じ入力からは必ず同じ本文 (対局 ID は本文の SHA-256)。正規化済みを
            // もう一度通しても変わらない。
            assert_eq!(normalize(&text), normalized);
            assert_eq!(normalize(&normalized), normalized);
        }
    }
}
