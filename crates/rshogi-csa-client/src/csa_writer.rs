//! wdoor 形式の CSA 棋譜テキストを組み立てる。
//!
//! wdoor (shogi-server) の公開棋譜と同じ並びで出力する:
//!
//! ```text
//! +7776FU
//! T2
//! '** 54 +7776FU -3334FU
//! ...
//! %TORYO
//! 'summary:toryo:<先手名> win:<後手名> lose
//! '$END_TIME:2026/10/09 00:21:55
//! ```
//!
//! `'**` コメントは直前の指し手のもので、評価値は先手視点。`csa_client` の棋譜保存と、
//! `tools` の `kifu_publish jsonl`（JSONL → CSA 変換）が同じ実装を使う。
//!
//! 勝敗は `'summary:` 行で表す。wdoor の実物と同じく、反則負けと連続王手の千日手は
//! `%` コマンドを書かずに `'summary:` だけで終える（反則手は指し手行にせず
//! `'ILLEGAL_MOVE(<手>)` コメントにする）。勝敗が決まっていない局は `%CHUDAN` で終え、
//! 勝敗の付く終局コマンドは書かない。

use std::fmt::Write as _;

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, TimeDelta, TimeZone};

use rshogi_csa::{Color, Position, usi_move_to_csa};

/// 棋譜に記録する終局理由。
///
/// [`EndReason::as_str`] の値が `GameRecord::result` と JSONL `result.reason` に入る。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// 投了（`#RESIGN`）。
    Resign,
    /// 時間切れ（`#TIME_UP`）。
    TimeUp,
    /// 反則手（`#ILLEGAL_MOVE`）。
    IllegalMove,
    /// 入玉宣言勝ち（`%KACHI` 成立。サーバー通知は `#JISHOGI` + `#WIN` / `#LOSE`）。
    WinDeclaration,
    /// 連続王手の千日手（`#OUTE_SENNICHITE`）。王手側の負け。
    OuteSennichite,
    /// 千日手（`#SENNICHITE`）。引き分け。
    Sennichite,
    /// 持将棋による引き分け（`#JISHOGI` + `#DRAW`）。
    Jishogi,
    /// 最大手数到達（`#MAX_MOVES`）。引き分け。
    MaxMoves,
    /// 切断などの異常終了（`#ABNORMAL`）、および未知の理由行での決着。
    Abnormal,
    /// 中断（`#CHUDAN`）。勝敗なし。
    Interrupted,
}

impl EndReason {
    /// 全ての終局理由。
    pub const ALL: [EndReason; 10] = [
        EndReason::Resign,
        EndReason::TimeUp,
        EndReason::IllegalMove,
        EndReason::WinDeclaration,
        EndReason::OuteSennichite,
        EndReason::Sennichite,
        EndReason::Jishogi,
        EndReason::MaxMoves,
        EndReason::Abnormal,
        EndReason::Interrupted,
    ];

    /// `GameRecord::result` / JSONL `result.reason` に書く文字列。
    pub fn as_str(self) -> &'static str {
        match self {
            EndReason::Resign => "resign",
            EndReason::TimeUp => "time_up",
            EndReason::IllegalMove => "illegal_move",
            EndReason::WinDeclaration => "win_declaration",
            EndReason::OuteSennichite => "oute_sennichite",
            EndReason::Sennichite => "sennichite",
            EndReason::Jishogi => "jishogi",
            EndReason::MaxMoves => "max_moves",
            EndReason::Abnormal => "abnormal",
            EndReason::Interrupted => "interrupted",
        }
    }

    /// [`EndReason::as_str`] の逆変換。未知の文字列は `None`。
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.as_str() == s)
    }

    /// `GameRecord::result` / `csa_client` の JSONL `result.reason` を終局理由に直す。
    ///
    /// サーバーが理由行を送らずに `#WIN` / `#LOSE` だけを返した対局は `win` / `lose` と
    /// 記録される。理由行を省くサーバーでの通常の決着として投了扱いにする。未終局
    /// （空文字）や未知の文字列は `None`。
    pub fn from_record_result(s: &str) -> Option<Self> {
        match s {
            "win" | "lose" => Some(EndReason::Resign),
            other => Self::parse(other),
        }
    }

    /// 勝者がいないとき引き分けとして扱う理由か。
    pub fn is_draw(self) -> bool {
        matches!(self, EndReason::Sennichite | EndReason::Jishogi | EndReason::MaxMoves)
    }

    /// 勝敗が決まった局の末尾に書く CSA の終局コマンド。
    ///
    /// wdoor の実物に合わせ、反則負けと連続王手の千日手は `%` コマンドを書かない
    /// （勝敗は `'summary:` 行が持つ）。異常終了は勝者がいても `%CHUDAN` にする。
    fn csa_command(self) -> Option<&'static str> {
        match self {
            EndReason::Resign => Some("%TORYO"),
            EndReason::TimeUp => Some("%TIME_UP"),
            EndReason::WinDeclaration => Some("%KACHI"),
            EndReason::Sennichite => Some("%SENNICHITE"),
            EndReason::Jishogi => Some("%JISHOGI"),
            EndReason::MaxMoves => Some("%MAX_MOVES"),
            EndReason::Abnormal | EndReason::Interrupted => Some("%CHUDAN"),
            EndReason::IllegalMove | EndReason::OuteSennichite => None,
        }
    }

    /// `'summary:` 行の理由文字列。
    ///
    /// `jishogi` 以外は wdoor の実物（2026 年 6〜7 月の約 2.4 万局）に現れる表記。
    /// `jishogi`（`#JISHOGI` + `#DRAW` の持将棋引き分け）は wdoor にない独自ラベル。
    /// 勝者の付いた中断は wdoor の語彙に合わせて `abnormal` とする。
    fn summary_label(self) -> &'static str {
        match self {
            EndReason::Resign => "toryo",
            EndReason::TimeUp => "time up",
            EndReason::IllegalMove => "illegal move",
            EndReason::WinDeclaration => "kachi",
            EndReason::OuteSennichite => "oute_sennichite",
            EndReason::Sennichite => "sennichite",
            EndReason::Jishogi => "jishogi",
            EndReason::MaxMoves => "max_moves",
            EndReason::Abnormal | EndReason::Interrupted => "abnormal",
        }
    }
}

/// 持ち時間（秒）。`$TIME_LIMIT:HH:MM+SS` と `'Increment:<秒>` に書く。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CsaTimeLimit {
    /// 持ち時間。`$TIME_LIMIT` は分単位なので、1 分未満の端数は切り捨てて書く。
    pub total_sec: u64,
    /// 秒読み。
    pub byoyomi_sec: u64,
    /// 1 手ごとの加算（フィッシャー）。0 より大きいとき `'Increment:<秒>` 行を書く。
    pub increment_sec: u64,
}

/// 棋譜ヘッダー。
#[derive(Clone, Debug)]
pub struct CsaGameHeader<'a> {
    /// 先手の対局者名（`N+`）。
    pub sente_name: &'a str,
    /// 後手の対局者名（`N-`）。
    pub gote_name: &'a str,
    /// `$EVENT` に書く文字列。
    pub event: &'a str,
    /// 対局開始時刻（`$START_TIME`）。タイムゾーン表記がなく、棋譜サーバーは JST として
    /// 読むので、[`jst_naive`] で JST に直した値を渡す。
    pub start_time: NaiveDateTime,
    /// 持ち時間。`None` なら `$TIME_LIMIT` 行を書かない。
    pub time_limit: Option<CsaTimeLimit>,
    /// バージョン行の直後に書くコメント行（先頭の `'` を除いた本文）。
    pub leading_comment: Option<&'a str>,
}

/// 指し手に付ける評価値と読み筋。
#[derive(Clone, Copy, Debug)]
pub struct MoveEval<'a> {
    /// 先手視点の評価値。
    pub score_black: i32,
    /// 指す前の局面からの読み筋（USI）。先頭が実際の指し手と一致するときだけ出力する。
    pub pv_usi: &'a [String],
}

/// 終局情報。
#[derive(Clone, Debug)]
pub struct CsaGameEnd {
    /// 終局理由。
    pub reason: EndReason,
    /// 勝者。引き分け・勝敗なしは `None`。
    pub winner: Option<Color>,
    /// 終局時刻（`'$END_TIME`）。`None` なら書かない。JST（[`jst_naive`]）で渡す。
    pub end_time: Option<NaiveDateTime>,
    /// 反則負けになった手（CSA 形式）。サーバーに受理されていないので指し手行には
    /// 書かず、理由が反則のとき `'ILLEGAL_MOVE(<手>)` コメントとして残す。
    pub rejected_move: Option<String>,
}

/// 時刻を JST の壁時計時刻に直す。CSA の `$START_TIME` / `'$END_TIME:` 用。
pub fn jst_naive<Tz: TimeZone>(time: &DateTime<Tz>) -> NaiveDateTime {
    let utc = time.naive_utc();
    utc.checked_add_signed(TimeDelta::hours(9)).unwrap_or(utc)
}

/// wdoor 形式 CSA の逐次ライター。
pub struct WdoorCsaWriter {
    out: String,
    pos: Position,
    sente_name: String,
    gote_name: String,
}

impl WdoorCsaWriter {
    /// ヘッダーと開始局面を書き出した状態で作る。
    pub fn new(header: &CsaGameHeader<'_>, initial_position: &Position) -> Self {
        let mut out = String::new();
        out.push_str("V2.2\n");
        if let Some(comment) = header.leading_comment {
            let _ = writeln!(out, "'{comment}");
        }
        let _ = writeln!(out, "N+{}", header.sente_name);
        let _ = writeln!(out, "N-{}", header.gote_name);
        let _ = writeln!(out, "$EVENT:{}", header.event);
        let _ = writeln!(out, "$START_TIME:{}", format_csa_time(header.start_time));
        if let Some(limit) = header.time_limit {
            // CSA 規格の `HH:MM+SS`（持ち時間の時:分 + 秒読みの秒）。フィッシャーの加算は
            // wdoor と同じ `'Increment:<秒>` コメントで表す。
            let (hours, minutes) = (limit.total_sec / 3600, limit.total_sec % 3600 / 60);
            let _ = writeln!(out, "$TIME_LIMIT:{hours:02}:{minutes:02}+{:02}", limit.byoyomi_sec);
            if limit.increment_sec > 0 {
                let _ = writeln!(out, "'Increment:{}", limit.increment_sec);
            }
        }
        let _ = writeln!(out, "{}", initial_position.to_csa_board());
        Self {
            out,
            pos: initial_position.clone(),
            sente_name: header.sente_name.to_owned(),
            gote_name: header.gote_name.to_owned(),
        }
    }

    /// 次に指す局面。
    pub fn position(&self) -> &Position {
        &self.pos
    }

    /// CSA 形式の指し手を 1 手書く（指し手 → `T<秒>` → `'** <評価値> <読み筋>`）。
    ///
    /// 行は常に書き出す。戻り値は内部局面へ指し手を適用できたかで、失敗した場合
    /// 以降の読み筋は変換できない局面から作られるため、呼び出し側で中止を判断する。
    pub fn push_move(
        &mut self,
        csa_move: &str,
        time_sec: u64,
        eval: Option<MoveEval<'_>>,
    ) -> Result<()> {
        let _ = writeln!(self.out, "{csa_move}");
        let _ = writeln!(self.out, "T{time_sec}");
        if let Some(eval) = eval {
            let _ = write!(self.out, "'** {}", eval.score_black);
            for csa in pv_to_csa(eval.pv_usi, csa_move, &self.pos) {
                let _ = write!(self.out, " {csa}");
            }
            self.out.push('\n');
        }
        self.pos.apply_csa_move(csa_move)
    }

    /// USI 形式の指し手を CSA に変換して 1 手書く。変換できない手はエラー。
    pub fn push_usi_move(
        &mut self,
        usi_move: &str,
        time_sec: u64,
        eval: Option<MoveEval<'_>>,
    ) -> Result<()> {
        let csa_move = usi_move_to_csa(usi_move, &self.pos)?;
        self.push_move(&csa_move, time_sec, eval)
    }

    /// 終局の行（終局コマンド・`'summary:`・`'$END_TIME:`）を書いて棋譜テキストを返す。
    ///
    /// `end` が `None`（未終局）なら指し手までで終える。
    ///
    /// - 勝敗が決まっている局（勝者あり、または引き分けの理由）は必ず `'summary:` を書く。
    /// - 勝敗が決まっていない局（中断、結果を受け取る前の切断など）は理由によらず
    ///   `%CHUDAN` だけを書く。`%TORYO` のように手番から勝敗が読める終局コマンドを
    ///   残すと、未確定の局が決着済みとして扱われてしまうため。
    pub fn finish(mut self, end: Option<&CsaGameEnd>) -> String {
        let Some(end) = end else {
            return self.out;
        };
        let results = match end.winner {
            Some(Color::Black) => Some(("win", "lose")),
            Some(Color::White) => Some(("lose", "win")),
            None if end.reason.is_draw() => Some(("draw", "draw")),
            None => None,
        };
        match results {
            Some((sente, gote)) => {
                if end.reason == EndReason::IllegalMove
                    && let Some(rejected) = &end.rejected_move
                {
                    let _ = writeln!(self.out, "'ILLEGAL_MOVE({rejected})");
                }
                if let Some(command) = end.reason.csa_command() {
                    let _ = writeln!(self.out, "{command}");
                }
                let _ = writeln!(
                    self.out,
                    "'summary:{}:{} {sente}:{} {gote}",
                    end.reason.summary_label(),
                    self.sente_name,
                    self.gote_name
                );
            }
            None => self.out.push_str("%CHUDAN\n"),
        }
        if let Some(end_time) = end.end_time {
            let _ = writeln!(self.out, "'$END_TIME:{}", format_csa_time(end_time));
        }
        self.out
    }
}

/// CSA ヘッダーの時刻表記（`YYYY/MM/DD HH:MM:SS`）。
fn format_csa_time(time: NaiveDateTime) -> String {
    time.format("%Y/%m/%d %H:%M:%S").to_string()
}

/// 読み筋（USI）を、指す前の局面から CSA の指し手列に変換する。
///
/// `csa_client` が Floodgate へ送るコメントと同じく、読み筋の先頭は実際の指し手そのもの
/// （wdoor の棋譜には、相手の応手から始める流儀のエンジンも混在する）。先頭が実際の
/// 指し手と違う読み筋は別の手を読んだ結果なので、空を返す。途中で変換できない手・
/// 局面に適用できない手が出たら、その手の前までを返す。
pub fn pv_to_csa(pv_usi: &[String], played_csa: &str, pos: &Position) -> Vec<String> {
    let mut out = Vec::with_capacity(pv_usi.len());
    let mut pv_pos = pos.clone();
    for usi in pv_usi {
        let Ok(csa) = usi_move_to_csa(usi, &pv_pos) else {
            break;
        };
        if out.is_empty() && csa != played_csa {
            break;
        }
        if pv_pos.apply_csa_move(&csa).is_err() {
            break;
        }
        out.push(csa);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDate, Utc};
    use rshogi_csa::{SpecialMove, SummaryResult, initial_position};

    fn time(h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 9).unwrap().and_hms_opt(h, m, s).unwrap()
    }

    fn header<'a>() -> CsaGameHeader<'a> {
        CsaGameHeader {
            sente_name: "alice",
            gote_name: "bob",
            event: "test-event",
            start_time: time(0, 0, 0),
            time_limit: Some(CsaTimeLimit {
                total_sec: 300,
                byoyomi_sec: 0,
                increment_sec: 10,
            }),
            leading_comment: None,
        }
    }

    fn end(reason: EndReason, winner: Option<Color>) -> CsaGameEnd {
        CsaGameEnd {
            reason,
            winner,
            end_time: None,
            rejected_move: None,
        }
    }

    fn pv(moves: &[&str]) -> Vec<String> {
        moves.iter().map(|m| (*m).to_owned()).collect()
    }

    #[test]
    fn writes_wdoor_layout() {
        let mut writer = WdoorCsaWriter::new(&header(), &initial_position());
        let black_pv = pv(&["7g7f", "3c3d", "2g2f"]);
        writer
            .push_move(
                "+7776FU",
                2,
                Some(MoveEval {
                    score_black: 54,
                    pv_usi: &black_pv,
                }),
            )
            .unwrap();
        writer.push_usi_move("3c3d", 0, None).unwrap();
        let csa = writer.finish(Some(&CsaGameEnd {
            reason: EndReason::Resign,
            winner: Some(Color::White),
            end_time: Some(time(0, 21, 55)),
            rejected_move: None,
        }));
        let expected = "V2.2\n\
N+alice\n\
N-bob\n\
$EVENT:test-event\n\
$START_TIME:2026/10/09 00:00:00\n\
$TIME_LIMIT:00:05+00\n\
'Increment:10\n\
P1-KY-KE-GI-KI-OU-KI-GI-KE-KY\n\
P2 * -HI *  *  *  *  * -KA * \n\
P3-FU-FU-FU-FU-FU-FU-FU-FU-FU\n\
P4 *  *  *  *  *  *  *  *  * \n\
P5 *  *  *  *  *  *  *  *  * \n\
P6 *  *  *  *  *  *  *  *  * \n\
P7+FU+FU+FU+FU+FU+FU+FU+FU+FU\n\
P8 * +KA *  *  *  *  * +HI * \n\
P9+KY+KE+GI+KI+OU+KI+GI+KE+KY\n\
+\n\
+7776FU\n\
T2\n\
'** 54 +7776FU -3334FU +2726FU\n\
-3334FU\n\
T0\n\
%TORYO\n\
'summary:toryo:alice lose:bob win\n\
'$END_TIME:2026/10/09 00:21:55\n";
        assert_eq!(csa, expected);
    }

    #[test]
    fn output_is_readable_by_csa_parser() {
        let mut writer = WdoorCsaWriter::new(&header(), &initial_position());
        let black_pv = pv(&["7g7f"]);
        writer
            .push_move(
                "+7776FU",
                3,
                Some(MoveEval {
                    score_black: 30,
                    pv_usi: &black_pv,
                }),
            )
            .unwrap();
        writer.push_move("-3334FU", 4, None).unwrap();
        let csa = writer.finish(Some(&end(EndReason::TimeUp, Some(Color::White))));
        let (_, moves, info, evals) = rshogi_csa::parse_csa_full_with_evals(&csa).unwrap();
        assert_eq!(info.black_name.as_deref(), Some("alice"));
        assert_eq!(evals, vec![Some(30), None]);
        assert_eq!(
            moves,
            vec![
                rshogi_csa::ParsedMove::Normal(rshogi_csa::CsaMove {
                    mv: "+7776FU".to_owned(),
                    time_sec: Some(3),
                }),
                rshogi_csa::ParsedMove::Normal(rshogi_csa::CsaMove {
                    mv: "-3334FU".to_owned(),
                    time_sec: Some(4),
                }),
                rshogi_csa::ParsedMove::Special(SpecialMove::TimeUp),
            ]
        );
        let summary = info.summary.unwrap();
        assert_eq!(summary.result, SummaryResult::WhiteWin);
        assert_eq!(summary.special_move(), Some(SpecialMove::TimeUp));
    }

    #[test]
    fn end_reason_lines_and_summary() {
        let cases: [(EndReason, Option<Color>, &str); 13] = [
            (
                EndReason::Resign,
                Some(Color::Black),
                "%TORYO\n'summary:toryo:alice win:bob lose\n",
            ),
            (
                EndReason::TimeUp,
                Some(Color::White),
                "%TIME_UP\n'summary:time up:alice lose:bob win\n",
            ),
            // 反則負けと連続王手の千日手は wdoor と同じく `%` コマンドなし。
            (
                EndReason::IllegalMove,
                Some(Color::Black),
                "'summary:illegal move:alice win:bob lose\n",
            ),
            (
                EndReason::WinDeclaration,
                Some(Color::White),
                "%KACHI\n'summary:kachi:alice lose:bob win\n",
            ),
            (
                EndReason::OuteSennichite,
                Some(Color::Black),
                "'summary:oute_sennichite:alice win:bob lose\n",
            ),
            (
                EndReason::Sennichite,
                None,
                "%SENNICHITE\n'summary:sennichite:alice draw:bob draw\n",
            ),
            (EndReason::Jishogi, None, "%JISHOGI\n'summary:jishogi:alice draw:bob draw\n"),
            (
                EndReason::MaxMoves,
                None,
                "%MAX_MOVES\n'summary:max_moves:alice draw:bob draw\n",
            ),
            (
                EndReason::Abnormal,
                Some(Color::Black),
                "%CHUDAN\n'summary:abnormal:alice win:bob lose\n",
            ),
            // 勝敗が決まっていない終局は、理由によらず `%CHUDAN` だけ。
            (EndReason::Abnormal, None, "%CHUDAN\n"),
            (EndReason::Interrupted, None, "%CHUDAN\n"),
            (EndReason::Resign, None, "%CHUDAN\n"),
            (EndReason::WinDeclaration, None, "%CHUDAN\n"),
        ];
        for (reason, winner, expected_tail) in cases {
            let writer = WdoorCsaWriter::new(&header(), &initial_position());
            let csa = writer.finish(Some(&end(reason, winner)));
            let tail = csa.split_once("\n+\n").unwrap().1;
            assert_eq!(tail, expected_tail, "{reason:?} {winner:?}");
        }
    }

    /// 不変条件: 勝者が確定していれば必ず `'summary:` があり、パーサーが同じ勝者を読む。
    /// 勝敗が決まっていなければ `'summary:` も勝敗の付く終局コマンドもない。
    #[test]
    fn summary_is_written_exactly_when_the_result_is_decided() {
        for reason in EndReason::ALL {
            for winner in [Some(Color::Black), Some(Color::White), None] {
                let writer = WdoorCsaWriter::new(&header(), &initial_position());
                let csa = writer.finish(Some(&end(reason, winner)));
                let (_, moves, info) = rshogi_csa::parse_csa_full(&csa).unwrap();
                let expected = match winner {
                    Some(Color::Black) => Some(SummaryResult::BlackWin),
                    Some(Color::White) => Some(SummaryResult::WhiteWin),
                    None if reason.is_draw() => Some(SummaryResult::Draw),
                    None => None,
                };
                assert_eq!(info.summary.map(|s| s.result), expected, "{reason:?} {winner:?}");
                if expected.is_none() {
                    assert_eq!(
                        moves,
                        [rshogi_csa::ParsedMove::Special(SpecialMove::Interrupt)],
                        "{reason:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn rejected_illegal_move_becomes_a_comment() {
        let mut writer = WdoorCsaWriter::new(&header(), &initial_position());
        writer.push_move("+7776FU", 1, None).unwrap();
        let csa = writer.finish(Some(&CsaGameEnd {
            reason: EndReason::IllegalMove,
            winner: Some(Color::Black),
            end_time: None,
            rejected_move: Some("-8292HI".to_owned()),
        }));
        assert!(
            csa.ends_with(
                "+7776FU\nT1\n'ILLEGAL_MOVE(-8292HI)\n'summary:illegal move:alice win:bob lose\n"
            ),
            "{csa}"
        );
        // 反則以外の理由では、受理されなかった手は書かない。
        let writer = WdoorCsaWriter::new(&header(), &initial_position());
        let csa = writer.finish(Some(&CsaGameEnd {
            reason: EndReason::TimeUp,
            winner: Some(Color::Black),
            end_time: None,
            rejected_move: Some("-8292HI".to_owned()),
        }));
        assert!(!csa.contains("8292HI"), "{csa}");
    }

    #[test]
    fn unfinished_game_has_no_terminal_lines() {
        let mut writer = WdoorCsaWriter::new(&header(), &initial_position());
        writer.push_move("+7776FU", 1, None).unwrap();
        let csa = writer.finish(None);
        assert!(csa.ends_with("+7776FU\nT1\n"), "{csa}");
    }

    #[test]
    fn pv_is_dropped_when_root_differs_from_played_move() {
        let pos = initial_position();
        assert!(pv_to_csa(&pv(&["2g2f", "8c8d"]), "+7776FU", &pos).is_empty());
        // 途中で変換できない手が出たらそこまで。
        assert_eq!(
            pv_to_csa(&pv(&["7g7f", "3c3d", "5e5f", "2g2f"]), "+7776FU", &pos),
            ["+7776FU", "-3334FU"]
        );
        // 変換はできても局面に適用できない手（持っていない駒を打つ）は出力に残さない。
        assert_eq!(
            pv_to_csa(&pv(&["7g7f", "3c3d", "P*5e", "2g2f"]), "+7776FU", &pos),
            ["+7776FU", "-3334FU"]
        );
        // USI として壊れたトークンでも panic しない。
        assert_eq!(pv_to_csa(&pv(&["7g7f", "(3c3d)", "2g2f"]), "+7776FU", &pos), ["+7776FU"]);
    }

    #[test]
    fn end_reason_string_roundtrip() {
        for reason in EndReason::ALL {
            assert_eq!(EndReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(EndReason::parse("win"), None);
    }

    /// 棋譜サーバーの読み方（`$TIME_LIMIT:HH:MM+SS` の時:分 + 秒読み、`'Increment:<秒>`）
    /// と同じ手順で読み戻し、書いた持ち時間に戻ること。
    #[test]
    fn time_limit_roundtrips_through_the_server_reading() {
        fn read_back(csa: &str) -> (u64, u64, Option<u64>) {
            let limit = csa.lines().find_map(|l| l.strip_prefix("$TIME_LIMIT:")).unwrap();
            let (main, byoyomi) = limit.split_once('+').unwrap();
            let (hours, minutes) = main.split_once(':').unwrap();
            let total = (hours.parse::<u64>().unwrap() * 60 + minutes.parse::<u64>().unwrap()) * 60;
            let increment = csa
                .lines()
                .find_map(|l| l.strip_prefix("'Increment:"))
                .map(|v| v.parse().unwrap());
            (total, byoyomi.parse().unwrap(), increment)
        }
        let cases = [
            // (持ち時間, 秒読み, 加算) → (ヘッダー行, 読み戻し)
            ((900, 10, 0), "$TIME_LIMIT:00:15+10\n", (900, 10, None)),
            ((300, 0, 10), "$TIME_LIMIT:00:05+00\n'Increment:10\n", (300, 0, Some(10))),
            ((7200, 60, 0), "$TIME_LIMIT:02:00+60\n", (7200, 60, None)),
            ((0, 3, 0), "$TIME_LIMIT:00:00+03\n", (0, 3, None)),
            // 1 分未満の端数は書けないので切り捨てる。
            ((905, 5, 0), "$TIME_LIMIT:00:15+05\n", (900, 5, None)),
        ];
        for ((total_sec, byoyomi_sec, increment_sec), line, expected) in cases {
            let mut h = header();
            h.time_limit = Some(CsaTimeLimit {
                total_sec,
                byoyomi_sec,
                increment_sec,
            });
            let csa = WdoorCsaWriter::new(&h, &initial_position()).finish(None);
            assert!(csa.contains(&format!("\n{line}P1")), "{csa}");
            assert_eq!(read_back(&csa), expected);
        }
    }

    #[test]
    fn header_without_time_limit_and_with_leading_comment() {
        let mut h = header();
        h.time_limit = None;
        h.leading_comment = Some("RECONNECT_FRAGMENT: why");
        let csa = WdoorCsaWriter::new(&h, &initial_position()).finish(None);
        assert!(csa.starts_with("V2.2\n'RECONNECT_FRAGMENT: why\nN+alice\n"), "{csa}");
        assert!(!csa.contains("$TIME_LIMIT"), "{csa}");
        assert!(!csa.contains("'Increment"), "{csa}");
    }

    #[test]
    fn jst_naive_converts_any_offset_to_jst_wall_clock() {
        let expected = time(9, 0, 0);
        let utc = Utc.with_ymd_and_hms(2026, 10, 9, 0, 0, 0).unwrap();
        assert_eq!(jst_naive(&utc), expected);
        let pacific = FixedOffset::west_opt(7 * 3600).unwrap();
        assert_eq!(jst_naive(&utc.with_timezone(&pacific)), expected);
        let jst = FixedOffset::east_opt(9 * 3600).unwrap();
        assert_eq!(jst_naive(&utc.with_timezone(&jst)), expected);
    }
}
