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
//! - 私的対局 (`CHALLENGE_LOBBY` / `LOGIN_LOBBY <handle>+private-<token>+free`)
//!   の入口パース。Issue #582 の Workers 側受け入れ基準のうち本 PR スコープ
//!   (token 発行 + LOGIN 認識 + 永続化 + Alarm purge) で参照される。両者揃った
//!   後の対局起動経路は次 PR に分割するため、本モジュールは対局室 (GameRoom DO)
//!   起動側の知識を持たない。

use rshogi_csa_server::matching::challenge::ChallengeToken;
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
    /// `<game_name>` が `CLOCK_PRESETS` で宣言されたいずれの preset にも一致しない
    /// （strict mode）。`CLOCK_PRESETS` 未設定 / 空配列のときは strict mode 自体が
    /// 無効化されているため、本エラーは発生しない。
    UnknownGameName,
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
            Self::UnknownGameName => "unknown_game_name",
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

/// `CHALLENGE_LOBBY <inviter> <opponent> <color> <clock_preset> [<sfen>]` の
/// パース結果。Issue #582 Workers 経路で `%%CHALLENGE` の代わりとなる
/// メッセージで、`Lobby` DO の `websocket_message` 入口から駆動する。
///
/// password / 認証は持たない (Workers 経路は self-claim、`<inviter>` を
/// 申告ベースで信頼する。`opponent_handle` 存在チェックも Workers では
/// 行わない)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChallengeLobbyRequest {
    /// 招待者 handle (発行者)。
    pub inviter: String,
    /// 招待される相手 handle。
    pub opponent: String,
    /// 招待者の希望色。`free` は `None` で表現する (両者揃った時点で乱択)。
    pub inviter_color: Option<Color>,
    /// 持ち時間 preset 名。`CLOCK_PRESETS` で宣言された `game_name` に対応する。
    pub clock_preset: String,
    /// 開始局面 SFEN (任意)。`None` は平手。
    pub initial_sfen: Option<String>,
}

/// CHALLENGE_LOBBY パースエラー。`reason` でクライアント応答用の reason 文字列
/// を返す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChallengeLobbyError {
    /// `CHALLENGE_LOBBY` プレフィックスがない。
    NotChallengeCommand,
    /// 引数 (inviter / opponent / color / clock_preset) が足りない。
    BadFormat,
    /// `<color>` が `black` / `sente` / `white` / `gote` / `free` 以外。
    BadColor,
    /// `<inviter>` または `<opponent>` が空。
    BadHandle,
    /// `<clock_preset>` が `[A-Za-z0-9_-]` の文字種または 1〜32 文字長制限に違反。
    BadClockPreset,
}

impl ChallengeLobbyError {
    /// クライアントへ返す `CHALLENGE_LOBBY:incorrect <reason>` の reason 部分。
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NotChallengeCommand => "not_challenge_command",
            Self::BadFormat => "bad_format",
            Self::BadColor => "bad_color",
            Self::BadHandle => "bad_handle",
            Self::BadClockPreset => "bad_clock_preset",
        }
    }
}

/// `<color>` トークンを解釈する。`free` は `None`、`black|sente` / `white|gote`
/// は対応する `Color` を返す。
fn parse_challenge_color(token: &str) -> Result<Option<Color>, ChallengeLobbyError> {
    match token {
        "black" | "sente" => Ok(Some(Color::Black)),
        "white" | "gote" => Ok(Some(Color::White)),
        "free" => Ok(None),
        _ => Err(ChallengeLobbyError::BadColor),
    }
}

/// `CHALLENGE_LOBBY <inviter> <opponent> <color> <clock_preset> [<sfen>]` を
/// パースする。
///
/// `<sfen>` はトークン途中の空白を許容する (SFEN 文字列内には空白を含む) ため、
/// 5 トークン目以降を残り全てとして取り扱う。
pub fn parse_challenge_lobby(line: &str) -> Result<ChallengeLobbyRequest, ChallengeLobbyError> {
    let rest = line
        .strip_prefix("CHALLENGE_LOBBY ")
        .ok_or(ChallengeLobbyError::NotChallengeCommand)?;
    // 4 つの必須トークン + 残り (= optional SFEN) に分割する。
    let mut parts = rest.splitn(5, char::is_whitespace);
    // 4 つの必須トークンは全て `trim_start` を適用して連続空白の影響を吸収する
    // (`splitn` は連続空白でも空文字を返すため、対称的に処理する)。
    let inviter = parts.next().map(str::trim_start).ok_or(ChallengeLobbyError::BadFormat)?;
    let opponent = parts.next().map(str::trim_start).ok_or(ChallengeLobbyError::BadFormat)?;
    let color_tok = parts.next().map(str::trim_start).ok_or(ChallengeLobbyError::BadFormat)?;
    let clock_preset = parts.next().map(str::trim_start).ok_or(ChallengeLobbyError::BadFormat)?;
    if inviter.is_empty() || opponent.is_empty() {
        return Err(ChallengeLobbyError::BadHandle);
    }
    if color_tok.is_empty() || clock_preset.is_empty() {
        return Err(ChallengeLobbyError::BadFormat);
    }
    let inviter_color = parse_challenge_color(color_tok)?;
    if !is_valid_game_name(clock_preset) {
        return Err(ChallengeLobbyError::BadClockPreset);
    }
    let initial_sfen = match parts.next().map(str::trim) {
        Some(s) if !s.is_empty() => Some(s.to_owned()),
        _ => None,
    };

    Ok(ChallengeLobbyRequest {
        inviter: inviter.to_owned(),
        opponent: opponent.to_owned(),
        inviter_color,
        clock_preset: clock_preset.to_owned(),
        initial_sfen,
    })
}

/// `CHALLENGE_LOBBY:OK <token> <ttl_sec>` 応答 line。
pub fn build_challenge_ok_line(token: &str, ttl_sec: u64) -> String {
    format!("CHALLENGE_LOBBY:OK {token} {ttl_sec}")
}

/// `CHALLENGE_LOBBY:incorrect <reason>` 応答 line。
pub fn build_challenge_incorrect_line(reason: &str) -> String {
    format!("CHALLENGE_LOBBY:incorrect {reason}")
}

/// 私的対局 LOGIN (`<handle>+private-<24hex>+free`) のパース結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginLobbyPrivateRequest {
    /// LOGIN 申告された handle (= challenge entry の `inviter` または `opponent`
    /// のいずれかと一致するはず。一致確認は `lobby.rs` 側で `ChallengeRegistry`
    /// と照合する)。
    pub handle: String,
    /// `private-` prefix を除いた 24 文字 hex 部分。`ChallengeToken::from_raw`
    /// で wrap 済の値を保持する。
    pub token: ChallengeToken,
}

/// 私的対局 LOGIN_LOBBY パースの失敗種別。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginLobbyPrivateError {
    /// `LOGIN_LOBBY` プレフィックスがない。
    NotLoginCommand,
    /// 引数構造 (`<id> <password>` の 2 トークン) が崩れている。
    BadFormat,
    /// `+` で正確に 3 分割できない / handle が空 / 中央トークンが
    /// `private-` prefix を持たない。
    Malformed,
    /// 中央トークンが `private-<...>` だが、続く `<...>` が 24 文字小文字 hex
    /// でない。
    PrivateTokenMalformed,
    /// 末尾の color トークンが `+free` 以外 (色指定は token に焼き込み済のため、
    /// LOGIN 側では `+free` のみ受理する仕様)。
    ColorMustBeFree,
}

impl LoginLobbyPrivateError {
    /// クライアントへ返す `LOGIN_LOBBY:incorrect <reason>` の reason 部分。
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NotLoginCommand => "not_login_command",
            Self::BadFormat => "bad_format",
            Self::Malformed => "bad_id_format",
            Self::PrivateTokenMalformed => "bad_private_token",
            Self::ColorMustBeFree => "color_must_be_free_for_private_game",
        }
    }
}

/// LOGIN_LOBBY 入口で「私的対局フォーマット (`<handle>+private-<...>+...`) か」
/// を peek する。`+` で分割した中央トークンが `private-` prefix を持てば
/// `true`。`parse_login_lobby` (公開経路) と `parse_login_lobby_with_free`
/// (私的経路) の入口分岐に使う。
pub fn is_private_login_handle(id: &str) -> bool {
    id.split('+').nth(1).is_some_and(|middle| middle.starts_with("private-"))
}

/// `LOGIN_LOBBY <handle>+private-<24hex>+free <password>` をパースする。
///
/// 既存 [`parse_login_lobby`] と異なり、中央トークンが `private-<...>` 形式の
/// 私的対局専用 handle であることを前提とする。`is_private_login_handle` で
/// `true` を返した接続のみがここに分岐する契約。
///
/// 検証順:
/// 1. `LOGIN_LOBBY ` prefix
/// 2. `<id> <password>` の 2 トークン (extra args は `BadFormat`)
/// 3. id を `+` で正確に 3 分割
/// 4. handle (index 0) が非空
/// 5. 中央トークン (index 1) が `private-` prefix + ちょうど 24 文字小文字 hex
/// 6. 末尾トークン (index 2) が `"free"` のみ受理 (private は `+free` のみ)
pub fn parse_login_lobby_with_free(
    line: &str,
) -> Result<LoginLobbyPrivateRequest, LoginLobbyPrivateError> {
    let rest = line
        .strip_prefix("LOGIN_LOBBY ")
        .ok_or(LoginLobbyPrivateError::NotLoginCommand)?;
    let mut parts = rest.split_whitespace();
    let id = parts.next().ok_or(LoginLobbyPrivateError::BadFormat)?;
    // password は受信するが本体では検証しない (self-claim)。引数の存在のみ確認。
    let _password = parts.next().ok_or(LoginLobbyPrivateError::BadFormat)?;
    if parts.next().is_some() {
        return Err(LoginLobbyPrivateError::BadFormat);
    }

    let mut id_parts = id.split('+');
    let handle = id_parts.next().ok_or(LoginLobbyPrivateError::Malformed)?;
    let middle = id_parts.next().ok_or(LoginLobbyPrivateError::Malformed)?;
    let color = id_parts.next().ok_or(LoginLobbyPrivateError::Malformed)?;
    if id_parts.next().is_some() {
        return Err(LoginLobbyPrivateError::Malformed);
    }
    if handle.is_empty() {
        return Err(LoginLobbyPrivateError::Malformed);
    }
    let hex_part = match middle.strip_prefix("private-") {
        Some(rest) => rest,
        None => return Err(LoginLobbyPrivateError::Malformed),
    };
    let hex_ok = hex_part.len() == 24
        && hex_part.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hex_ok {
        return Err(LoginLobbyPrivateError::PrivateTokenMalformed);
    }
    if color != "free" {
        return Err(LoginLobbyPrivateError::ColorMustBeFree);
    }
    Ok(LoginLobbyPrivateRequest {
        handle: handle.to_owned(),
        token: ChallengeToken::from_raw(hex_part),
    })
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

    /// 既存 `parse_login_lobby` は私的対局の `+free` を `BadColor` として
    /// 拒否し続ける (`dispatch_pending_line` 側で `is_private_login_handle` 経由
    /// で先に分岐させる契約)。本テストは「私的対局専用 parser 追加で公開
    /// マッチング parser が `+free` を黙って通すようになっていない」ことを
    /// 固定する後方互換 regression。
    #[test]
    fn parse_login_lobby_still_rejects_free_color() {
        assert_eq!(
            parse_login_lobby("LOGIN_LOBBY alice+private-0123456789abcdef0123abcd+free pw"),
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

    /// `UnknownGameName` の reason は `"unknown_game_name"` を返す（lobby が
    /// `LOGIN_LOBBY:incorrect unknown_game_name` を組み立てるためのキー）。
    #[test]
    fn unknown_game_name_reason_is_stable() {
        assert_eq!(LoginLobbyError::UnknownGameName.reason(), "unknown_game_name");
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

    /// `CHALLENGE_LOBBY` の正常パース。`free` は `None` (両者揃った時点で乱択)。
    #[test]
    fn parse_challenge_lobby_happy_path_free() {
        let req = parse_challenge_lobby("CHALLENGE_LOBBY alice bob free byoyomi-600-10").unwrap();
        assert_eq!(req.inviter, "alice");
        assert_eq!(req.opponent, "bob");
        assert_eq!(req.inviter_color, None);
        assert_eq!(req.clock_preset, "byoyomi-600-10");
        assert_eq!(req.initial_sfen, None);
    }

    /// `<color>` トークンの sente / gote 別名を受理する (CSA 慣習)。
    #[test]
    fn parse_challenge_lobby_accepts_sente_gote_aliases() {
        let req = parse_challenge_lobby("CHALLENGE_LOBBY alice bob sente byoyomi-600-10").unwrap();
        assert_eq!(req.inviter_color, Some(Color::Black));
        let req = parse_challenge_lobby("CHALLENGE_LOBBY alice bob gote byoyomi-600-10").unwrap();
        assert_eq!(req.inviter_color, Some(Color::White));
    }

    /// SFEN は 5 トークン目以降を残り全てとして取り、内部空白を保持する。
    #[test]
    fn parse_challenge_lobby_preserves_sfen_with_internal_whitespace() {
        let raw = "CHALLENGE_LOBBY alice bob black byoyomi-600-10 lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let req = parse_challenge_lobby(raw).unwrap();
        assert_eq!(
            req.initial_sfen.as_deref(),
            Some("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
        );
    }

    /// プレフィックスが違う行は `NotChallengeCommand`。
    #[test]
    fn parse_challenge_lobby_rejects_wrong_command() {
        assert_eq!(
            parse_challenge_lobby("LOGIN_LOBBY alice+g+black pw"),
            Err(ChallengeLobbyError::NotChallengeCommand)
        );
    }

    /// 必須トークン不足は `BadFormat`。
    #[test]
    fn parse_challenge_lobby_rejects_missing_args() {
        assert_eq!(
            parse_challenge_lobby("CHALLENGE_LOBBY alice bob free"),
            Err(ChallengeLobbyError::BadFormat)
        );
    }

    /// 未知の color は `BadColor`。
    #[test]
    fn parse_challenge_lobby_rejects_bad_color() {
        assert_eq!(
            parse_challenge_lobby("CHALLENGE_LOBBY alice bob purple byoyomi-600-10"),
            Err(ChallengeLobbyError::BadColor)
        );
    }

    /// `clock_preset` が文字種制限に違反すると `BadClockPreset`。
    /// 注意: `splitn(5, ws)` で 4 トークン目までを clock_preset として確保するため、
    /// 「`has space`」のように空白で区切ると `has` が clock_preset、`space` が
    /// SFEN として受理される。文字種違反は `.` 等の非 ASCII alnum / `_` / `-`
    /// 文字で検証する。
    #[test]
    fn parse_challenge_lobby_rejects_bad_clock_preset() {
        assert_eq!(
            parse_challenge_lobby("CHALLENGE_LOBBY alice bob free with.dot"),
            Err(ChallengeLobbyError::BadClockPreset)
        );
        let too_long = "x".repeat(33);
        let line = format!("CHALLENGE_LOBBY alice bob free {too_long}");
        assert_eq!(parse_challenge_lobby(&line), Err(ChallengeLobbyError::BadClockPreset));
    }

    /// `<inviter>` または `<opponent>` が空のときは `BadHandle` を返す。
    /// 連続 space で空 handle が紛れ込まないことを確認する。
    #[test]
    fn parse_challenge_lobby_rejects_empty_handle() {
        // 空 inviter / opponent: トークン不足側に倒れる
        // ("CHALLENGE_LOBBY  bob ..." は `splitn` 後 inviter="", opponent="bob")
        assert_eq!(
            parse_challenge_lobby("CHALLENGE_LOBBY  bob free byoyomi-600-10"),
            Err(ChallengeLobbyError::BadHandle),
        );
    }

    /// 応答 line のフォーマット安定性。
    #[test]
    fn challenge_lobby_response_lines_format() {
        assert_eq!(
            build_challenge_ok_line("0123456789abcdef0123abcd", 3600),
            "CHALLENGE_LOBBY:OK 0123456789abcdef0123abcd 3600"
        );
        assert_eq!(
            build_challenge_incorrect_line("self_challenge"),
            "CHALLENGE_LOBBY:incorrect self_challenge"
        );
    }

    /// `is_private_login_handle` は `+private-` を peek するだけで hex 部分の
    /// 妥当性は問わない (parser 側で検証する)。
    #[test]
    fn is_private_login_handle_detects_prefix() {
        assert!(is_private_login_handle("alice+private-0123456789abcdef0123abcd+free"));
        assert!(is_private_login_handle("alice+private-short+free"));
        assert!(!is_private_login_handle("alice+game-eval+black"));
        assert!(!is_private_login_handle("aliceonly"));
    }

    /// 私的対局 LOGIN_LOBBY の正常パス。token は 24 文字 hex として wrap される。
    #[test]
    fn parse_login_lobby_with_free_happy_path() {
        let req = parse_login_lobby_with_free(
            "LOGIN_LOBBY alice+private-0123456789abcdef0123abcd+free pw",
        )
        .unwrap();
        assert_eq!(req.handle, "alice");
        assert_eq!(req.token.as_str(), "0123456789abcdef0123abcd");
    }

    /// 末尾 color が `free` 以外なら `ColorMustBeFree`。
    #[test]
    fn parse_login_lobby_with_free_rejects_non_free_color() {
        assert_eq!(
            parse_login_lobby_with_free(
                "LOGIN_LOBBY alice+private-0123456789abcdef0123abcd+black pw"
            ),
            Err(LoginLobbyPrivateError::ColorMustBeFree)
        );
        assert_eq!(
            parse_login_lobby_with_free(
                "LOGIN_LOBBY alice+private-0123456789abcdef0123abcd+white pw"
            ),
            Err(LoginLobbyPrivateError::ColorMustBeFree)
        );
    }

    /// hex 部分が 24 文字でない / 大文字 / 非 hex の場合は `PrivateTokenMalformed`。
    #[test]
    fn parse_login_lobby_with_free_rejects_malformed_token() {
        // 短すぎ
        assert_eq!(
            parse_login_lobby_with_free("LOGIN_LOBBY alice+private-0123456789ab+free pw"),
            Err(LoginLobbyPrivateError::PrivateTokenMalformed)
        );
        // 大文字混入
        assert_eq!(
            parse_login_lobby_with_free(
                "LOGIN_LOBBY alice+private-0123456789ABCDEF0123abcd+free pw"
            ),
            Err(LoginLobbyPrivateError::PrivateTokenMalformed)
        );
        // 非 hex
        assert_eq!(
            parse_login_lobby_with_free(
                "LOGIN_LOBBY alice+private-zzzz456789abcdef0123abcd+free pw"
            ),
            Err(LoginLobbyPrivateError::PrivateTokenMalformed)
        );
    }

    /// 中央トークンが `private-` prefix を持たないなら `Malformed` (peek が
    /// 通った後の防御的キャッチ)。
    #[test]
    fn parse_login_lobby_with_free_rejects_non_private_middle() {
        assert_eq!(
            parse_login_lobby_with_free("LOGIN_LOBBY alice+game-eval+free pw"),
            Err(LoginLobbyPrivateError::Malformed)
        );
    }

    /// 引数不足 / 余剰は `BadFormat`。
    #[test]
    fn parse_login_lobby_with_free_rejects_arg_count_mismatch() {
        assert_eq!(
            parse_login_lobby_with_free("LOGIN_LOBBY alice+private-0123456789abcdef0123abcd+free"),
            Err(LoginLobbyPrivateError::BadFormat)
        );
        assert_eq!(
            parse_login_lobby_with_free(
                "LOGIN_LOBBY alice+private-0123456789abcdef0123abcd+free pw extra"
            ),
            Err(LoginLobbyPrivateError::BadFormat)
        );
    }
}
