//! CSA V2 形式の棋譜生成。
//!
//! `record_v22.html` 準拠のテキストを組み立てる。`KifuRecord` をビルドして
//! [`KifuRecord::build_v2`] を呼ぶと、保存可能な棋譜文字列が得られる。
//!
//! Floodgate 拡張の `'eval pv` コメントは `KifuMove::comment` に String として
//! 埋め込む形で支援する。
//!
//! # 記号方針
//!
//! 棋譜本体（`%...`）と 00LIST（`#...`）で語彙が異なる。特に連続王手千日手は
//! 棋譜本体が `%ILLEGAL_MOVE` + `'OUTE_SENNICHITE` コメント、00LIST が
//! `#OUTE_SENNICHITE` の二層運用になっている（CSA 標準パーサ
//! `rshogi_csa::parse_special_move` が `%OUTE_SENNICHITE` を受理しないため）。
//! 結果コードは必ず [`primary_result_code`] を単一ソースとして参照すること。

use std::fmt::Write as _;

use rshogi_csa::{ParsedMove, initial_position, parse_csa_full};

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
/// 連続王手千日手は専用の `%` トークンが標準化されていないため、
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
/// （単一スペース区切り、改行は呼び出し側で付ける）。`split(' ')` で 6 列に
/// 一意分割できることが消費側との契約。
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
/// 00LIST の `result_code` 列は CSA プロトコル通知コード (`#...`) を採用する。
/// 棋譜本体の特殊手 (`%...`) とは語彙が異なる点に注意。
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

/// 勝敗側を 00LIST 補助情報として取得するヘルパ。レート集計などの後段処理が
/// 必要に応じて利用する（現状は公開のみで、本 crate 内部では未使用）。
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

/// `IllegalReason` を CSA 棋譜上のサブコードへ変換するヘルパ（拡張用の予約 API）。
pub fn illegal_reason_subcode(reason: IllegalReason) -> &'static str {
    match reason {
        IllegalReason::Generic => "ILLEGAL_MOVE",
        IllegalReason::Uchifuzume => "UCHIFUZUME",
        IllegalReason::IllegalKachi => "ILLEGAL_KACHI",
    }
}

/// 平手初期局面に CSA 手列を順に適用し、開始局面 SFEN を導出する。
pub fn initial_sfen_from_csa_moves(moves: &[CsaMoveToken]) -> Result<String, String> {
    let mut pos = initial_position();
    for mv in moves {
        pos.apply_csa_move(mv.as_str())
            .map_err(|e| format!("invalid buoy move {}: {e}", mv.as_str()))?;
    }
    Ok(pos.to_sfen())
}

/// 既存棋譜から任意手数の派生開始局面 SFEN を導出する。
///
/// 返り値の第 2 要素は実際に適用した通常手数。`nth_move = None` のときは
/// 特殊手 (`%TORYO` など) を除く全通常手を適用する。
pub fn fork_initial_sfen_from_kifu(
    csa_v2_text: &str,
    nth_move: Option<u32>,
) -> Result<(String, u32), String> {
    let (mut pos, moves, _info) =
        parse_csa_full(csa_v2_text).map_err(|e| format!("parse source kifu: {e}"))?;
    let normal_moves: Vec<_> = moves
        .into_iter()
        .filter_map(|m| match m {
            ParsedMove::Normal(mv) => Some(mv.mv),
            ParsedMove::Special(_) => None,
        })
        .collect();
    let apply_count = nth_move.unwrap_or(normal_moves.len() as u32) as usize;
    if apply_count > normal_moves.len() {
        return Err(format!(
            "nth_move {apply_count} exceeds available moves {}",
            normal_moves.len()
        ));
    }
    for mv in normal_moves.iter().take(apply_count) {
        pos.apply_csa_move(mv).map_err(|e| format!("replay move {mv}: {e}"))?;
    }
    Ok((pos.to_sfen(), apply_count as u32))
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
        // 00LIST の時刻列は `split(' ')` で単一トークンに収まる必要があるため、
        // 実呼び出し側 (`storage/file.rs::append_summary`) は ISO 8601 形式
        // `YYYY-MM-DDTHH:MM:SSZ` を渡す（CSA V2 棋譜の `$START_TIME` /
        // `$END_TIME` は `YYYY/MM/DD HH:MM:SS` で空白入り、こちらは別経路）。
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
    fn initial_sfen_from_csa_moves_applies_moves_on_hirate() {
        let sfen = initial_sfen_from_csa_moves(&[
            CsaMoveToken::new("+7776FU"),
            CsaMoveToken::new("-3334FU"),
        ])
        .unwrap();
        assert_eq!(sfen, "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3");
    }

    #[test]
    fn fork_initial_sfen_from_kifu_respects_nth_move() {
        let mut rec = rec_skeleton();
        rec.initial_position = "PI\n+\n".to_owned();
        let txt = rec.build_v2();
        let (sfen, applied) = fork_initial_sfen_from_kifu(&txt, Some(1)).unwrap();
        assert_eq!(applied, 1);
        assert_eq!(sfen, "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2");
    }

    #[test]
    fn fork_initial_sfen_from_kifu_rejects_out_of_range_nth_move() {
        let mut rec = rec_skeleton();
        rec.initial_position = "PI\n+\n".to_owned();
        let txt = rec.build_v2();
        let err = fork_initial_sfen_from_kifu(&txt, Some(3)).unwrap_err();
        assert!(err.contains("exceeds available moves"), "unexpected: {err}");
    }

    /// CSA V2 棋譜全文をバイト一致で固定するゴールデン。trailing space や区切り
    /// 文字の混入を 1 byte 単位で検出するため、既存の `contains` 系テストでは
    /// 拾えない静かな破壊を防ぐ。format を意図的に変える際は `expected` を
    /// 更新する。
    #[test]
    fn csa_v2_full_text_is_byte_stable_for_representative_record() {
        let mut rec = rec_skeleton();
        rec.initial_position = "PI\n+\n".to_owned();
        let txt = rec.build_v2();
        let expected = "\
V2.2
N+alice
N-bob
$EVENT:rshogi-csa-server-test
$GAME_ID:20140101120000
$START_TIME:2026/04/17 12:00:00
$END_TIME:2026/04/17 12:05:00
BEGIN Time
Time_Unit:1sec
Total_Time:600
Byoyomi:10
Least_Time_Per_Move:0
END Time
PI
+
+7776FU,T3
-3334FU,T4
'eval=12 pv 3c3d
%TORYO
";
        assert_eq!(txt, expected, "CSA V2 棋譜のゴールデン形式が変更されました");
    }

    /// 00LIST の 1 行を全 `GameResult` variant について完全一致で固定するゴールデン。
    /// 単一スペース 6 列のフォーマット契約 (`split(' ')` 前提) を 1 byte 単位で
    /// 守ることが消費側との契約。
    #[test]
    fn zerozero_list_format_is_byte_stable_for_all_result_variants() {
        let game_id = GameId::new("g1");
        let black = PlayerName::new("alice");
        let white = PlayerName::new("bob");
        // 00LIST の時刻列は `split(' ')` で単一トークンに収まる必要があるため、
        // 実呼び出し側は ISO 8601 形式 `YYYY-MM-DDTHH:MM:SSZ` を渡す。CSA V2
        // 棋譜の `$START_TIME` / `$END_TIME` は `YYYY/MM/DD HH:MM:SS` で空白
        // 入りだが、それは別経路なのでここではテストしない。
        let start = "2026-04-17T12:00:00Z";
        let end = "2026-04-17T12:10:00Z";

        // (variant, expected_result_code) を網羅する。新しい variant が `GameResult` に
        // 増えた場合、本テストの match と list 両方を更新する必要がある (single source
        // of truth として `primary_result_code` も同時に更新する契約)。
        // `winner` / `loser` は `primary_result_code` が無視するため片側のみ網羅する。
        let cases: Vec<(GameResult, &str)> = vec![
            (
                GameResult::Toryo {
                    winner: Color::Black,
                },
                "#RESIGN",
            ),
            (
                GameResult::TimeUp {
                    loser: Color::White,
                },
                "#TIME_UP",
            ),
            (
                GameResult::IllegalMove {
                    loser: Color::Black,
                    reason: IllegalReason::Generic,
                },
                "#ILLEGAL_MOVE",
            ),
            (
                GameResult::IllegalMove {
                    loser: Color::Black,
                    reason: IllegalReason::Uchifuzume,
                },
                "#ILLEGAL_MOVE",
            ),
            (
                GameResult::IllegalMove {
                    loser: Color::Black,
                    reason: IllegalReason::IllegalKachi,
                },
                "#ILLEGAL_MOVE",
            ),
            (
                GameResult::Kachi {
                    winner: Color::Black,
                },
                "#JISHOGI",
            ),
            (
                GameResult::OuteSennichite {
                    loser: Color::Black,
                },
                "#OUTE_SENNICHITE",
            ),
            (GameResult::Sennichite, "#SENNICHITE"),
            (GameResult::MaxMoves, "#MAX_MOVES"),
            (
                GameResult::Abnormal {
                    winner: Some(Color::Black),
                },
                "#ABNORMAL",
            ),
            (GameResult::Abnormal { winner: None }, "#ABNORMAL"),
        ];

        for (result, code) in cases {
            let line = format_zerozero_list_line(&game_id, &black, &white, start, end, &result);
            let expected = format!("g1 alice bob {start} {end} {code}");
            assert_eq!(line, expected, "00LIST 行が固定形式から外れました: {result:?}");
            // 列数 6 と区切り単一スペースを直接固定する（leading/trailing space 0、
            // タブ混入なし、改行なし）。
            assert_eq!(line.split(' ').count(), 6, "列数が 6 から逸脱: {line}");
            assert!(!line.contains('\t'), "タブ文字混入: {line}");
            assert!(!line.contains('\n'), "改行混入: {line}");
            assert!(!line.contains("  "), "連続スペース混入: {line}");
        }
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
