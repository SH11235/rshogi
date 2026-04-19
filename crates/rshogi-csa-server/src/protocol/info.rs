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
use crate::types::{CsaLine, PlayerName};

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

/// `%%HELP` に対する応答を複数行で生成する。
///
/// 応答は CSA 拡張 `##[HELP]` プレフィックス付きの行列。各行に 1 コマンドずつ
/// 概要を載せる。クライアントで GUI 補助に使える粒度にする。
pub fn help_lines() -> Vec<CsaLine> {
    let entries: &[&str] = &[
        "%%VERSION - show server implementation and version",
        "%%HELP - list available %% commands",
        "%%WHO - list logged-in players",
        "%%LIST - list active games",
        "%%SHOW <game_id> - show summary and move history of a game",
        "%%MONITOR2ON <game_id> - start spectating a game",
        "%%MONITOR2OFF <game_id> - stop spectating a game",
        "%%CHAT <message> - send a chat message to a room",
        "%%SETBUOY <game_name> <moves...> <count> - register a buoy template (admin)",
        "%%DELETEBUOY <game_name> - delete a buoy template (admin)",
        "%%GETBUOYCOUNT <game_name> - show remaining buoy slots",
        "%%FORK <source_game> [buoy_name] [nth_move] - derive a game from an existing record",
    ];
    entries.iter().map(|e| CsaLine::new(format!("##[HELP] {e}"))).collect()
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
    fn help_lines_cover_all_x1_commands() {
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
}
