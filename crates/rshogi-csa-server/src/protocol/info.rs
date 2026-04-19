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

use crate::types::CsaLine;

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
}
