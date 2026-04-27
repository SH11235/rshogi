//! x1 拡張モードの「サーバー情報系」応答生成。
//!
//! `%%VERSION` / `%%HELP` は対局状態やプレイヤ一覧に依存しない純粋な応答で、
//! ここでは [`CsaLine`] のベクタを返す純関数として提供する。フロントエンドは
//! 受信セッションが x1 フラグ付きであることを確認した上で、返ってきた行列を
//! そのまま送信する。
//!
//! 実装名・バージョン文字列は `env!("CARGO_PKG_VERSION")` から埋め込み、
//! Cargo.toml の version を唯一の情報源にする（コード内にハードコードしない）。
//!
//! 応答行は `##[<TAG>] ...` の CSA 拡張プレフィックスを採用し、クライアントが
//! 既存のサーバー応答（`LOGIN:alice OK` や `START:<game_id>` 等）と区別できる
//! ようにする。

use crate::matching::league::PlayerStatus;
use crate::matching::registry::GameListing;
use crate::port::PlayerRateRecord;
use crate::storage::floodgate_history::{FloodgateHistoryEntry, HistoryColor};
use crate::types::{CsaLine, GameId, PlayerName};

/// サーバー実装名。応答ヘッダに埋め込む固定識別子。
const SERVER_IMPL_NAME: &str = "rshogi-csa-server";

/// `%%VERSION` に対する応答行を 1 行分生成する。
///
/// 返す行は `##[VERSION] <server-name> <version>` 形式。`<version>` には
/// コア crate の Cargo.toml の version をそのまま埋める。
pub fn version_lines() -> Vec<CsaLine> {
    vec![CsaLine::new(format!(
        "##[VERSION] {} {}",
        SERVER_IMPL_NAME,
        env!("CARGO_PKG_VERSION")
    ))]
}

/// `%%WHO` に対する応答を複数行で生成する。
///
/// 引数 `players` は通常 `League::who()` の戻り値をそのまま渡す。
/// 各プレイヤに 1 行ずつ `##[WHO] <name> <status>` を出し、末尾に終端行
/// `##[WHO] END` を必ず付ける（クライアントが行列の終わりを検出できる
/// ようにするため）。出力は名前で昇順に並べ、同じ入力に対して決定論的な順序に
/// なるようにする。
///
/// `<status>` は以下の語彙:
/// - `connected`
/// - `waiting:<game_name>`
/// - `agree:<game_id>` / `start:<game_id>` / `playing:<game_id>`
/// - `finished`
///
/// 詳細な `preferred_color` や `agreed_by` は省略する（クライアント表示用に
/// 十分な粒度で、かつステータスの陳腐化を抑えるため）。
pub fn who_lines(players: &[(PlayerName, PlayerStatus)]) -> Vec<CsaLine> {
    let mut rows: Vec<(&str, String)> =
        players.iter().map(|(n, s)| (n.as_str(), format_status_token(s))).collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = Vec::with_capacity(rows.len() + 1);
    for (name, status) in rows {
        out.push(CsaLine::new(format!("##[WHO] {name} {status}")));
    }
    out.push(CsaLine::new("##[WHO] END"));
    out
}

fn format_status_token(status: &PlayerStatus) -> String {
    match status {
        PlayerStatus::Connected => "connected".to_owned(),
        PlayerStatus::GameWaiting { game_name, .. } => format!("waiting:{}", game_name.as_str()),
        PlayerStatus::AgreeWaiting { game_id } => format!("agree:{}", game_id.as_str()),
        PlayerStatus::StartWaiting { game_id } => format!("start:{}", game_id.as_str()),
        PlayerStatus::InGame { game_id } => format!("playing:{}", game_id.as_str()),
        PlayerStatus::Finished => "finished".to_owned(),
    }
}

/// `%%LIST` に対する応答を複数行で生成する。
///
/// 引数 `games` は [`crate::matching::registry::GameRegistry::snapshot`] の
/// 戻り値をそのまま渡す（呼び出し側で `game_id` 昇順にソート済み）。
/// 各対局に 1 行ずつ `##[LIST] <game_id> <black> <white> <game_name> <started_at>`、
/// 末尾に終端行 `##[LIST] END` を付ける。
pub fn list_lines(games: &[GameListing]) -> Vec<CsaLine> {
    let mut out = Vec::with_capacity(games.len() + 1);
    for g in games {
        out.push(CsaLine::new(format!(
            "##[LIST] {} {} {} {} {}",
            g.game_id.as_str(),
            g.black.as_str(),
            g.white.as_str(),
            g.game_name.as_str(),
            g.started_at,
        )));
    }
    out.push(CsaLine::new("##[LIST] END"));
    out
}

/// `%%SHOW <game_id>` に対する応答を生成する。
///
/// `listing` が `Some` なら対局サマリを `##[SHOW] <field> <value>` 群として
/// 出力する。`None`（未登録 game_id）なら `##[SHOW] NOT_FOUND <game_id>` を
/// 先頭に出す。どちらの分岐でも末尾には終端行 `##[SHOW] END` を必ず付ける
/// （persistent socket 上で「END まで読む」クライアントが missing game_id で
/// 無限待ちにならないよう、framing を `%%WHO` / `%%LIST` / `%%HELP` と揃える）。
///
/// 指し手列の添付は本関数のスコープ外（`GameRoom` から別途取得して
/// `show_lines_with_moves` に差し替え拡張する想定）。
pub fn show_lines(game_id: &GameId, listing: Option<&GameListing>) -> Vec<CsaLine> {
    let Some(g) = listing else {
        return vec![
            CsaLine::new(format!("##[SHOW] NOT_FOUND {}", game_id.as_str())),
            CsaLine::new("##[SHOW] END"),
        ];
    };
    vec![
        CsaLine::new(format!("##[SHOW] game_id {}", g.game_id.as_str())),
        CsaLine::new(format!("##[SHOW] black {}", g.black.as_str())),
        CsaLine::new(format!("##[SHOW] white {}", g.white.as_str())),
        CsaLine::new(format!("##[SHOW] game_name {}", g.game_name.as_str())),
        CsaLine::new(format!("##[SHOW] started_at {}", g.started_at)),
        CsaLine::new("##[SHOW] END"),
    ]
}

/// `%%HELP` に対する応答を複数行で生成する。
///
/// 応答は CSA 拡張 `##[HELP]` プレフィックス付きの行列 + 末尾に終端行
/// `##[HELP] END` を必ず付ける。**このリストは実際に受け付けるコマンドだけを
/// 含める** (advertise ≠ accept の乖離を防ぐため)。未配線の `%%FORK` 系は、
/// 各コマンドの配線コミットで順次追加する。
///
/// 終端行があることで、persistent socket 上でクライアントは「HELP 応答が何行
/// 続くか」を事前に知らずに次コマンド送信に進める（`%%WHO` / `%%LIST` /
/// `%%SHOW` と同じ framing 規約）。
pub fn help_lines() -> Vec<CsaLine> {
    let entries: &[&str] = &[
        "%%VERSION - show server implementation and version",
        "%%HELP - list available %% commands",
        "%%WHO - list logged-in players",
        "%%LIST - list active games",
        "%%SHOW <game_id> - show a game summary",
        "%%MONITOR2ON <game_id> - subscribe to a game as a spectator \
(the session leaves matchmaking; re-LOGIN to resume)",
        "%%MONITOR2OFF <game_id> - unsubscribe from a game (stays observer-only; \
re-LOGIN to return to matchmaking)",
        "%%CHAT <message> - broadcast a chat message to spectators of the monitored game",
        "%%SETBUOY <game_name> <moves> <count> - register a buoy (admin only)",
        "%%DELETEBUOY <game_name> - delete a buoy (admin only)",
        "%%GETBUOYCOUNT <game_name> - query remaining count of a buoy",
        "%%FORK <source_game> [buoy_name] [nth_move] - derive a buoy from an existing game",
        "%%FLOODGATE history [N] - list the most recent N floodgate game results (default 10)",
        "%%FLOODGATE rating <handle> - show stored rate / wins / losses for one handle",
    ];
    let mut out: Vec<CsaLine> =
        entries.iter().map(|e| CsaLine::new(format!("##[HELP] {e}"))).collect();
    out.push(CsaLine::new("##[HELP] END"));
    out
}

/// `%%FLOODGATE history [N]` に対する応答行を生成する。
///
/// `entries` は [`crate::FloodgateHistoryStorage::list_recent`] の戻り値をその
/// ままの順序（新しい順）で渡す。各 entry を 1 行 `##[FLOODGATE] history
/// <game_id> <game_name> <black> <white> <result_code> <winner> <start_time> <end_time>`
/// として出し、末尾に `##[FLOODGATE] history END` を必ず付ける。空応答でも終端
/// 行は出すため、persistent socket 上でクライアントは「END まで読む」契約で
/// 安全に framing できる。
///
/// `<winner>` は `Black` / `White` / `-`（千日手・最大手数等で勝者不確定）の
/// いずれか。プレイヤ名や game_id にスペースが混入すると行 framing が壊れるが、
/// 既存の CSA `LOGIN` / [`FloodgateHistoryEntry`] の入力契約上 ASCII printable
/// (空白を含まない) のみを許容しているため運用上問題にならない。
pub fn floodgate_history_lines(entries: &[FloodgateHistoryEntry]) -> Vec<CsaLine> {
    // 行 framing は 1 entry = 1 行 (空白区切り) 契約。各フィールドに空白が混入すると
    // 受信側のトークン分解が壊れる。CSA LOGIN / `FloodgateHistoryEntry::new` の入力
    // 契約上は ASCII printable (空白なし) のみを通す前提だが、上流契約の退行を debug
    // ビルドで早期検出できるよう assert する (release はゼロコスト)。
    debug_assert!(
        entries.iter().all(|e| {
            !e.game_id.contains(' ')
                && !e.game_name.contains(' ')
                && !e.black.contains(' ')
                && !e.white.contains(' ')
                && !e.result_code.contains(' ')
                && !e.start_time.contains(' ')
                && !e.end_time.contains(' ')
        }),
        "FloodgateHistoryEntry fields must not contain ASCII whitespace; \
         line framing in `##[FLOODGATE] history` would break otherwise"
    );

    let mut out = Vec::with_capacity(entries.len() + 1);
    for e in entries {
        let winner = match e.winner {
            Some(HistoryColor::Black) => "Black",
            Some(HistoryColor::White) => "White",
            None => "-",
        };
        out.push(CsaLine::new(format!(
            "##[FLOODGATE] history {} {} {} {} {} {} {} {}",
            e.game_id,
            e.game_name,
            e.black,
            e.white,
            e.result_code,
            winner,
            e.start_time,
            e.end_time
        )));
    }
    out.push(CsaLine::new("##[FLOODGATE] history END"));
    out
}

/// `%%FLOODGATE rating <handle>` に対する応答行を生成する。
///
/// `record` が `Some` なら rate / wins / losses / last_game_id / last_modified を
/// `##[FLOODGATE] rating <handle> <rate> <wins> <losses> <last_game_id> <last_modified>`
/// で 1 行返す。`last_game_id` 未設定 (`None`) は `-` で埋める。
/// `record` が `None`（未登録ハンドル）の場合は `##[FLOODGATE] rating NOT_FOUND
/// <handle>` を返す。どちらの分岐でも末尾に `##[FLOODGATE] rating END` を付け、
/// `##[FLOODGATE] history` と同じ framing 契約に揃える。
pub fn floodgate_rating_lines(
    handle: &PlayerName,
    record: Option<&PlayerRateRecord>,
) -> Vec<CsaLine> {
    // `floodgate_history_lines` と同じ行 framing 契約。`PlayerName` / `last_modified` /
    // `last_game_id` の上流契約も「空白を含まない」前提だが、debug ビルドで退行検出
    // できるようにしておく。
    debug_assert!(
        !handle.as_str().contains(' '),
        "handle must not contain ASCII whitespace; line framing would break"
    );
    if let Some(r) = record {
        debug_assert!(
            !r.name.as_str().contains(' ')
                && !r.last_modified.contains(' ')
                && r.last_game_id.as_ref().is_none_or(|g| !g.as_str().contains(' ')),
            "PlayerRateRecord fields must not contain ASCII whitespace"
        );
    }

    let head = match record {
        Some(r) => {
            let last_game_id = r.last_game_id.as_ref().map_or("-", GameId::as_str);
            CsaLine::new(format!(
                "##[FLOODGATE] rating {} {} {} {} {} {}",
                r.name, r.rate, r.wins, r.losses, last_game_id, r.last_modified
            ))
        }
        None => CsaLine::new(format!("##[FLOODGATE] rating NOT_FOUND {handle}")),
    };
    vec![head, CsaLine::new("##[FLOODGATE] rating END")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_has_expected_prefix_and_package_version() {
        let lines = version_lines();
        assert_eq!(lines.len(), 1);
        let s = lines[0].as_str();
        assert!(s.starts_with("##[VERSION] "), "unexpected prefix: {s}");
        assert!(s.contains(SERVER_IMPL_NAME), "impl name missing: {s}");
        // Cargo.toml の version をそのまま末尾に埋める契約。
        assert!(s.ends_with(env!("CARGO_PKG_VERSION")), "version missing: {s}");
    }

    #[test]
    fn help_lines_cover_currently_wired_commands() {
        // HELP は「実際に受け付けるコマンドだけを advertise する」方針。
        let lines = help_lines();
        let joined: String =
            lines.iter().map(|l| l.as_str().to_owned()).collect::<Vec<_>>().join("\n");
        for cmd in [
            "%%VERSION",
            "%%HELP",
            "%%WHO",
            "%%LIST",
            "%%SHOW",
            "%%MONITOR2ON",
            "%%MONITOR2OFF",
            "%%CHAT",
            "%%SETBUOY",
            "%%DELETEBUOY",
            "%%GETBUOYCOUNT",
            "%%FORK",
            "%%FLOODGATE history",
            "%%FLOODGATE rating",
        ] {
            assert!(joined.contains(cmd), "help missing {cmd}: {joined}");
        }
    }

    #[test]
    fn help_lines_all_use_help_prefix() {
        for line in help_lines() {
            assert!(line.as_str().starts_with("##[HELP] "), "bad prefix: {}", line.as_str());
        }
    }

    #[test]
    fn help_lines_end_with_terminator() {
        // WHO / LIST / SHOW と揃えて HELP も終端行を持つ。
        let lines = help_lines();
        assert_eq!(lines.last().map(|l| l.as_str()), Some("##[HELP] END"));
    }

    #[test]
    fn who_lines_sorted_by_name_and_terminated() {
        use crate::types::{GameId, GameName};
        let players = vec![
            (PlayerName::new("carol"), PlayerStatus::Connected),
            (
                PlayerName::new("alice"),
                PlayerStatus::GameWaiting {
                    game_name: GameName::new("floodgate-600-10"),
                    preferred_color: None,
                },
            ),
            (
                PlayerName::new("bob"),
                PlayerStatus::InGame {
                    game_id: GameId::new("20140101-0001"),
                },
            ),
        ];
        let lines: Vec<String> = who_lines(&players).into_iter().map(|l| l.into_string()).collect();
        assert_eq!(
            lines,
            vec![
                "##[WHO] alice waiting:floodgate-600-10".to_owned(),
                "##[WHO] bob playing:20140101-0001".to_owned(),
                "##[WHO] carol connected".to_owned(),
                "##[WHO] END".to_owned(),
            ]
        );
    }

    #[test]
    fn who_lines_empty_still_has_terminator() {
        let lines = who_lines(&[]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].as_str(), "##[WHO] END");
    }

    fn sample_listing(gid: &str) -> GameListing {
        use crate::types::GameName;
        GameListing {
            game_id: GameId::new(gid),
            black: PlayerName::new("alice"),
            white: PlayerName::new("bob"),
            game_name: GameName::new("floodgate-600-10"),
            started_at: "2026-04-17T12:00:00Z".to_owned(),
        }
    }

    #[test]
    fn list_lines_include_all_fields_and_terminator() {
        let games = vec![sample_listing("g-1"), sample_listing("g-2")];
        let lines: Vec<String> = list_lines(&games).into_iter().map(|l| l.into_string()).collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "##[LIST] g-1 alice bob floodgate-600-10 2026-04-17T12:00:00Z");
        assert_eq!(lines[1], "##[LIST] g-2 alice bob floodgate-600-10 2026-04-17T12:00:00Z");
        assert_eq!(lines[2], "##[LIST] END");
    }

    #[test]
    fn list_lines_empty_is_just_terminator() {
        let lines = list_lines(&[]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].as_str(), "##[LIST] END");
    }

    #[test]
    fn show_lines_for_known_game_emits_field_lines_and_terminator() {
        let g = sample_listing("g-1");
        let lines: Vec<String> = show_lines(&GameId::new("g-1"), Some(&g))
            .into_iter()
            .map(|l| l.into_string())
            .collect();
        assert_eq!(
            lines,
            vec![
                "##[SHOW] game_id g-1".to_owned(),
                "##[SHOW] black alice".to_owned(),
                "##[SHOW] white bob".to_owned(),
                "##[SHOW] game_name floodgate-600-10".to_owned(),
                "##[SHOW] started_at 2026-04-17T12:00:00Z".to_owned(),
                "##[SHOW] END".to_owned(),
            ]
        );
    }

    #[test]
    fn show_lines_for_unknown_game_emits_not_found_then_terminator() {
        let lines = show_lines(&GameId::new("g-missing"), None);
        assert_eq!(
            lines.iter().map(|l| l.as_str().to_owned()).collect::<Vec<_>>(),
            vec![
                "##[SHOW] NOT_FOUND g-missing".to_owned(),
                "##[SHOW] END".to_owned(),
            ]
        );
    }

    #[test]
    fn who_lines_cover_all_status_variants() {
        use crate::types::{GameId, GameName};
        let players = vec![
            (
                PlayerName::new("a"),
                PlayerStatus::GameWaiting {
                    game_name: GameName::new("g"),
                    preferred_color: None,
                },
            ),
            (
                PlayerName::new("b"),
                PlayerStatus::AgreeWaiting {
                    game_id: GameId::new("x"),
                },
            ),
            (
                PlayerName::new("c"),
                PlayerStatus::StartWaiting {
                    game_id: GameId::new("x"),
                },
            ),
            (
                PlayerName::new("d"),
                PlayerStatus::InGame {
                    game_id: GameId::new("x"),
                },
            ),
            (PlayerName::new("e"), PlayerStatus::Finished),
            (PlayerName::new("f"), PlayerStatus::Connected),
        ];
        let lines: Vec<String> = who_lines(&players).into_iter().map(|l| l.into_string()).collect();
        assert!(lines.contains(&"##[WHO] a waiting:g".to_owned()));
        assert!(lines.contains(&"##[WHO] b agree:x".to_owned()));
        assert!(lines.contains(&"##[WHO] c start:x".to_owned()));
        assert!(lines.contains(&"##[WHO] d playing:x".to_owned()));
        assert!(lines.contains(&"##[WHO] e finished".to_owned()));
        assert!(lines.contains(&"##[WHO] f connected".to_owned()));
    }

    fn sample_history_entry(game_id: &str, winner: Option<HistoryColor>) -> FloodgateHistoryEntry {
        FloodgateHistoryEntry {
            game_id: game_id.to_owned(),
            game_name: "floodgate-600-10".to_owned(),
            black: "alice".to_owned(),
            white: "bob".to_owned(),
            start_time: "2026-04-26T12:00:00Z".to_owned(),
            end_time: "2026-04-26T12:30:00Z".to_owned(),
            result_code: "#RESIGN".to_owned(),
            winner,
        }
    }

    #[test]
    fn floodgate_history_lines_emit_one_row_per_entry_then_terminator() {
        let entries = vec![
            sample_history_entry("g-1", Some(HistoryColor::White)),
            sample_history_entry("g-2", None),
        ];
        let lines: Vec<String> =
            floodgate_history_lines(&entries).into_iter().map(|l| l.into_string()).collect();
        assert_eq!(
            lines,
            vec![
                "##[FLOODGATE] history g-1 floodgate-600-10 alice bob #RESIGN White \
                 2026-04-26T12:00:00Z 2026-04-26T12:30:00Z"
                    .to_owned(),
                "##[FLOODGATE] history g-2 floodgate-600-10 alice bob #RESIGN - \
                 2026-04-26T12:00:00Z 2026-04-26T12:30:00Z"
                    .to_owned(),
                "##[FLOODGATE] history END".to_owned(),
            ]
        );
    }

    #[test]
    fn floodgate_history_lines_empty_yields_only_terminator() {
        let lines = floodgate_history_lines(&[]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].as_str(), "##[FLOODGATE] history END");
    }

    fn sample_rate_record(name: &str, last_game_id: Option<&str>) -> PlayerRateRecord {
        PlayerRateRecord {
            name: PlayerName::new(name),
            rate: 1500,
            wins: 10,
            losses: 7,
            last_game_id: last_game_id.map(GameId::new),
            last_modified: "2026-04-26T12:30:00Z".to_owned(),
        }
    }

    #[test]
    fn floodgate_rating_lines_for_known_handle_emits_record_then_terminator() {
        let record = sample_rate_record("alice", Some("20260426-0001"));
        let handle = PlayerName::new("alice");
        let lines: Vec<String> = floodgate_rating_lines(&handle, Some(&record))
            .into_iter()
            .map(|l| l.into_string())
            .collect();
        assert_eq!(
            lines,
            vec![
                "##[FLOODGATE] rating alice 1500 10 7 20260426-0001 2026-04-26T12:30:00Z"
                    .to_owned(),
                "##[FLOODGATE] rating END".to_owned(),
            ]
        );
    }

    #[test]
    fn floodgate_rating_lines_omits_last_game_id_when_absent() {
        let record = sample_rate_record("bob", None);
        let handle = PlayerName::new("bob");
        let lines: Vec<String> = floodgate_rating_lines(&handle, Some(&record))
            .into_iter()
            .map(|l| l.into_string())
            .collect();
        assert_eq!(
            lines,
            vec![
                "##[FLOODGATE] rating bob 1500 10 7 - 2026-04-26T12:30:00Z".to_owned(),
                "##[FLOODGATE] rating END".to_owned(),
            ]
        );
    }

    #[test]
    fn floodgate_rating_lines_for_unknown_handle_emits_not_found() {
        let handle = PlayerName::new("ghost");
        let lines: Vec<String> = floodgate_rating_lines(&handle, None)
            .into_iter()
            .map(|l| l.into_string())
            .collect();
        assert_eq!(
            lines,
            vec![
                "##[FLOODGATE] rating NOT_FOUND ghost".to_owned(),
                "##[FLOODGATE] rating END".to_owned(),
            ]
        );
    }
}
