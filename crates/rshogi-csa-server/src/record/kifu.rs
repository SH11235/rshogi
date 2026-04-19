//! CSA V2 形式の棋譜生成。
//!
//! `record_v22.html` 準拠のテキストを組み立てる。`KifuRecord` をビルドして
//! [`KifuRecord::build_v2`] を呼ぶと、保存可能な棋譜文字列が得られる。
//!
//! 設計書 §KifuWriter に対応するモジュール。Phase 1 では Floodgate 拡張の
//! `'eval pv` コメントは `KifuMove::comment` に String として埋め込む形で支援する。
//!
//! # 記号方針
//!
//! 棋譜本体（`%...`）と 00LIST（`#...`）で語彙が異なる点は
//! `docs/csa-server/design.md` §6.5.1 を参照。特に連続王手千日手は棋譜本体が
//! `%ILLEGAL_MOVE` + `'OUTE_SENNICHITE` コメント、00LIST が `#OUTE_SENNICHITE`
//! の二層運用になっている（CSA 標準パーサ `rshogi_csa::parse_special_move` が
//! `%OUTE_SENNICHITE` を受理しないため）。結果コードは必ず
//! [`primary_result_code`] を単一ソースとして参照すること。

use std::fmt::Write as _;

use crate::game::result::{GameResult, IllegalReason};
use crate::types::{Color, CsaMoveToken, GameId, PlayerName};

/// 1 手分の記録。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KifuMove {
    /// CSA 手トークン（例: `+7776FU`）。
    pub token: CsaMoveToken,
    /// 消費時間（秒、整数切り捨て）。
    pub elapsed_sec: u32,
    /// 任意のコメント行（先頭 `'` は付けずに本文だけ渡す）。Floodgate 拡張で
    /// `eval=123 pv 7g7f 3c3d` のような評価値・PV を入れる用途。
    pub comment: Option<String>,
}

/// CSA V2 棋譜 1 件分のレコード。
#[derive(Debug, Clone)]
pub struct KifuRecord {
    /// 対局 ID（`20140101120000` 等）。
    pub game_id: GameId,
    /// 先手プレイヤ名。
    pub black: PlayerName,
    /// 後手プレイヤ名。
    pub white: PlayerName,
    /// 開始日時（CSA `$START_TIME:` 用に既にフォーマット済みの文字列、
    /// 例: `2026/04/17 12:00:00`）。タイムゾーンは呼び出し側の責任。
    pub start_time: String,
    /// 終了日時（`$END_TIME:`）。空文字なら出力しない。
    pub end_time: String,
    /// イベント名（任意）。`$EVENT:` 行に出力。空文字なら省略。
    pub event: String,
    /// 持ち時間セクション（`BEGIN Time` から `END Time` まで、末尾改行込み）。
    /// 通常は [`crate::game::clock::TimeClock::format_summary`] の戻り値を渡す。
    pub time_section: String,
    /// 初期局面ブロック（`PI` または `P1`–`P9`+持ち駒+手番）。空なら省略可。
    /// 平手初期局面は `"PI\n+"` などを渡す。
    pub initial_position: String,
    /// 指し手列。
    pub moves: Vec<KifuMove>,
    /// 終局結果。終局理由コード（`%TORYO` 等）と勝敗コード（`#RESIGN`+`#WIN/#LOSE`）を生成する。
    pub result: GameResult,
}

impl KifuRecord {
    /// CSA V2 棋譜テキストを組み立てる。
    ///
    /// 行末は `\n`（LF）。最終行も改行で終わる。
    pub fn build_v2(&self) -> String {
        let mut out = String::with_capacity(256);
        // バージョンタグ。
        out.push_str("V2.2\n");
        // プレイヤ名。
        let _ = writeln!(out, "N+{}", self.black);
        let _ = writeln!(out, "N-{}", self.white);
        // 任意のメタ。
        if !self.event.is_empty() {
            let _ = writeln!(out, "$EVENT:{}", self.event);
        }
        let _ = writeln!(out, "$GAME_ID:{}", self.game_id);
        if !self.start_time.is_empty() {
            let _ = writeln!(out, "$START_TIME:{}", self.start_time);
        }
        if !self.end_time.is_empty() {
            let _ = writeln!(out, "$END_TIME:{}", self.end_time);
        }
        // 持ち時間セクション。呼び出し側で末尾改行込みの形を渡す前提。
        out.push_str(&self.time_section);
        if !self.time_section.ends_with('\n') {
            out.push('\n');
        }
        // 初期局面。空なら省略（クライアント側が平手とみなす）。
        if !self.initial_position.is_empty() {
            out.push_str(&self.initial_position);
            if !self.initial_position.ends_with('\n') {
                out.push('\n');
            }
        }
        // 指し手列。
        for mv in &self.moves {
            let _ = writeln!(out, "{},T{}", mv.token, mv.elapsed_sec);
            if let Some(c) = mv.comment.as_ref() {
                // Floodgate 互換の `'` 始まりコメント行。
                let _ = writeln!(out, "'{c}");
            }
        }
        // 終局理由コード + 勝敗コード（必要に応じて）。
        for line in result_lines(&self.result) {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

/// 終局結果から棋譜末尾に記録する行を生成する。
///
/// CSA V2 棋譜の終局行は `%...` 特殊手のみ（`#WIN` / `#LOSE` / `#DRAW` /
/// `#CENSORED` は CSA プロトコルの通知コードであり、棋譜本体には書かない）。
/// `rshogi_csa::parse_special_move` が認識する語彙
/// (`TORYO`/`KACHI`/`HIKIWAKE`/`SENNICHITE`/`CHUDAN`/`TIME_UP`/`ILLEGAL_MOVE`/
/// `JISHOGI`/`MAX_MOVES`) に揃える。
///
/// 連続王手千日手は専用の `%` トークンが標準化されていないため、Phase 1 では
/// `%ILLEGAL_MOVE` で記録し、補足としてコメント行 `'OUTE_SENNICHITE` を追加する。
/// `IllegalReason::Uchifuzume` / `IllegalKachi` も同様にコメント行で残す。
fn result_lines(result: &GameResult) -> Vec<String> {
    match result {
        GameResult::Toryo { .. } => vec!["%TORYO".to_owned()],
        GameResult::TimeUp { .. } => vec!["%TIME_UP".to_owned()],
        GameResult::IllegalMove { reason, .. } => {
            let mut v = vec!["%ILLEGAL_MOVE".to_owned()];
            match reason {
                IllegalReason::Uchifuzume => v.push("'UCHIFUZUME".to_owned()),
                IllegalReason::IllegalKachi => v.push("'ILLEGAL_KACHI".to_owned()),
                IllegalReason::Generic => {}
            }
            v
        }
        GameResult::Kachi { .. } => vec!["%KACHI".to_owned()],
        GameResult::OuteSennichite { .. } => {
            vec!["%ILLEGAL_MOVE".to_owned(), "'OUTE_SENNICHITE".to_owned()]
        }
        GameResult::Sennichite => vec!["%SENNICHITE".to_owned()],
        GameResult::MaxMoves => vec!["%MAX_MOVES".to_owned()],
        GameResult::Abnormal { .. } => vec!["%CHUDAN".to_owned()],
    }
}

/// 00LIST 1 行分のフォーマット。
///
/// 形式: `<game_id> <sente> <gote> <start_time> <end_time> <result_code>`
/// （Ruby `mk_rate` 互換のシンプルなスペース区切り）。改行は呼び出し側で付ける。
pub fn format_zerozero_list_line(
    game_id: &GameId,
    black: &PlayerName,
    white: &PlayerName,
    start_time: &str,
    end_time: &str,
    result: &GameResult,
) -> String {
    let code = primary_result_code(result);
    format!("{game_id} {black} {white} {start_time} {end_time} {code}")
}

/// 終局結果から「主要結果コード 1 つ」を返す。00LIST 用に集約値が必要な箇所で使う。
///
/// 00LIST の `result_code` 列は CSA プロトコル通知コード (`#...`) を採用する
/// （Ruby `mk_rate` 互換）。棋譜本体の特殊手 (`%...`) とは語彙が異なる点に注意。
///
/// フロントエンド crate からも同じ語彙で `GameSummaryEntry::result_code` を
/// 埋めるため `pub` で公開している（TCP 側に二重定義を作らないためのシングルソース）。
pub fn primary_result_code(result: &GameResult) -> &'static str {
    match result {
        GameResult::Toryo { .. } => "#RESIGN",
        GameResult::TimeUp { .. } => "#TIME_UP",
        GameResult::IllegalMove { .. } => "#ILLEGAL_MOVE",
        GameResult::Kachi { .. } => "#JISHOGI",
        GameResult::OuteSennichite { .. } => "#OUTE_SENNICHITE",
        GameResult::Sennichite => "#SENNICHITE",
        GameResult::MaxMoves => "#MAX_MOVES",
        GameResult::Abnormal { .. } => "#ABNORMAL",
    }
}

/// 勝敗側を 00LIST 補助情報として取得するヘルパ。Floodgate 等のレートバッチが
/// 必要に応じて利用する。Phase 1 ではエクスポートのみ。
pub fn winner_of(result: &GameResult) -> Option<Color> {
    match result {
        GameResult::Toryo { winner } | GameResult::Kachi { winner } => Some(*winner),
        GameResult::TimeUp { loser }
        | GameResult::IllegalMove { loser, .. }
        | GameResult::OuteSennichite { loser } => Some(loser.opposite()),
        GameResult::Abnormal { winner } => *winner,
        GameResult::Sennichite | GameResult::MaxMoves => None,
    }
}

/// `IllegalReason` を CSA 棋譜上のサブコードへ変換するヘルパ（Phase 3 拡張用に予約）。
pub fn illegal_reason_subcode(reason: IllegalReason) -> &'static str {
    match reason {
        IllegalReason::Generic => "ILLEGAL_MOVE",
        IllegalReason::Uchifuzume => "UCHIFUZUME",
        IllegalReason::IllegalKachi => "ILLEGAL_KACHI",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec_skeleton() -> KifuRecord {
        KifuRecord {
            game_id: GameId::new("20140101120000"),
            black: PlayerName::new("alice"),
            white: PlayerName::new("bob"),
            start_time: "2026/04/17 12:00:00".to_owned(),
            end_time: "2026/04/17 12:05:00".to_owned(),
            event: "rshogi-csa-server-test".to_owned(),
            time_section: "BEGIN Time\nTime_Unit:1sec\nTotal_Time:600\nByoyomi:10\nLeast_Time_Per_Move:0\nEND Time\n".to_owned(),
            initial_position: String::new(),
            moves: vec![
                KifuMove { token: CsaMoveToken::new("+7776FU"), elapsed_sec: 3, comment: None },
                KifuMove { token: CsaMoveToken::new("-3334FU"), elapsed_sec: 4, comment: Some("eval=12 pv 3c3d".to_owned()) },
            ],
            result: GameResult::Toryo { winner: Color::White },
        }
    }

    #[test]
    fn build_v2_starts_with_version_and_includes_player_names() {
        let txt = rec_skeleton().build_v2();
        assert!(txt.starts_with("V2.2\n"));
        assert!(txt.contains("\nN+alice\n"));
        assert!(txt.contains("\nN-bob\n"));
    }

    #[test]
    fn build_v2_emits_event_and_game_id_and_times() {
        let txt = rec_skeleton().build_v2();
        assert!(txt.contains("\n$EVENT:rshogi-csa-server-test\n"));
        assert!(txt.contains("\n$GAME_ID:20140101120000\n"));
        assert!(txt.contains("\n$START_TIME:2026/04/17 12:00:00\n"));
        assert!(txt.contains("\n$END_TIME:2026/04/17 12:05:00\n"));
    }

    #[test]
    fn build_v2_includes_time_section_verbatim() {
        let txt = rec_skeleton().build_v2();
        assert!(txt.contains("BEGIN Time\n"));
        assert!(txt.contains("Time_Unit:1sec\n"));
        assert!(txt.contains("Total_Time:600\n"));
        assert!(txt.contains("END Time\n"));
    }

    #[test]
    fn build_v2_emits_moves_with_t_field_and_comment_lines() {
        let txt = rec_skeleton().build_v2();
        assert!(txt.contains("\n+7776FU,T3\n"));
        assert!(txt.contains("\n-3334FU,T4\n"));
        // Floodgate 拡張のコメント行（先頭 `'`）。
        assert!(txt.contains("\n'eval=12 pv 3c3d\n"));
    }

    #[test]
    fn build_v2_ends_with_special_move_only() {
        let txt = rec_skeleton().build_v2();
        // 棋譜末尾は %TORYO のみ。`#RESIGN` などの protocol 通知コードは入れない。
        assert!(txt.contains("\n%TORYO\n"));
        assert!(!txt.contains("#RESIGN"));
        assert!(!txt.contains("#WIN"));
        assert!(!txt.contains("#LOSE"));
        // 末尾は改行で終わる。
        assert!(txt.ends_with('\n'));
    }

    #[test]
    fn build_v2_omits_optional_fields_when_empty() {
        let mut rec = rec_skeleton();
        rec.event = String::new();
        rec.start_time = String::new();
        rec.end_time = String::new();
        let txt = rec.build_v2();
        assert!(!txt.contains("$EVENT:"));
        assert!(!txt.contains("$START_TIME:"));
        assert!(!txt.contains("$END_TIME:"));
        // GAME_ID は常に出る。
        assert!(txt.contains("$GAME_ID:20140101120000"));
    }

    #[test]
    fn result_lines_use_csa_special_move_vocabulary() {
        // すべて parse_special_move() が認識する `%...` 語彙のみで構成される。
        assert_eq!(
            result_lines(&GameResult::Toryo {
                winner: Color::Black
            }),
            vec!["%TORYO"]
        );
        assert_eq!(
            result_lines(&GameResult::TimeUp {
                loser: Color::Black
            }),
            vec!["%TIME_UP"]
        );
        assert_eq!(
            result_lines(&GameResult::IllegalMove {
                loser: Color::Black,
                reason: IllegalReason::Uchifuzume,
            }),
            vec!["%ILLEGAL_MOVE", "'UCHIFUZUME"]
        );
        assert_eq!(
            result_lines(&GameResult::IllegalMove {
                loser: Color::Black,
                reason: IllegalReason::IllegalKachi,
            }),
            vec!["%ILLEGAL_MOVE", "'ILLEGAL_KACHI"]
        );
        assert_eq!(
            result_lines(&GameResult::IllegalMove {
                loser: Color::Black,
                reason: IllegalReason::Generic,
            }),
            vec!["%ILLEGAL_MOVE"]
        );
        assert_eq!(
            result_lines(&GameResult::Kachi {
                winner: Color::Black
            }),
            vec!["%KACHI"]
        );
        assert_eq!(
            result_lines(&GameResult::OuteSennichite {
                loser: Color::Black
            }),
            vec!["%ILLEGAL_MOVE", "'OUTE_SENNICHITE"]
        );
        assert_eq!(result_lines(&GameResult::Sennichite), vec!["%SENNICHITE"]);
        assert_eq!(result_lines(&GameResult::MaxMoves), vec!["%MAX_MOVES"]);
        assert_eq!(
            result_lines(&GameResult::Abnormal {
                winner: Some(Color::Black)
            }),
            vec!["%CHUDAN"]
        );
    }

    /// rshogi-csa パーサで round-trip できることを確認する回帰テスト。
    #[test]
    fn build_v2_is_parseable_by_rshogi_csa() {
        let mut rec = rec_skeleton();
        // 平手初期局面ヘッダを入れて parse_csa が局面を再構成できるようにする。
        rec.initial_position = "PI\n+\n".to_owned();
        let txt = rec.build_v2();
        let (_pos, moves, info) = rshogi_csa::parse_csa(&txt).expect("CSA parser should accept");
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[0], "+7776FU");
        assert_eq!(moves[1], "-3334FU");
        assert_eq!(info.black_name.as_deref(), Some("alice"));
        assert_eq!(info.white_name.as_deref(), Some("bob"));
    }

    #[test]
    fn zerozero_list_line_format() {
        let line = format_zerozero_list_line(
            &GameId::new("g1"),
            &PlayerName::new("alice"),
            &PlayerName::new("bob"),
            "2026-04-17T12:00:00Z",
            "2026-04-17T12:10:00Z",
            &GameResult::Toryo {
                winner: Color::Black,
            },
        );
        assert_eq!(line, "g1 alice bob 2026-04-17T12:00:00Z 2026-04-17T12:10:00Z #RESIGN");
    }

    #[test]
    fn winner_of_resolves_correctly() {
        assert_eq!(
            winner_of(&GameResult::Toryo {
                winner: Color::Black
            }),
            Some(Color::Black)
        );
        assert_eq!(
            winner_of(&GameResult::TimeUp {
                loser: Color::White
            }),
            Some(Color::Black)
        );
        assert_eq!(winner_of(&GameResult::Sennichite), None);
        assert_eq!(winner_of(&GameResult::MaxMoves), None);
        assert_eq!(winner_of(&GameResult::Abnormal { winner: None }), None);
    }
}
