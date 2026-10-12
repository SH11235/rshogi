//! 大会の公式 CSA 棋譜を、棋譜サーバーが読める形に正規化する。
//!
//! 対象は WCSC / WCSO の公式アーカイブで見つかった書式のゆれだけ。サーバーが元々読める
//! 棋譜は何も変えない。
//!
//! 1. 先頭の `'CSA encoding=...` 行を落とす (UTF-8 で送るので嘘になる)。
//! 2. 指し手・`%` 行の後ろに単独である `T<秒>` を `,T<秒>` として連結する
//!    (間のコメント行は飛び越える)。
//! 3. 持ち時間を `$TIME_LIMIT:HH:MM+SS` と `'Increment:<秒>` で書き足す
//!    (`$TIME:<総秒>+<秒読み>+<加算>` と `$TIME_LIMIT:<分>分＋手番×<秒>秒` から)。
//! 4. `$START_TIME` が無ければ、与えられた代わりの開始時刻を入れる。
//!
//! # 決定性
//!
//! サーバーの対局 ID は本文の SHA-256 なので、同じ対局は必ず同じ本文にする必要がある。
//! [`normalize_csa`] の出力は、棋譜の本文と [`StartTimeFallback`] だけで決まる。
//! `$START_TIME` がある棋譜は本文だけで決まる。**無い棋譜は補完値に依存する**: 同じ回戦の
//! 他の棋譜から取る値 ([`RoundStartTimes`]) は一緒に処理したファイルの集合で変わり、
//! 仮の値は利用者の指定で変わる。補完した棋譜を送り直すときは、同じディレクトリの中身・
//! 同じ指定で行う。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::Rejection;

/// [`Rejection::Excluded`] の理由: `$START_TIME` が無く、代わりの値も無い。
pub const REASON_NO_START_TIME: &str = "開始時刻なし";
const REASON_ORPHAN_TIME: &str = "消費時間の行を指し手に連結できない";

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

/// 補完した開始時刻の出どころ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartTimeSource {
    /// 同じ回戦の他の棋譜の最も早い `$START_TIME`。
    SameRound,
    /// 利用者が指定した仮の開始時刻。
    Given,
}

/// 正規化の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalized {
    /// 正規化した CSA 本文。
    pub csa: String,
    /// `$START_TIME` を補完したときの値と出どころ。元からあれば `None`。
    pub filled_start_time: Option<(String, StartTimeSource)>,
}

/// CSA 本文を正規化する。改行は `\n` に揃え、末尾に改行を 1 つ付ける。
///
/// - `$START_TIME` が無く `start` にも候補が無いときは
///   [`Rejection::Excluded`] ([`REASON_NO_START_TIME`])。
/// - 局面より後ろに、連結先の無い単独の `T<秒>` 行があれば [`Rejection::Failed`]。
/// - 持ち時間の記載が無い棋譜や読めない書式の持ち時間には何も足さない。
pub fn normalize_csa(text: &str, start: &StartTimeFallback<'_>) -> Result<Normalized, Rejection> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let mut needs_start_time = !lines.iter().any(|line| line.starts_with("$START_TIME:"));
    let mut needs_clock = !lines.iter().any(|line| {
        line.strip_prefix("$TIME_LIMIT:")
            .is_some_and(|value| SERVER_TIME_LIMIT.is_match(value))
    });
    let mut filled_start_time = None;
    let mut seen_position = false;

    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 4);
    for line in lines {
        if out.is_empty() && line.starts_with("'CSA encoding=") {
            continue;
        }
        // 局面より後ろの単独の `T<秒>` は、コメント行・空行を飛び越えて直前の指し手・
        // `%` 行に連結する。連結先が無い (既に時間が付いている、指し手がまだ無い) なら、
        // 消費時間を黙って落とさずに変換失敗にする。
        if seen_position && TIME_ONLY_LINE.is_match(line) {
            let target = out
                .iter_mut()
                .rev()
                .find(|previous| !previous.is_empty() && !previous.starts_with('\''))
                .filter(|previous| is_move_or_special_line(previous) && !previous.contains(','));
            let Some(target) = target else {
                return Err(Rejection::Failed {
                    reason: REASON_ORPHAN_TIME,
                    detail: line.to_owned(),
                });
            };
            target.push(',');
            target.push_str(line);
            continue;
        }
        if line.starts_with('P') {
            seen_position = true;
        }
        // 開始時刻は局面 (`PI` / `P1`...) の直前に入れる。
        if needs_start_time && line.starts_with('P') {
            needs_start_time = false;
            let (value, source, note) = match (start.same_round, start.given) {
                (Some(value), _) => (
                    value,
                    StartTimeSource::SameRound,
                    "'開始時刻は記録が無いため、同じ回戦の他の対局の最も早い開始時刻",
                ),
                (None, Some(value)) => {
                    (value, StartTimeSource::Given, "'開始時刻は記録が無いため大会日程からの仮の値")
                }
                (None, None) => return Err(Rejection::Excluded(REASON_NO_START_TIME)),
            };
            out.push(format!("$START_TIME:{value}"));
            out.push(note.to_owned());
            filled_start_time = Some((value.to_owned(), source));
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
    let mut csa = out.join("\n");
    csa.push('\n');
    Ok(Normalized {
        csa,
        filled_start_time,
    })
}

/// 回戦 (同じディレクトリの、ファイル名の「大会_回戦」部分) ごとの最も早い `$START_TIME`。
///
/// 開始時刻の記録が無い棋譜に入れる値を決めるのに使う。持つのは回戦ごとに 1 件。
#[derive(Debug, Default)]
pub struct RoundStartTimes {
    earliest: BTreeMap<(PathBuf, String), String>,
}

impl RoundStartTimes {
    fn round_of(path: &Path) -> Option<(PathBuf, String)> {
        let name = path.file_name()?.to_str()?;
        Some((path.parent()?.to_path_buf(), round_key(name)?.to_owned()))
    }

    /// `path` の棋譜 (本文 `csa`) の `$START_TIME` を、その回戦の候補に加える。
    /// 回戦名を取り出せないファイル名、開始時刻の無い・読めない棋譜は何もしない。
    pub fn observe(&mut self, path: &Path, csa: &str) {
        let (Some(round), Some(start)) = (Self::round_of(path), csa_start_time(csa)) else {
            return;
        };
        match self.earliest.get_mut(&round) {
            Some(current) if current.as_str() <= start => {}
            Some(current) => *current = start.to_owned(),
            None => {
                self.earliest.insert(round, start.to_owned());
            }
        }
    }

    /// `path` と同じディレクトリ・同じ回戦の最も早い開始時刻。
    pub fn earliest_for(&self, path: &Path) -> Option<&str> {
        self.earliest.get(&Self::round_of(path)?).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_FALLBACK: StartTimeFallback<'static> = StartTimeFallback {
        same_round: None,
        given: None,
    };

    fn normalize(text: &str) -> String {
        normalize_csa(text, &NO_FALLBACK).unwrap().csa
    }

    fn failure(text: &str) -> &'static str {
        match normalize_csa(text, &NO_FALLBACK) {
            Err(Rejection::Failed { reason, .. }) => reason,
            other => panic!("変換失敗になるはず: {other:?}"),
        }
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
        // 局面より前の T 行 (指し手の時間ではありえない) には触らない。
        let before_position = "V2.2\nN+a\nN-b\n$START_TIME:2021/05/03 10:00:00\nT5\nPI\n+\n";
        assert_eq!(normalize(before_position), before_position);
    }

    #[test]
    fn time_lines_skip_comments_to_reach_their_move() {
        // 指し手と T 行の間に評価値コメントや空行が挟まっても、消費時間を落とさない。
        let csa = "V2.2\nN+a\nN-b\n$START_TIME:2021/05/03 10:00:00\nPI\n+\n+2726FU\n'** 30 -8384FU\n'memo\nT18\n-8384FU\n\nT7\n%TORYO\n'end\nT6\n";
        assert_eq!(
            normalize(csa),
            "V2.2\nN+a\nN-b\n$START_TIME:2021/05/03 10:00:00\nPI\n+\n+2726FU,T18\n'** 30 -8384FU\n'memo\n-8384FU,T7\n\n%TORYO,T6\n'end\n"
        );
    }

    #[test]
    fn time_line_without_a_move_to_attach_is_a_failure() {
        let head = "V2.2\nN+a\nN-b\n$START_TIME:2021/05/03 10:00:00\nPI\n+\n";
        for tail in [
            // 指し手がまだ無い。
            "T5\n+2726FU\n",
            "'comment\nT5\n",
            // 既に時間が付いている (T 行が 2 つ続く場合も含む)。
            "+2726FU,T1\nT18\n",
            "+2726FU\nT1\nT18\n",
            "+2726FU,T1\n'comment\nT3\n",
            "%TORYO,T0\nT3\n",
        ] {
            assert_eq!(failure(&format!("{head}{tail}")), REASON_ORPHAN_TIME, "{tail}");
        }
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
            Normalized {
                csa: "V2.2\nN+a\nN-b\n$START_TIME:2021/05/04 09:00:00\n'開始時刻は記録が無いため大会日程からの仮の値\nPI\n+\n+2726FU,T1\n%TORYO\n".to_owned(),
                filled_start_time: Some(("2021/05/04 09:00:00".to_owned(), StartTimeSource::Given)),
            }
        );
        let both = StartTimeFallback {
            same_round: Some("2021/05/04 16:30:12"),
            given: Some("2021/05/04 09:00:00"),
        };
        assert_eq!(
            normalize_csa(csa, &both).unwrap(),
            Normalized {
                csa: "V2.2\nN+a\nN-b\n$START_TIME:2021/05/04 16:30:12\n'開始時刻は記録が無いため、同じ回戦の他の対局の最も早い開始時刻\nPI\n+\n+2726FU,T1\n%TORYO\n".to_owned(),
                filled_start_time: Some((
                    "2021/05/04 16:30:12".to_owned(),
                    StartTimeSource::SameRound
                )),
            }
        );
        // 開始時刻がある棋譜には候補を使わない。局面行が無い棋譜は対象外のまま。
        let with_start = "V2.2\nN+a\nN-b\n$START_TIME:2021/05/03 10:00:00\nPI\n+\n";
        assert_eq!(
            normalize_csa(with_start, &both).unwrap(),
            Normalized {
                csa: with_start.to_owned(),
                filled_start_time: None,
            }
        );
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
    fn round_start_times_keep_the_earliest_per_directory_and_round() {
        let csa = |start: &str| format!("V2.2\nN+a\nN-b\n$START_TIME:{start}\nPI\n+\n");
        let mut rounds = RoundStartTimes::default();
        // 投入順に依らず最小値を採る。
        rounds.observe(Path::new("a/WCSC31_L8_BBB_CCC.csa"), &csa("2021/05/03 19:12:40"));
        rounds.observe(Path::new("a/WCSC31_L8_AAA_BBB.csa"), &csa("2021/05/03 19:12:23"));
        rounds.observe(Path::new("a/WCSC31_L8_CCC_DDD.csa"), &csa("2021/05/03 19:13:00"));
        rounds.observe(Path::new("a/WCSC31_L7_AAA_CCC.csa"), &csa("2021/05/03 18:00:05"));
        // 別のディレクトリの同名の回戦は混ぜない。
        rounds.observe(Path::new("b/WCSC31_L8_AAA_BBB.csa"), &csa("2020/01/01 00:00:00"));
        // 開始時刻が無い・読めない棋譜、回戦名を取り出せないファイル名は候補にならない。
        rounds.observe(Path::new("a/WCSC31_L8_DDD_EEE.csa"), "V2.2\nN+a\nN-b\nPI\n+\n");
        rounds.observe(Path::new("a/WCSC31_L8_EEE_FFF.csa"), &csa("2021/5/3 1:00:00"));
        rounds.observe(Path::new("a/WCSC31-L8-x-y.csa"), &csa("2019/01/01 00:00:00"));

        assert_eq!(
            rounds.earliest_for(Path::new("a/WCSC31_L8_DDD_EEE.csa")),
            Some("2021/05/03 19:12:23")
        );
        assert_eq!(
            rounds.earliest_for(Path::new("a/WCSC31_L7_XXX_YYY.csa")),
            Some("2021/05/03 18:00:05")
        );
        assert_eq!(
            rounds.earliest_for(Path::new("b/WCSC31_L8_DDD_EEE.csa")),
            Some("2020/01/01 00:00:00")
        );
        assert_eq!(rounds.earliest_for(Path::new("a/WCSC31_L6_DDD_EEE.csa")), None);
        assert_eq!(rounds.earliest_for(Path::new("c/WCSC31_L8_DDD_EEE.csa")), None);
        assert_eq!(rounds.earliest_for(Path::new("a/WCSC31-L8-x-y.csa")), None);
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
            let (text, _) = decode_kifu_bytes(raw).unwrap();
            let normalized = normalize(&text);
            assert_eq!(normalized, golden);
            // `$START_TIME` がある棋譜は、本文だけで出力が決まる (対局 ID は本文の
            // SHA-256)。正規化済みをもう一度通しても変わらない。
            assert_eq!(normalize(&text), normalized);
            assert_eq!(normalize(&normalized), normalized);
        }
    }
}
