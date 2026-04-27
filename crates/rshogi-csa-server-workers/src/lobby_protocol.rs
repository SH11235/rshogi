//! LobbyDO プロトコルと in-memory 状態の純粋ロジック。
//!
//! `lobby.rs` (wasm32 限定の DO ランタイム) から I/O 非依存な部分を切り出して
//! ホスト target でユニットテストできるようにする。
//!
//! 含まれる責務:
//! - `LOGIN_LOBBY <handle>+<game_name>+<color> <password>` のパース。
//! - `<game_name>` の文字種制限 (`[A-Za-z0-9_-]`、長さ 1〜32)。
//! - in-memory queue ([`LobbyQueue`]) と直接マッチング (`DirectMatchStrategy` 再利用)。
//! - 出力 line のシリアライズ (`LOGIN_LOBBY:<handle> OK` / `MATCHED <room_id> <color>` 等)。

use rshogi_csa_server::matching::{
    league::PairingCandidate,
    pairing::{DirectMatchStrategy, PairingLogic},
};
use rshogi_csa_server::types::{Color, PlayerName};

/// LOGIN_LOBBY コマンドのパース結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginLobbyRequest {
    pub handle: String,
    pub game_name: String,
    pub color: Color,
}

/// LOGIN_LOBBY パースエラー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginLobbyError {
    /// `LOGIN_LOBBY` プレフィックスがない。
    NotLoginCommand,
    /// `LOGIN_LOBBY <name> <password>` の引数が足りない。
    BadFormat,
    /// `<handle>+<game_name>+<color>` の `+` 区切り 3 トークンになっていない。
    BadIdFormat,
    /// `<color>` が `black` / `white` のどちらでもない。
    BadColor,
    /// `<game_name>` が `[A-Za-z0-9_-]` の文字種または 1〜32 文字長制限に違反。
    BadGameName,
}

impl LoginLobbyError {
    /// クライアントへ返す `LOGIN_LOBBY:incorrect <reason>` の reason 部分。
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NotLoginCommand => "not_login_command",
            Self::BadFormat => "bad_format",
            Self::BadIdFormat => "bad_id_format",
            Self::BadColor => "bad_color",
            Self::BadGameName => "bad_game_name",
        }
    }
}

const MAX_GAME_NAME_LEN: usize = 32;

fn is_valid_game_name(name: &str) -> bool {
    let len = name.len();
    if !(1..=MAX_GAME_NAME_LEN).contains(&len) {
        return false;
    }
    name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `LOGIN_LOBBY <handle>+<game_name>+<color> <password>` をパースする。
pub fn parse_login_lobby(line: &str) -> Result<LoginLobbyRequest, LoginLobbyError> {
    let rest = line.strip_prefix("LOGIN_LOBBY ").ok_or(LoginLobbyError::NotLoginCommand)?;
    let mut parts = rest.split_whitespace();
    let id = parts.next().ok_or(LoginLobbyError::BadFormat)?;
    // password は受信するが本体では検証しない (self-claim)。引数の存在のみ確認。
    let _password = parts.next().ok_or(LoginLobbyError::BadFormat)?;
    if parts.next().is_some() {
        return Err(LoginLobbyError::BadFormat);
    }

    let mut id_parts = id.split('+');
    let handle = id_parts.next().ok_or(LoginLobbyError::BadIdFormat)?;
    let game_name = id_parts.next().ok_or(LoginLobbyError::BadIdFormat)?;
    let color_str = id_parts.next().ok_or(LoginLobbyError::BadIdFormat)?;
    if id_parts.next().is_some() {
        return Err(LoginLobbyError::BadIdFormat);
    }
    if handle.is_empty() {
        return Err(LoginLobbyError::BadIdFormat);
    }
    if !is_valid_game_name(game_name) {
        return Err(LoginLobbyError::BadGameName);
    }
    let color = match color_str {
        "black" => Color::Black,
        "white" => Color::White,
        _ => return Err(LoginLobbyError::BadColor),
    };

    Ok(LoginLobbyRequest {
        handle: handle.to_owned(),
        game_name: game_name.to_owned(),
        color,
    })
}

/// 1 件のキューエントリ。WS 識別子は呼び出し側で別途保持する (DO ランタイム側で
/// `WebSocket` 値に紐付ける)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    pub handle: String,
    pub game_name: String,
    pub color: Color,
}

/// LobbyDO の in-memory queue。
///
/// queue は volatile (Hibernation 復帰で空になる)。client は再 LOGIN_LOBBY する想定。
#[derive(Debug, Default)]
pub struct LobbyQueue {
    entries: Vec<QueueEntry>,
}

impl LobbyQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// queue にエントリを追加する。同名 handle が既存なら旧を削除して新で置換する
    /// (`evict_old` 挙動、本家 Floodgate と同じ)。`limit` 超過時は false を返して
    /// 失敗を通知する (`LobbyDO` 側で `LOGIN_LOBBY:incorrect queue_full` を返す)。
    pub fn enqueue(&mut self, entry: QueueEntry, limit: usize) -> bool {
        self.entries.retain(|e| e.handle != entry.handle);
        if self.entries.len() >= limit {
            return false;
        }
        self.entries.push(entry);
        true
    }

    /// handle で 1 件削除する。LOGOUT_LOBBY / WS close 時に呼ぶ。
    pub fn remove(&mut self, handle: &str) {
        self.entries.retain(|e| e.handle != handle);
    }

    /// 指定 entry のスナップショットを返す (テスト用)。
    pub fn entries(&self) -> &[QueueEntry] {
        &self.entries
    }

    /// 同 `game_name` 内で `DirectMatchStrategy` を回し、最初に成立したペアを返す。
    /// 成立したエントリは queue から取り除いて返す (本ペアの送出用に handle/color を保持)。
    pub fn try_pair(&mut self) -> Option<MatchedEntries> {
        let game_names: Vec<String> = {
            let mut names: Vec<String> = self.entries.iter().map(|e| e.game_name.clone()).collect();
            names.sort();
            names.dedup();
            names
        };

        for game_name in game_names {
            let candidates: Vec<PairingCandidate> = self
                .entries
                .iter()
                .filter(|e| e.game_name == game_name)
                .map(|e| PairingCandidate {
                    name: PlayerName::new(&e.handle),
                    preferred_color: Some(e.color),
                    rate: None,
                    recent_opponents: Vec::new(),
                })
                .collect();
            let pairs = DirectMatchStrategy::new().try_pair(&candidates);
            if let Some(pair) = pairs.into_iter().next() {
                let black = self.take_entry(pair.black.as_str(), &game_name);
                let white = self.take_entry(pair.white.as_str(), &game_name);
                if let (Some(black), Some(white)) = (black, white) {
                    return Some(MatchedEntries {
                        black,
                        white,
                        game_name,
                    });
                }
            }
        }
        None
    }

    fn take_entry(&mut self, handle: &str, game_name: &str) -> Option<QueueEntry> {
        let pos = self
            .entries
            .iter()
            .position(|e| e.handle == handle && e.game_name == game_name)?;
        Some(self.entries.remove(pos))
    }
}

/// `try_pair` 成立時の返却値。`MatchedPair` (`PlayerName` のみ) を queue 上の
/// メタ情報まで含めた形に拡張したもの。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedEntries {
    pub black: QueueEntry,
    pub white: QueueEntry,
    pub game_name: String,
}

/// 成立した `MatchedEntries` から発番する `room_id` を組み立てる。
///
/// 形式: `lobby-<game_name>-<32hex>` (32 hex = 128 bit rand)。
pub fn build_room_id(game_name: &str, rand128_hex: &str) -> String {
    format!("lobby-{game_name}-{rand128_hex}")
}

/// MATCHED 通知 line を組み立てる。`<room_id>` と `<color>` は半角スペース区切り。
pub fn build_matched_line(room_id: &str, color: Color) -> String {
    let color_str = match color {
        Color::Black => "black",
        Color::White => "white",
    };
    format!("MATCHED {room_id} {color_str}")
}

/// LOGIN_LOBBY:OK 行。
pub fn build_login_ok_line(handle: &str) -> String {
    format!("LOGIN_LOBBY:{handle} OK")
}

/// LOGIN_LOBBY:incorrect <reason> 行。
pub fn build_login_incorrect_line(reason: &str) -> String {
    format!("LOGIN_LOBBY:incorrect {reason}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_login_lobby_happy_path() {
        let req = parse_login_lobby("LOGIN_LOBBY alice+game-eval+black anything").unwrap();
        assert_eq!(req.handle, "alice");
        assert_eq!(req.game_name, "game-eval");
        assert_eq!(req.color, Color::Black);
    }

    #[test]
    fn parse_login_lobby_rejects_missing_command() {
        assert_eq!(parse_login_lobby("LOGIN alice pw"), Err(LoginLobbyError::NotLoginCommand));
    }

    #[test]
    fn parse_login_lobby_rejects_short_args() {
        assert_eq!(parse_login_lobby("LOGIN_LOBBY alice"), Err(LoginLobbyError::BadFormat));
    }

    #[test]
    fn parse_login_lobby_rejects_extra_args() {
        assert_eq!(
            parse_login_lobby("LOGIN_LOBBY alice+g+black pw extra"),
            Err(LoginLobbyError::BadFormat)
        );
    }

    #[test]
    fn parse_login_lobby_rejects_bad_id() {
        assert_eq!(parse_login_lobby("LOGIN_LOBBY no_plus pw"), Err(LoginLobbyError::BadIdFormat));
        assert_eq!(parse_login_lobby("LOGIN_LOBBY a+b+c+d pw"), Err(LoginLobbyError::BadIdFormat));
    }

    #[test]
    fn parse_login_lobby_rejects_bad_color() {
        assert_eq!(
            parse_login_lobby("LOGIN_LOBBY alice+g+gray pw"),
            Err(LoginLobbyError::BadColor)
        );
    }

    #[test]
    fn parse_login_lobby_rejects_bad_game_name() {
        assert_eq!(
            parse_login_lobby("LOGIN_LOBBY alice++black pw"),
            Err(LoginLobbyError::BadGameName)
        );
        // Special char `+` in game_name not allowed (would break MATCHED parse).
        // Note: the `+` separator already eats this, so we test a different non-allowed char.
        assert_eq!(
            parse_login_lobby("LOGIN_LOBBY alice+game.name+black pw"),
            Err(LoginLobbyError::BadGameName)
        );
        let too_long = "x".repeat(33);
        let line = format!("LOGIN_LOBBY alice+{too_long}+black pw");
        assert_eq!(parse_login_lobby(&line), Err(LoginLobbyError::BadGameName));
    }

    fn entry(h: &str, g: &str, c: Color) -> QueueEntry {
        QueueEntry {
            handle: h.to_owned(),
            game_name: g.to_owned(),
            color: c,
        }
    }

    #[test]
    fn enqueue_evicts_old_handle() {
        let mut q = LobbyQueue::new();
        assert!(q.enqueue(entry("alice", "g", Color::Black), 100));
        assert!(q.enqueue(entry("alice", "g", Color::White), 100));
        assert_eq!(q.len(), 1);
        assert_eq!(q.entries()[0].color, Color::White);
    }

    #[test]
    fn enqueue_respects_limit() {
        let mut q = LobbyQueue::new();
        assert!(q.enqueue(entry("a", "g", Color::Black), 1));
        assert!(!q.enqueue(entry("b", "g", Color::White), 1));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn try_pair_matches_complementary_colors() {
        let mut q = LobbyQueue::new();
        q.enqueue(entry("alice", "g", Color::Black), 100);
        q.enqueue(entry("bob", "g", Color::White), 100);
        let m = q.try_pair().expect("pair");
        assert_eq!(m.black.handle, "alice");
        assert_eq!(m.white.handle, "bob");
        assert_eq!(m.game_name, "g");
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn try_pair_does_not_match_across_game_names() {
        let mut q = LobbyQueue::new();
        q.enqueue(entry("alice", "g1", Color::Black), 100);
        q.enqueue(entry("bob", "g2", Color::White), 100);
        assert!(q.try_pair().is_none());
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn try_pair_does_not_match_same_color() {
        let mut q = LobbyQueue::new();
        q.enqueue(entry("alice", "g", Color::Black), 100);
        q.enqueue(entry("bob", "g", Color::Black), 100);
        assert!(q.try_pair().is_none());
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn try_pair_returns_one_pair_at_a_time() {
        let mut q = LobbyQueue::new();
        q.enqueue(entry("a", "g", Color::Black), 100);
        q.enqueue(entry("b", "g", Color::White), 100);
        q.enqueue(entry("c", "g", Color::Black), 100);
        q.enqueue(entry("d", "g", Color::White), 100);
        let m1 = q.try_pair().expect("first pair");
        assert_eq!(q.len(), 2);
        let m2 = q.try_pair().expect("second pair");
        assert_eq!(q.len(), 0);
        // 名前順なので (a,b) と (c,d)
        assert_eq!(m1.black.handle, "a");
        assert_eq!(m1.white.handle, "b");
        assert_eq!(m2.black.handle, "c");
        assert_eq!(m2.white.handle, "d");
    }

    #[test]
    fn build_room_id_format() {
        assert_eq!(
            build_room_id("game-eval", "0123456789abcdef0123456789abcdef"),
            "lobby-game-eval-0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn build_matched_line_uses_space_separator() {
        assert_eq!(build_matched_line("lobby-g-abcd", Color::Black), "MATCHED lobby-g-abcd black");
    }

    #[test]
    fn login_lines_format() {
        assert_eq!(build_login_ok_line("alice"), "LOGIN_LOBBY:alice OK");
        assert_eq!(build_login_incorrect_line("queue_full"), "LOGIN_LOBBY:incorrect queue_full");
    }
}
