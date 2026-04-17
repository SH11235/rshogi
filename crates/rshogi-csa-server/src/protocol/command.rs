//! クライアントから受信した 1 行を [`ClientCommand`] に構造化する。
//!
//! CSA プロトコル v1.2.1 の標準コマンドに加え、x1 拡張の `%%` 系コマンド構文も認識する。
//! 意味検証（状態機械整合・合法手判定）は上位層（[`crate::matching::league::League`]、
//! [`crate::game`]）で行う。

use crate::error::ProtocolError;
use crate::types::{Color, CsaLine, CsaMoveToken, GameId, GameName, PlayerName, Secret};

/// クライアントから到着し得るコマンド一覧。
///
/// 非公開フィールドは持たず、パターンマッチによるルーティングが容易な `enum`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientCommand {
    /// `LOGIN <name> <password> [x1]`
    Login {
        /// プレイヤ名。
        name: PlayerName,
        /// パスワード（平文）。マスクして扱う。
        password: Secret,
        /// x1 拡張モードを要求するか。
        x1: bool,
    },
    /// `LOGOUT`
    Logout,
    /// `AGREE [game_id]`
    Agree {
        /// 省略可能な対局 ID。省略時は `None`。
        game_id: Option<GameId>,
    },
    /// `REJECT [game_id]`
    Reject {
        /// 省略可能な対局 ID。
        game_id: Option<GameId>,
    },
    /// 指し手（例: `+7776FU`, `-3334FU,T3` 等）。
    Move {
        /// CSA 手トークン（`+7776FU` 等）。
        token: CsaMoveToken,
        /// `'` で始まる追記コメント（Floodgate 評価値 PV 等）。
        comment: Option<String>,
    },
    /// `%TORYO`
    Toryo,
    /// `%KACHI`
    Kachi,
    /// `%CHUDAN`
    Chudan,
    /// 空行（keep-alive）。
    KeepAlive,

    // --- x1 拡張 ---
    /// `%%WHO`
    Who,
    /// `%%LIST`
    List,
    /// `%%SHOW <game_id>`
    Show {
        /// 表示対象の対局 ID。
        game_id: GameId,
    },
    /// `%%MONITOR2ON <game_id>`
    Monitor2On {
        /// 観戦対象の対局 ID。
        game_id: GameId,
    },
    /// `%%MONITOR2OFF <game_id>`
    Monitor2Off {
        /// 観戦解除対象の対局 ID。
        game_id: GameId,
    },
    /// `%%CHAT <message>`
    Chat {
        /// メッセージ本文（先頭スペース除去済み）。
        message: String,
    },
    /// `%%VERSION`
    Version,
    /// `%%HELP`
    Help,
    /// `%%SETBUOY <game_name> <moves> <count>`（運営権限が必要）。
    ///
    /// 構文木には権限情報を持たない。運営権限判定は呼び出し側（[`crate::matching::league::League`]
    /// とセッション情報）が担う（Requirement 7.10, 7.12）。
    SetBuoy {
        /// 登録先 game_name。
        game_name: GameName,
        /// 初期局面に差し込む CSA 手列。
        moves: Vec<CsaMoveToken>,
        /// 残り対局数。
        count: u32,
    },
    /// `%%DELETEBUOY <game_name>`（運営権限が必要）。
    DeleteBuoy {
        /// 削除対象の game_name。
        game_name: GameName,
    },
    /// `%%GETBUOYCOUNT <game_name>`
    GetBuoyCount {
        /// 対象 game_name。
        game_name: GameName,
    },
    /// `%%FORK <source_game> [buoy_name] [nth_move]`
    Fork {
        /// 派生元の対局 ID。
        source_game: GameId,
        /// 新規ブイ名（任意）。
        new_buoy: Option<GameName>,
        /// 何手目からフォークするか（任意）。
        nth_move: Option<u32>,
    },
}

/// 1 行の生 CSA テキストをパースして [`ClientCommand`] に変換する。
///
/// 行末改行コード（`\r` / `\n`）は呼び出し側で除去済みであることを前提にする
/// （[`CsaLine`] 型の契約）。
pub fn parse_command(line: &CsaLine) -> Result<ClientCommand, ProtocolError> {
    let raw = line.as_str();

    // keep-alive は空行で判定（trim 前）。
    if raw.trim().is_empty() {
        return Ok(ClientCommand::KeepAlive);
    }

    let trimmed = raw.trim_end();

    // CSA 手（先手 `+`、後手 `-`）
    if let Some(first) = trimmed.chars().next()
        && (first == '+' || first == '-')
    {
        return parse_move(trimmed);
    }

    // 特殊コマンド / 拡張コマンド
    if let Some(rest) = trimmed.strip_prefix("%%") {
        return parse_x1(rest);
    }

    if let Some(rest) = trimmed.strip_prefix('%') {
        return match rest {
            "TORYO" => Ok(ClientCommand::Toryo),
            "KACHI" => Ok(ClientCommand::Kachi),
            "CHUDAN" => Ok(ClientCommand::Chudan),
            other => Err(ProtocolError::Unknown(format!("%{other}"))),
        };
    }

    // 標準コマンド
    let mut parts = trimmed.split_whitespace();
    let head = parts.next().unwrap_or("");
    match head {
        "LOGIN" => {
            let name = parts
                .next()
                .ok_or_else(|| ProtocolError::Malformed("LOGIN: missing name".into()))?;
            let password = parts
                .next()
                .ok_or_else(|| ProtocolError::Malformed("LOGIN: missing password".into()))?;
            let x1 = match parts.next() {
                None => false,
                Some("x1") => true,
                Some(extra) => {
                    return Err(ProtocolError::Malformed(format!(
                        "LOGIN: unexpected trailing token `{extra}`"
                    )));
                }
            };
            if parts.next().is_some() {
                return Err(ProtocolError::Malformed("LOGIN: too many trailing tokens".into()));
            }
            Ok(ClientCommand::Login {
                name: PlayerName::new(name),
                password: Secret::new(password),
                x1,
            })
        }
        "LOGOUT" => {
            if parts.next().is_some() {
                return Err(ProtocolError::Malformed("LOGOUT: unexpected trailing tokens".into()));
            }
            Ok(ClientCommand::Logout)
        }
        "AGREE" => {
            let game_id = parts.next().map(GameId::new);
            if parts.next().is_some() {
                return Err(ProtocolError::Malformed("AGREE: unexpected trailing tokens".into()));
            }
            Ok(ClientCommand::Agree { game_id })
        }
        "REJECT" => {
            let game_id = parts.next().map(GameId::new);
            if parts.next().is_some() {
                return Err(ProtocolError::Malformed("REJECT: unexpected trailing tokens".into()));
            }
            Ok(ClientCommand::Reject { game_id })
        }
        other => Err(ProtocolError::Unknown(other.to_owned())),
    }
}

fn parse_move(line: &str) -> Result<ClientCommand, ProtocolError> {
    // `+7776FU,T3'comment...` のような形式を想定。先頭 7 文字がトークン部。
    // 最短 7 文字（符号 + 4 数字 + 2 駒種）未満なら malformed。
    if line.len() < 7 {
        return Err(ProtocolError::Malformed(format!("move token too short: {line}")));
    }

    let mut rest = line;
    let (token_str, after) = match rest.split_once(',') {
        Some((tok, after)) => (tok, after),
        None => {
            // カンマなし（最低限 +7776FU のみ）
            let token = CsaMoveToken::new(rest);
            return Ok(ClientCommand::Move {
                token,
                comment: None,
            });
        }
    };
    // after はタイムフィールド `T<sec>` とコメント `'...` が続き得る。
    rest = after;

    // コメントは最初の `'` 以降を採用（Floodgate 拡張）。
    let comment = rest.split_once('\'').map(|(_, c)| c.to_owned());

    Ok(ClientCommand::Move {
        token: CsaMoveToken::new(token_str),
        comment,
    })
}

fn parse_x1(rest: &str) -> Result<ClientCommand, ProtocolError> {
    // `rest` は `%%` の後続部分。先頭トークンで分岐。
    let mut parts = rest.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let tail = parts.next().unwrap_or("").trim_start();

    match head {
        "WHO" => {
            require_no_tail(tail, "%%WHO")?;
            Ok(ClientCommand::Who)
        }
        "LIST" => {
            require_no_tail(tail, "%%LIST")?;
            Ok(ClientCommand::List)
        }
        "VERSION" => {
            require_no_tail(tail, "%%VERSION")?;
            Ok(ClientCommand::Version)
        }
        "HELP" => {
            require_no_tail(tail, "%%HELP")?;
            Ok(ClientCommand::Help)
        }
        "SHOW" => {
            let id = single_token(tail, "%%SHOW", "game_id")?;
            Ok(ClientCommand::Show {
                game_id: GameId::new(id),
            })
        }
        "MONITOR2ON" => {
            let id = single_token(tail, "%%MONITOR2ON", "game_id")?;
            Ok(ClientCommand::Monitor2On {
                game_id: GameId::new(id),
            })
        }
        "MONITOR2OFF" => {
            let id = single_token(tail, "%%MONITOR2OFF", "game_id")?;
            Ok(ClientCommand::Monitor2Off {
                game_id: GameId::new(id),
            })
        }
        "CHAT" => Ok(ClientCommand::Chat {
            message: tail.to_owned(),
        }),
        "SETBUOY" => {
            // game_name moves count。moves はスペース区切り。
            let mut toks = tail.split_whitespace().collect::<Vec<_>>();
            if toks.len() < 3 {
                return Err(ProtocolError::Malformed(
                    "%%SETBUOY: expected <game_name> <moves> <count>".into(),
                ));
            }
            let count: u32 = toks
                .pop()
                .unwrap()
                .parse()
                .map_err(|e| ProtocolError::Malformed(format!("%%SETBUOY: bad count ({e})")))?;
            let game_name = GameName::new(toks.remove(0));
            let moves = toks.into_iter().map(CsaMoveToken::new).collect();
            Ok(ClientCommand::SetBuoy {
                game_name,
                moves,
                count,
            })
        }
        "DELETEBUOY" => {
            let g = single_token(tail, "%%DELETEBUOY", "game_name")?;
            Ok(ClientCommand::DeleteBuoy {
                game_name: GameName::new(g),
            })
        }
        "GETBUOYCOUNT" => {
            let g = single_token(tail, "%%GETBUOYCOUNT", "game_name")?;
            Ok(ClientCommand::GetBuoyCount {
                game_name: GameName::new(g),
            })
        }
        "FORK" => {
            let mut toks = tail.split_whitespace();
            let src = toks
                .next()
                .ok_or_else(|| ProtocolError::Malformed("%%FORK: missing source_game".into()))?;
            let buoy = toks.next().map(GameName::new);
            let nth = match toks.next() {
                Some(s) => Some(
                    s.parse()
                        .map_err(|e| ProtocolError::Malformed(format!("%%FORK: bad nth ({e})")))?,
                ),
                None => None,
            };
            if toks.next().is_some() {
                return Err(ProtocolError::Malformed("%%FORK: unexpected trailing tokens".into()));
            }
            Ok(ClientCommand::Fork {
                source_game: GameId::new(src),
                new_buoy: buoy,
                nth_move: nth,
            })
        }
        other => Err(ProtocolError::Unknown(format!("%%{other}"))),
    }
}

/// 末尾トークン許容しないコマンド（`%%WHO` など）の余剰検出。
fn require_no_tail(tail: &str, cmd: &str) -> Result<(), ProtocolError> {
    if tail.is_empty() {
        Ok(())
    } else {
        Err(ProtocolError::Malformed(format!("{cmd}: unexpected trailing tokens")))
    }
}

/// 末尾に単一トークンのみを要求するコマンド（`%%SHOW <game_id>` 等）の検査。
fn single_token(tail: &str, cmd: &str, field: &str) -> Result<String, ProtocolError> {
    let mut toks = tail.split_whitespace();
    let value = toks
        .next()
        .ok_or_else(|| ProtocolError::Malformed(format!("{cmd}: missing {field}")))?;
    if toks.next().is_some() {
        return Err(ProtocolError::Malformed(format!("{cmd}: unexpected trailing tokens")));
    }
    Ok(value.to_owned())
}

/// 指し手トークンから手番色を判定する。
///
/// CSA の指し手は先頭 1 文字で手番（先手 `+` / 後手 `-`）が明示される。
pub fn color_of_move(token: &CsaMoveToken) -> Option<Color> {
    match token.as_str().chars().next() {
        Some('+') => Some(Color::Black),
        Some('-') => Some(Color::White),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> CsaLine {
        CsaLine::new(s)
    }

    #[test]
    fn parses_login_basic() {
        let cmd = parse_command(&line("LOGIN alice pw")).unwrap();
        assert_eq!(
            cmd,
            ClientCommand::Login {
                name: PlayerName::new("alice"),
                password: Secret::new("pw"),
                x1: false,
            }
        );
    }

    #[test]
    fn parses_login_x1() {
        let cmd = parse_command(&line("LOGIN bob secret x1")).unwrap();
        let ClientCommand::Login { x1, .. } = cmd else {
            panic!("expected Login");
        };
        assert!(x1);
    }

    #[test]
    fn parses_login_missing_password() {
        let err = parse_command(&line("LOGIN alice")).unwrap_err();
        assert!(matches!(err, ProtocolError::Malformed(_)));
    }

    #[test]
    fn parses_logout_and_agree_reject() {
        assert_eq!(parse_command(&line("LOGOUT")).unwrap(), ClientCommand::Logout);
        assert_eq!(parse_command(&line("AGREE")).unwrap(), ClientCommand::Agree { game_id: None });
        assert_eq!(
            parse_command(&line("AGREE 20140101123000")).unwrap(),
            ClientCommand::Agree {
                game_id: Some(GameId::new("20140101123000"))
            }
        );
        assert_eq!(
            parse_command(&line("REJECT")).unwrap(),
            ClientCommand::Reject { game_id: None }
        );
    }

    #[test]
    fn parses_special_moves() {
        assert_eq!(parse_command(&line("%TORYO")).unwrap(), ClientCommand::Toryo);
        assert_eq!(parse_command(&line("%KACHI")).unwrap(), ClientCommand::Kachi);
        assert_eq!(parse_command(&line("%CHUDAN")).unwrap(), ClientCommand::Chudan);
    }

    #[test]
    fn parses_keep_alive_as_blank_line() {
        assert_eq!(parse_command(&line("")).unwrap(), ClientCommand::KeepAlive);
        assert_eq!(parse_command(&line("   ")).unwrap(), ClientCommand::KeepAlive);
    }

    #[test]
    fn parses_bare_move() {
        let cmd = parse_command(&line("+7776FU")).unwrap();
        assert_eq!(
            cmd,
            ClientCommand::Move {
                token: CsaMoveToken::new("+7776FU"),
                comment: None,
            }
        );
    }

    #[test]
    fn parses_move_with_time_and_comment() {
        let cmd = parse_command(&line("+7776FU,T3'* 123 7g7f 3c3d")).unwrap();
        match cmd {
            ClientCommand::Move { token, comment } => {
                assert_eq!(token.as_str(), "+7776FU");
                assert_eq!(comment.as_deref(), Some("* 123 7g7f 3c3d"));
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn parses_x1_who_and_help() {
        assert_eq!(parse_command(&line("%%WHO")).unwrap(), ClientCommand::Who);
        assert_eq!(parse_command(&line("%%HELP")).unwrap(), ClientCommand::Help);
        assert_eq!(parse_command(&line("%%VERSION")).unwrap(), ClientCommand::Version);
    }

    #[test]
    fn parses_x1_show_and_monitor() {
        assert_eq!(
            parse_command(&line("%%SHOW 20140101")).unwrap(),
            ClientCommand::Show {
                game_id: GameId::new("20140101")
            }
        );
        assert_eq!(
            parse_command(&line("%%MONITOR2ON 20140101")).unwrap(),
            ClientCommand::Monitor2On {
                game_id: GameId::new("20140101")
            }
        );
        assert_eq!(
            parse_command(&line("%%MONITOR2OFF 20140101")).unwrap(),
            ClientCommand::Monitor2Off {
                game_id: GameId::new("20140101")
            }
        );
    }

    #[test]
    fn parses_setbuoy_and_deletebuoy_and_count() {
        let cmd = parse_command(&line("%%SETBUOY buoy1 +7776FU -3334FU 5")).unwrap();
        match cmd {
            ClientCommand::SetBuoy {
                game_name,
                moves,
                count,
                ..
            } => {
                assert_eq!(game_name.as_str(), "buoy1");
                assert_eq!(moves.len(), 2);
                assert_eq!(count, 5);
            }
            other => panic!("unexpected: {:?}", other),
        }
        assert_eq!(
            parse_command(&line("%%DELETEBUOY buoy1")).unwrap(),
            ClientCommand::DeleteBuoy {
                game_name: GameName::new("buoy1"),
            }
        );
        assert_eq!(
            parse_command(&line("%%GETBUOYCOUNT buoy1")).unwrap(),
            ClientCommand::GetBuoyCount {
                game_name: GameName::new("buoy1")
            }
        );
    }

    #[test]
    fn rejects_unknown_command() {
        let err = parse_command(&line("NOSUCH")).unwrap_err();
        assert!(matches!(err, ProtocolError::Unknown(_)));
    }

    #[test]
    fn rejects_extra_tokens_on_standard_commands() {
        assert!(matches!(
            parse_command(&line("LOGIN alice pw garbage")).unwrap_err(),
            ProtocolError::Malformed(_)
        ));
        assert!(matches!(
            parse_command(&line("LOGIN alice pw x1 garbage")).unwrap_err(),
            ProtocolError::Malformed(_)
        ));
        assert!(matches!(
            parse_command(&line("LOGOUT extra")).unwrap_err(),
            ProtocolError::Malformed(_)
        ));
        assert!(matches!(
            parse_command(&line("AGREE g1 extra")).unwrap_err(),
            ProtocolError::Malformed(_)
        ));
        assert!(matches!(
            parse_command(&line("REJECT g1 extra")).unwrap_err(),
            ProtocolError::Malformed(_)
        ));
    }

    #[test]
    fn rejects_move_token_too_short() {
        let err = parse_command(&line("+776FU")).unwrap_err();
        assert!(matches!(err, ProtocolError::Malformed(_)));
    }

    #[test]
    fn rejects_extra_tokens_on_x1_commands() {
        for cmd in [
            "%%WHO extra",
            "%%LIST extra",
            "%%VERSION extra",
            "%%HELP extra",
            "%%SHOW g1 extra",
            "%%MONITOR2ON g1 extra",
            "%%MONITOR2OFF g1 extra",
            "%%DELETEBUOY b1 extra",
            "%%GETBUOYCOUNT b1 extra",
            "%%FORK g1 b1 5 extra",
        ] {
            let err = parse_command(&line(cmd)).unwrap_err();
            assert!(
                matches!(err, ProtocolError::Malformed(_)),
                "expected Malformed for `{cmd}`, got {:?}",
                err
            );
        }
    }

    #[test]
    fn color_of_move_detects_prefix() {
        assert_eq!(color_of_move(&CsaMoveToken::new("+7776FU")), Some(Color::Black));
        assert_eq!(color_of_move(&CsaMoveToken::new("-3334FU")), Some(Color::White));
        assert_eq!(color_of_move(&CsaMoveToken::new("7776FU")), None);
    }
}
