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

use std::fmt::Write as _;

use anyhow::Result;
use chrono::NaiveDateTime;

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
    /// 切断などの異常終了（`#ABNORMAL`）。
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

    /// 棋譜末尾に書く行。CSA の終局コマンドと、必要なら補足コメント。
    ///
    /// 連続王手の千日手と異常終了には専用の `%` コマンドがないため、`rshogi-csa-server`
    /// の棋譜出力と同じく `%ILLEGAL_MOVE` + `'OUTE_SENNICHITE`、`%CHUDAN` で記録する。
    fn csa_lines(self) -> &'static [&'static str] {
        match self {
            EndReason::Resign => &["%TORYO"],
            EndReason::TimeUp => &["%TIME_UP"],
            EndReason::IllegalMove => &["%ILLEGAL_MOVE"],
            EndReason::WinDeclaration => &["%KACHI"],
            EndReason::OuteSennichite => &["%ILLEGAL_MOVE", "'OUTE_SENNICHITE"],
            EndReason::Sennichite => &["%SENNICHITE"],
            EndReason::Jishogi => &["%JISHOGI"],
            EndReason::MaxMoves => &["%MAX_MOVES"],
            EndReason::Abnormal | EndReason::Interrupted => &["%CHUDAN"],
        }
    }

    /// `'summary:` 行の理由文字列（wdoor の表記）。中断は勝敗がないので `None`。
    fn summary_label(self) -> Option<&'static str> {
        Some(match self {
            EndReason::Resign => "toryo",
            EndReason::TimeUp => "time up",
            EndReason::IllegalMove => "illegal move",
            EndReason::WinDeclaration => "kachi",
            EndReason::OuteSennichite => "oute_sennichite",
            EndReason::Sennichite => "sennichite",
            EndReason::Jishogi => "jishogi",
            EndReason::MaxMoves => "max_moves",
            EndReason::Abnormal => "abnormal",
            EndReason::Interrupted => return None,
        })
    }
}

/// `$TIME_LIMIT` 行に書く持ち時間（秒）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CsaTimeLimit {
    /// 持ち時間。
    pub total_sec: u64,
    /// 秒読み。
    pub byoyomi_sec: u64,
    /// 1 手ごとの加算。0 より大きいときフィッシャー表記（`+NNF`）にする。
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
    /// 対局開始時刻（`$START_TIME`）。
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
#[derive(Clone, Copy, Debug)]
pub struct CsaGameEnd {
    /// 終局理由。
    pub reason: EndReason,
    /// 勝者。引き分け・勝敗なしは `None`。
    pub winner: Option<Color>,
    /// 終局時刻（`'$END_TIME`）。`None` なら書かない。
    pub end_time: Option<NaiveDateTime>,
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
            let (min, sec) = (limit.total_sec / 60, limit.total_sec % 60);
            if limit.increment_sec > 0 {
                let _ = writeln!(out, "$TIME_LIMIT:{min}:{sec:02}+{:02}F", limit.increment_sec);
            } else {
                let _ = writeln!(out, "$TIME_LIMIT:{min}:{sec:02}+{:02}", limit.byoyomi_sec);
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

    /// 終局コマンドと `'summary:` / `'$END_TIME:` を書いて棋譜テキストを返す。
    ///
    /// `end` が `None`（未終局）なら指し手までで終える。`'summary:` は勝敗が決まって
    /// いるとき（勝者あり、または引き分けの理由）だけ書き、中断では書かない。
    pub fn finish(mut self, end: Option<&CsaGameEnd>) -> String {
        let Some(end) = end else {
            return self.out;
        };
        for line in end.reason.csa_lines() {
            let _ = writeln!(self.out, "{line}");
        }
        let results = match end.winner {
            Some(Color::Black) => Some(("win", "lose")),
            Some(Color::White) => Some(("lose", "win")),
            None if end.reason.is_draw() => Some(("draw", "draw")),
            None => None,
        };
        if let (Some((sente, gote)), Some(label)) = (results, end.reason.summary_label()) {
            let _ = writeln!(
                self.out,
                "'summary:{label}:{} {sente}:{} {gote}",
                self.sente_name, self.gote_name
            );
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
/// Floodgate へ送るコメントと同じく、読み筋の先頭は実際の指し手そのもの。先頭が
/// 実際の指し手と違う読み筋は別の手を読んだ結果なので、空を返す。途中で変換できない
/// 手が出たらそこまでを返す。
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
        let applied = pv_pos.apply_csa_move(&csa).is_ok();
        out.push(csa);
        if !applied {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rshogi_csa::initial_position;

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
        }));
        let expected = "V2.2\n\
N+alice\n\
N-bob\n\
$EVENT:test-event\n\
$START_TIME:2026/10/09 00:00:00\n\
$TIME_LIMIT:5:00+10F\n\
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
        let csa = writer.finish(Some(&CsaGameEnd {
            reason: EndReason::TimeUp,
            winner: Some(Color::White),
            end_time: None,
        }));
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
                rshogi_csa::ParsedMove::Special(rshogi_csa::SpecialMove::TimeUp),
            ]
        );
    }

    #[test]
    fn end_reason_lines_and_summary() {
        let cases: [(EndReason, Option<Color>, &str); 12] = [
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
            (
                EndReason::IllegalMove,
                Some(Color::Black),
                "%ILLEGAL_MOVE\n'summary:illegal move:alice win:bob lose\n",
            ),
            (
                EndReason::WinDeclaration,
                Some(Color::White),
                "%KACHI\n'summary:kachi:alice lose:bob win\n",
            ),
            (
                EndReason::OuteSennichite,
                Some(Color::Black),
                "%ILLEGAL_MOVE\n'OUTE_SENNICHITE\n'summary:oute_sennichite:alice win:bob lose\n",
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
            // 勝敗が決まっていない終局は summary を書かない。
            (EndReason::Abnormal, None, "%CHUDAN\n"),
            (EndReason::Interrupted, None, "%CHUDAN\n"),
            (EndReason::Resign, None, "%TORYO\n"),
        ];
        for (reason, winner, expected_tail) in cases {
            let writer = WdoorCsaWriter::new(&header(), &initial_position());
            let csa = writer.finish(Some(&CsaGameEnd {
                reason,
                winner,
                end_time: None,
            }));
            let tail = csa.split_once("\n+\n").unwrap().1;
            assert_eq!(tail, expected_tail, "{reason:?} {winner:?}");
        }
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
    }

    #[test]
    fn end_reason_string_roundtrip() {
        for reason in EndReason::ALL {
            assert_eq!(EndReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(EndReason::parse("win"), None);
    }

    #[test]
    fn time_limit_without_increment_uses_byoyomi_form() {
        let mut h = header();
        h.time_limit = Some(CsaTimeLimit {
            total_sec: 905,
            byoyomi_sec: 5,
            increment_sec: 0,
        });
        h.leading_comment = Some("RECONNECT_FRAGMENT: why");
        let csa = WdoorCsaWriter::new(&h, &initial_position()).finish(None);
        assert!(csa.starts_with("V2.2\n'RECONNECT_FRAGMENT: why\nN+alice\n"), "{csa}");
        assert!(csa.contains("$TIME_LIMIT:15:05+05\n"), "{csa}");
    }
}
