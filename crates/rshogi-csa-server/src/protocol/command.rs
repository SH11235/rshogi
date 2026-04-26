//! クライアントから受信した 1 行を [`ClientCommand`] に構造化する。
//!
//! CSA プロトコル v1.2.1 の標準コマンドに加え、x1 拡張の `%%` 系コマンド構文も認識する。
//! 意味検証（状態機械整合・合法手判定）は上位層（[`crate::matching::league::League`]、
//! [`crate::game`]）で行う。

use crate::error::ProtocolError;
use crate::types::{
    Color, CsaLine, CsaMoveToken, GameId, GameName, PlayerName, ReconnectToken, Secret,
};

/// 再接続要求の引数。LOGIN 行末尾の `reconnect:<game_id>+<token>` で送られる。
///
/// 通常の新規対局参加 LOGIN とは異なる経路で処理される（`game_id` の対局が
/// grace 中に存在し、`token` が一致した場合に同一対局者として再参加する）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectRequest {
    /// 再参加対象の対局 ID。
    pub game_id: GameId,
    /// 対局開始時に発行された再接続トークン (Game_Summary 末尾拡張行で配布済み)。
    pub token: ReconnectToken,
}

/// クライアントから到着し得るコマンド一覧。
///
/// 非公開フィールドは持たず、パターンマッチによるルーティングが容易な `enum`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientCommand {
    /// `LOGIN <name> <password> [x1 | reconnect:<game_id>+<token>]`
    Login {
        /// プレイヤ名。
        name: PlayerName,
        /// パスワード（平文）。マスクして扱う。
        password: Secret,
        /// x1 拡張モードを要求するか。
        x1: bool,
        /// 再接続要求。`Some` の場合は新規対局参加ではなく既存対局への再参加要求。
        /// `x1` と排他: 同一 LOGIN 行で両方を指定することはできない。
        reconnect: Option<ReconnectRequest>,
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
    /// とセッション情報）が担う。
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
    ///
    /// 省略形の曖昧性: 第 2 トークンだけが与えられたとき、それが数字だけなら
    /// `nth_move`、そうでなければ `buoy_name` として解釈する。数字だけの
    /// buoy 名（例: `"42"`）を指定したい場合は、3 トークン目に `nth_move`
    /// を付けて `%%FORK <id> 42 0` のように明示する必要がある（Copilot
    /// レビュー指摘）。通常の buoy 命名では影響しない。
    Fork {
        /// 派生元の対局 ID。
        source_game: GameId,
        /// 新規ブイ名（任意）。
        new_buoy: Option<GameName>,
        /// 何手目からフォークするか（任意）。
        nth_move: Option<u32>,
    },
    /// `%%FLOODGATE history [N]`
    ///
    /// `FloodgateHistoryStorage::list_recent(N)` 経由で直近 N 件の対局履歴を
    /// CSA 拡張応答書式で返す照会コマンド。`limit` 省略時は frontend 側で
    /// 既定値 (10 件) を補う契約。
    FloodgateHistory {
        /// 取得件数。`None` の場合は呼び出し側で既定値を補う。
        limit: Option<usize>,
    },
    /// `%%FLOODGATE rating <handle>`
    ///
    /// `RateStorage::load(handle)` 経由で 1 名分の rate / wins / losses /
    /// last_game_id / last_modified を CSA 拡張応答書式で返す照会コマンド。
    /// 該当ハンドル不在は応答内で NOT_FOUND を返す（パースエラーには倒さない）。
    FloodgateRating {
        /// 照会対象のプレイヤ名。
        handle: PlayerName,
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
            let (x1, reconnect) = match parts.next() {
                None => (false, None),
                Some("x1") => (true, None),
                Some(token) => {
                    if let Some(rest) = token.strip_prefix("reconnect:") {
                        let mut split = rest.splitn(2, '+');
                        let game_id_part =
                            split.next().filter(|s| !s.is_empty()).ok_or_else(|| {
                                ProtocolError::Malformed(
                                    "LOGIN: reconnect requires `<game_id>+<token>`".into(),
                                )
                            })?;
                        let token_part =
                            split.next().filter(|s| !s.is_empty()).ok_or_else(|| {
                                ProtocolError::Malformed(
                                    "LOGIN: reconnect requires `<game_id>+<token>`".into(),
                                )
                            })?;
                        (
                            false,
                            Some(ReconnectRequest {
                                game_id: GameId::new(game_id_part),
                                token: ReconnectToken::new(token_part),
                            }),
                        )
                    } else {
                        return Err(ProtocolError::Malformed(format!(
                            "LOGIN: unexpected trailing token `{token}`"
                        )));
                    }
                }
            };
            if parts.next().is_some() {
                return Err(ProtocolError::Malformed("LOGIN: too many trailing tokens".into()));
            }
            Ok(ClientCommand::Login {
                name: PlayerName::new(name),
                password: Secret::new(password),
                x1,
                reconnect,
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
            let second = toks.next();
            let third = toks.next();
            if toks.next().is_some() {
                return Err(ProtocolError::Malformed("%%FORK: unexpected trailing tokens".into()));
            }
            let (buoy, nth) =
                match (second, third) {
                    (None, None) => (None, None),
                    (Some(s), None) => match s.parse() {
                        Ok(nth) => (None, Some(nth)),
                        Err(_) => (Some(GameName::new(s)), None),
                    },
                    (Some(buoy), Some(nth)) => (
                        Some(GameName::new(buoy)),
                        Some(nth.parse().map_err(|e| {
                            ProtocolError::Malformed(format!("%%FORK: bad nth ({e})"))
                        })?),
                    ),
                    (None, Some(_)) => unreachable!("third token without second token"),
                };
            Ok(ClientCommand::Fork {
                source_game: GameId::new(src),
                new_buoy: buoy,
                nth_move: nth,
            })
        }
        "FLOODGATE" => parse_floodgate_subcommand(tail),
        other => Err(ProtocolError::Unknown(format!("%%{other}"))),
    }
}

fn parse_floodgate_subcommand(tail: &str) -> Result<ClientCommand, ProtocolError> {
    let mut parts = tail.splitn(2, char::is_whitespace);
    let sub = parts.next().unwrap_or("");
    let args = parts.next().unwrap_or("").trim_start();
    match sub {
        "" => Err(ProtocolError::Malformed("%%FLOODGATE: missing subcommand".into())),
        "history" => {
            let mut toks = args.split_whitespace();
            let limit = match toks.next() {
                None => None,
                Some(n) => Some(n.parse::<usize>().map_err(|e| {
                    ProtocolError::Malformed(format!("%%FLOODGATE history: bad limit ({e})"))
                })?),
            };
            if toks.next().is_some() {
                return Err(ProtocolError::Malformed(
                    "%%FLOODGATE history: unexpected trailing tokens".into(),
                ));
            }
            Ok(ClientCommand::FloodgateHistory { limit })
        }
        "rating" => {
            let handle = single_token(args, "%%FLOODGATE rating", "handle")?;
            Ok(ClientCommand::FloodgateRating {
                handle: PlayerName::new(handle),
            })
        }
        other => Err(ProtocolError::Unknown(format!("%%FLOODGATE {other}"))),
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

/// [`ClientCommand`] を CSA プロトコル 1 行に直列化する（行末改行は含めない）。
///
/// クライアント → サーバー方向の送信側で、`format!("LOGIN {id} {pw}")` のような
/// 散在する文字列組み立てを 1 関数に集約するためのヘルパ。サーバー側の
/// [`parse_command`] と組で動き、roundtrip プロパティ
/// `parse_command(serialize_client_command(c))` が等値性を満たすことを
/// テストで担保する（標準コマンド・x1 拡張コマンドそれぞれに対し）。
///
/// 注意:
/// - [`ClientCommand::KeepAlive`] は空文字列を返す（呼び出し側で改行のみを送る運用）。
/// - [`ClientCommand::Move::comment`] の値は CSA `'` プレフィックス**抜き**の本体を期待し、
///   出力では `,'<comment>` の形に整える（[`parse_command`] と対称）。
pub fn serialize_client_command(cmd: &ClientCommand) -> String {
    match cmd {
        ClientCommand::Login {
            name,
            password,
            x1,
            reconnect,
        } => {
            let mut s = format!("LOGIN {} {}", name.as_str(), password.expose());
            if *x1 {
                s.push_str(" x1");
            } else if let Some(rec) = reconnect {
                s.push_str(" reconnect:");
                s.push_str(rec.game_id.as_str());
                s.push('+');
                s.push_str(rec.token.as_str());
            }
            s
        }
        ClientCommand::Logout => "LOGOUT".to_owned(),
        ClientCommand::Agree { game_id } => match game_id {
            Some(g) => format!("AGREE {}", g.as_str()),
            None => "AGREE".to_owned(),
        },
        ClientCommand::Reject { game_id } => match game_id {
            Some(g) => format!("REJECT {}", g.as_str()),
            None => "REJECT".to_owned(),
        },
        ClientCommand::Move { token, comment } => match comment {
            Some(c) => format!("{},'{}", token.as_str(), c),
            None => token.as_str().to_owned(),
        },
        ClientCommand::Toryo => "%TORYO".to_owned(),
        ClientCommand::Kachi => "%KACHI".to_owned(),
        ClientCommand::Chudan => "%CHUDAN".to_owned(),
        ClientCommand::KeepAlive => String::new(),
        ClientCommand::Who => "%%WHO".to_owned(),
        ClientCommand::List => "%%LIST".to_owned(),
        ClientCommand::Show { game_id } => format!("%%SHOW {}", game_id.as_str()),
        ClientCommand::Monitor2On { game_id } => format!("%%MONITOR2ON {}", game_id.as_str()),
        ClientCommand::Monitor2Off { game_id } => format!("%%MONITOR2OFF {}", game_id.as_str()),
        ClientCommand::Chat { message } => format!("%%CHAT {message}"),
        ClientCommand::Version => "%%VERSION".to_owned(),
        ClientCommand::Help => "%%HELP".to_owned(),
        ClientCommand::SetBuoy {
            game_name,
            moves,
            count,
        } => {
            let mut s = format!("%%SETBUOY {}", game_name.as_str());
            for m in moves {
                s.push(' ');
                s.push_str(m.as_str());
            }
            s.push(' ');
            s.push_str(&count.to_string());
            s
        }
        ClientCommand::DeleteBuoy { game_name } => {
            format!("%%DELETEBUOY {}", game_name.as_str())
        }
        ClientCommand::GetBuoyCount { game_name } => {
            format!("%%GETBUOYCOUNT {}", game_name.as_str())
        }
        ClientCommand::Fork {
            source_game,
            new_buoy,
            nth_move,
        } => {
            let mut s = format!("%%FORK {}", source_game.as_str());
            match (new_buoy, nth_move) {
                (Some(b), Some(n)) => {
                    s.push(' ');
                    s.push_str(b.as_str());
                    s.push(' ');
                    s.push_str(&n.to_string());
                }
                (Some(b), None) => {
                    s.push(' ');
                    s.push_str(b.as_str());
                }
                (None, Some(n)) => {
                    s.push(' ');
                    s.push_str(&n.to_string());
                }
                (None, None) => {}
            }
            s
        }
        ClientCommand::FloodgateHistory { limit } => match limit {
            Some(n) => format!("%%FLOODGATE history {n}"),
            None => "%%FLOODGATE history".to_owned(),
        },
        ClientCommand::FloodgateRating { handle } => {
            format!("%%FLOODGATE rating {}", handle.as_str())
        }
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
                reconnect: None,
            }
        );
    }

    #[test]
    fn parses_login_x1() {
        let cmd = parse_command(&line("LOGIN bob secret x1")).unwrap();
        let ClientCommand::Login { x1, reconnect, .. } = cmd else {
            panic!("expected Login");
        };
        assert!(x1);
        assert!(reconnect.is_none());
    }

    #[test]
    fn parses_login_reconnect() {
        let cmd = parse_command(&line("LOGIN alice pw reconnect:20260426120000+abcd1234ef567890"))
            .unwrap();
        let ClientCommand::Login { x1, reconnect, .. } = cmd else {
            panic!("expected Login");
        };
        assert!(!x1);
        let req = reconnect.expect("reconnect must be set");
        assert_eq!(req.game_id.as_str(), "20260426120000");
        assert_eq!(req.token.as_str(), "abcd1234ef567890");
    }

    #[test]
    fn rejects_login_reconnect_without_separator() {
        let err = parse_command(&line("LOGIN alice pw reconnect:onlygameid")).unwrap_err();
        assert!(matches!(err, ProtocolError::Malformed(_)));
    }

    #[test]
    fn rejects_login_reconnect_with_empty_parts() {
        let err = parse_command(&line("LOGIN alice pw reconnect:+token")).unwrap_err();
        assert!(matches!(err, ProtocolError::Malformed(_)));
        let err = parse_command(&line("LOGIN alice pw reconnect:gid+")).unwrap_err();
        assert!(matches!(err, ProtocolError::Malformed(_)));
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
    fn parses_fork_with_optional_buoy_and_nth_move() {
        assert_eq!(
            parse_command(&line("%%FORK 20260417120000")).unwrap(),
            ClientCommand::Fork {
                source_game: GameId::new("20260417120000"),
                new_buoy: None,
                nth_move: None,
            }
        );
        assert_eq!(
            parse_command(&line("%%FORK 20260417120000 forked")).unwrap(),
            ClientCommand::Fork {
                source_game: GameId::new("20260417120000"),
                new_buoy: Some(GameName::new("forked")),
                nth_move: None,
            }
        );
        assert_eq!(
            parse_command(&line("%%FORK 20260417120000 24")).unwrap(),
            ClientCommand::Fork {
                source_game: GameId::new("20260417120000"),
                new_buoy: None,
                nth_move: Some(24),
            }
        );
        assert_eq!(
            parse_command(&line("%%FORK 20260417120000 forked 24")).unwrap(),
            ClientCommand::Fork {
                source_game: GameId::new("20260417120000"),
                new_buoy: Some(GameName::new("forked")),
                nth_move: Some(24),
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

    #[test]
    fn serialize_login_basic_and_x1_and_reconnect() {
        let basic = ClientCommand::Login {
            name: PlayerName::new("alice"),
            password: Secret::new("pw"),
            x1: false,
            reconnect: None,
        };
        assert_eq!(serialize_client_command(&basic), "LOGIN alice pw");

        let x1 = ClientCommand::Login {
            name: PlayerName::new("bob"),
            password: Secret::new("secret"),
            x1: true,
            reconnect: None,
        };
        assert_eq!(serialize_client_command(&x1), "LOGIN bob secret x1");

        let rec = ClientCommand::Login {
            name: PlayerName::new("carol"),
            password: Secret::new("p"),
            x1: false,
            reconnect: Some(ReconnectRequest {
                game_id: GameId::new("20260427120000"),
                token: ReconnectToken::new("abcd1234"),
            }),
        };
        assert_eq!(
            serialize_client_command(&rec),
            "LOGIN carol p reconnect:20260427120000+abcd1234"
        );
    }

    #[test]
    fn serialize_move_with_and_without_comment() {
        let bare = ClientCommand::Move {
            token: CsaMoveToken::new("+7776FU"),
            comment: None,
        };
        assert_eq!(serialize_client_command(&bare), "+7776FU");

        let with = ClientCommand::Move {
            token: CsaMoveToken::new("+7776FU"),
            comment: Some("* 123 +7776FU -3334FU".to_owned()),
        };
        assert_eq!(serialize_client_command(&with), "+7776FU,'* 123 +7776FU -3334FU");
    }

    #[test]
    fn serialize_special_and_x1_simple_commands() {
        assert_eq!(serialize_client_command(&ClientCommand::Logout), "LOGOUT");
        assert_eq!(serialize_client_command(&ClientCommand::Toryo), "%TORYO");
        assert_eq!(serialize_client_command(&ClientCommand::Kachi), "%KACHI");
        assert_eq!(serialize_client_command(&ClientCommand::Chudan), "%CHUDAN");
        assert_eq!(serialize_client_command(&ClientCommand::KeepAlive), "");
        assert_eq!(serialize_client_command(&ClientCommand::Who), "%%WHO");
        assert_eq!(serialize_client_command(&ClientCommand::List), "%%LIST");
        assert_eq!(serialize_client_command(&ClientCommand::Version), "%%VERSION");
        assert_eq!(serialize_client_command(&ClientCommand::Help), "%%HELP");
    }

    #[test]
    fn serialize_agree_reject_with_optional_id() {
        assert_eq!(serialize_client_command(&ClientCommand::Agree { game_id: None }), "AGREE");
        assert_eq!(
            serialize_client_command(&ClientCommand::Agree {
                game_id: Some(GameId::new("g1"))
            }),
            "AGREE g1"
        );
        assert_eq!(serialize_client_command(&ClientCommand::Reject { game_id: None }), "REJECT");
    }

    #[test]
    fn serialize_buoy_and_fork() {
        let setbuoy = ClientCommand::SetBuoy {
            game_name: GameName::new("buoy1"),
            moves: vec![CsaMoveToken::new("+7776FU"), CsaMoveToken::new("-3334FU")],
            count: 5,
        };
        assert_eq!(serialize_client_command(&setbuoy), "%%SETBUOY buoy1 +7776FU -3334FU 5");

        let del = ClientCommand::DeleteBuoy {
            game_name: GameName::new("buoy1"),
        };
        assert_eq!(serialize_client_command(&del), "%%DELETEBUOY buoy1");

        let count = ClientCommand::GetBuoyCount {
            game_name: GameName::new("buoy1"),
        };
        assert_eq!(serialize_client_command(&count), "%%GETBUOYCOUNT buoy1");

        let f0 = ClientCommand::Fork {
            source_game: GameId::new("g"),
            new_buoy: None,
            nth_move: None,
        };
        assert_eq!(serialize_client_command(&f0), "%%FORK g");

        let f1 = ClientCommand::Fork {
            source_game: GameId::new("g"),
            new_buoy: Some(GameName::new("forked")),
            nth_move: None,
        };
        assert_eq!(serialize_client_command(&f1), "%%FORK g forked");

        let f2 = ClientCommand::Fork {
            source_game: GameId::new("g"),
            new_buoy: None,
            nth_move: Some(24),
        };
        assert_eq!(serialize_client_command(&f2), "%%FORK g 24");

        let f3 = ClientCommand::Fork {
            source_game: GameId::new("g"),
            new_buoy: Some(GameName::new("forked")),
            nth_move: Some(24),
        };
        assert_eq!(serialize_client_command(&f3), "%%FORK g forked 24");
    }

    #[test]
    fn parse_then_serialize_then_parse_is_stable_for_all_variants() {
        // parse_command(serialize(cmd)) == cmd を主要バリアントに対して確認する
        // (Secret は PartialEq 実装で expose 値に基づくので Login も等価判定可能)。
        let samples = vec![
            ClientCommand::Login {
                name: PlayerName::new("alice"),
                password: Secret::new("pw"),
                x1: false,
                reconnect: None,
            },
            ClientCommand::Login {
                name: PlayerName::new("bob"),
                password: Secret::new("s"),
                x1: true,
                reconnect: None,
            },
            ClientCommand::Login {
                name: PlayerName::new("carol"),
                password: Secret::new("p"),
                x1: false,
                reconnect: Some(ReconnectRequest {
                    game_id: GameId::new("20260427120000"),
                    token: ReconnectToken::new("abcd1234"),
                }),
            },
            ClientCommand::Logout,
            ClientCommand::Agree { game_id: None },
            ClientCommand::Agree {
                game_id: Some(GameId::new("g1")),
            },
            ClientCommand::Reject { game_id: None },
            ClientCommand::Move {
                token: CsaMoveToken::new("+7776FU"),
                comment: None,
            },
            ClientCommand::Move {
                token: CsaMoveToken::new("+7776FU"),
                comment: Some("* 100 +7776FU -3334FU".to_owned()),
            },
            ClientCommand::Toryo,
            ClientCommand::Kachi,
            ClientCommand::Chudan,
            ClientCommand::KeepAlive,
            ClientCommand::Who,
            ClientCommand::List,
            ClientCommand::Show {
                game_id: GameId::new("g1"),
            },
            ClientCommand::Monitor2On {
                game_id: GameId::new("g1"),
            },
            ClientCommand::Monitor2Off {
                game_id: GameId::new("g1"),
            },
            ClientCommand::Version,
            ClientCommand::Help,
            ClientCommand::SetBuoy {
                game_name: GameName::new("buoy1"),
                moves: vec![CsaMoveToken::new("+7776FU"), CsaMoveToken::new("-3334FU")],
                count: 5,
            },
            ClientCommand::DeleteBuoy {
                game_name: GameName::new("buoy1"),
            },
            ClientCommand::GetBuoyCount {
                game_name: GameName::new("buoy1"),
            },
            ClientCommand::Fork {
                source_game: GameId::new("g"),
                new_buoy: None,
                nth_move: None,
            },
            ClientCommand::Fork {
                source_game: GameId::new("g"),
                new_buoy: Some(GameName::new("forked")),
                nth_move: Some(24),
            },
            ClientCommand::Fork {
                source_game: GameId::new("g"),
                new_buoy: None,
                nth_move: Some(24),
            },
            ClientCommand::Chat {
                message: "hello world".to_owned(),
            },
            ClientCommand::FloodgateHistory { limit: None },
            ClientCommand::FloodgateHistory { limit: Some(5) },
            ClientCommand::FloodgateRating {
                handle: PlayerName::new("alice"),
            },
        ];

        for cmd in samples {
            let line = serialize_client_command(&cmd);
            let parsed = parse_command(&CsaLine::new(&line)).unwrap_or_else(|e| {
                panic!("parse failed for serialized `{line}` ({cmd:?}): {e:?}")
            });
            assert_eq!(parsed, cmd, "roundtrip mismatch for {cmd:?} => `{line}`");
        }
    }

    #[test]
    fn parses_floodgate_history_without_limit() {
        let cmd = parse_command(&line("%%FLOODGATE history")).unwrap();
        assert_eq!(cmd, ClientCommand::FloodgateHistory { limit: None });
    }

    #[test]
    fn parses_floodgate_history_with_limit() {
        let cmd = parse_command(&line("%%FLOODGATE history 5")).unwrap();
        assert_eq!(cmd, ClientCommand::FloodgateHistory { limit: Some(5) });
    }

    #[test]
    fn rejects_floodgate_history_with_bad_limit() {
        let err = parse_command(&line("%%FLOODGATE history abc")).unwrap_err();
        assert!(matches!(err, ProtocolError::Malformed(_)), "got {err:?}");
    }

    #[test]
    fn rejects_floodgate_history_with_extra_tokens() {
        let err = parse_command(&line("%%FLOODGATE history 5 6")).unwrap_err();
        assert!(matches!(err, ProtocolError::Malformed(_)), "got {err:?}");
    }

    #[test]
    fn parses_floodgate_rating_with_handle() {
        let cmd = parse_command(&line("%%FLOODGATE rating alice")).unwrap();
        assert_eq!(
            cmd,
            ClientCommand::FloodgateRating {
                handle: PlayerName::new("alice"),
            }
        );
    }

    #[test]
    fn rejects_floodgate_rating_without_handle() {
        let err = parse_command(&line("%%FLOODGATE rating")).unwrap_err();
        assert!(matches!(err, ProtocolError::Malformed(_)), "got {err:?}");
    }

    #[test]
    fn rejects_floodgate_rating_with_extra_tokens() {
        let err = parse_command(&line("%%FLOODGATE rating alice bob")).unwrap_err();
        assert!(matches!(err, ProtocolError::Malformed(_)), "got {err:?}");
    }

    #[test]
    fn rejects_floodgate_without_subcommand() {
        let err = parse_command(&line("%%FLOODGATE")).unwrap_err();
        assert!(matches!(err, ProtocolError::Malformed(_)), "got {err:?}");
    }

    #[test]
    fn rejects_floodgate_unknown_subcommand() {
        let err = parse_command(&line("%%FLOODGATE rank alice")).unwrap_err();
        assert!(matches!(err, ProtocolError::Unknown(_)), "got {err:?}");
    }
}
