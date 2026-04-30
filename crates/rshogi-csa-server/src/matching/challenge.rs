//! 私的対局 (private match / challenge) の登録簿。
//!
//! `%%CHALLENGE` (TCP) / `CHALLENGE_LOBBY` (Workers) で発行される使い捨て
//! token と、その token に紐付く対局パラメータ (inviter / opponent / 配色 /
//! 時計 / 開始局面 / TTL) を保持する。1 マッチで使い捨て (`consume` 後は
//! lookup 不可) で、TTL 超過は `purge_expired` で自然枯死させる。
//!
//! # 永続化と runtime session の責務分離
//!
//! 本構造体 ([`ChallengeEntry`]) は **`Serialize`/`Deserialize` 可能な永続
//! データ**だけを持つ。Workers DO storage に put/get できる形に保つことで、
//! Hibernation 復帰時に restore できるようにする。
//!
//! TCP の runtime session (`Arc<Notify>` / `oneshot::Sender<TcpTransport>`)
//! は serialize 不能なため core では持たず、frontend (`crates/rshogi-csa-server-tcp`)
//! 側で `TcpChallengePending` として別途 in-memory 管理する。Workers 側は
//! WS attachment id (String) が serialize 可能なため
//! [`ChallengeEntry::pending_ws_attachment_ids`] に直接持たせる。
//!
//! # スコープ
//!
//! 本モジュールは Issue #582 の **Core foundation** 部分。`%%CHALLENGE` /
//! `CHALLENGE_LOBBY` の protocol parser、TCP `drive_private_game` lifecycle、
//! Workers DO storage 永続化 / Alarm purge は別の follow-up PR で実装される。

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::game::clock::ClockSpec;
use crate::types::{Color, PlayerName};

/// 96-bit エントロピーの challenge token。`private-<24hex>` 形式 game_name の
/// 末尾 24 文字部分。`Serialize`/`Deserialize` は内部 String を経由する。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChallengeToken(String);

impl ChallengeToken {
    /// 12 byte (96-bit) の乱数を引き、24 文字の小文字 hex 文字列として token に包む。
    /// 乱数源・hex 文字種・wasm32 経路の方針は [`crate::types::ReconnectToken::generate`]
    /// と同等。`private-<24hex>` で合計 32 文字となり、Workers 既存
    /// `MAX_GAME_NAME_LEN = 32` の制限を validator bypass せずに満たす。
    pub fn generate() -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let bytes: [u8; 12] = rand::random();
        let mut s = String::with_capacity(24);
        for b in bytes {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 0x0f) as usize] as char);
        }
        Self(s)
    }

    /// 既存文字列から token を作る。検証なしで wrap するだけなので、信頼できる
    /// 入力 (DO storage 復元 / `private-<token>` パース後など) に限定して使う。
    pub fn from_raw<S: Into<String>>(raw: S) -> Self {
        Self(raw.into())
    }

    /// hex 文字列としての参照を返す (24 文字)。`private-<token>` を組み立てる側で
    /// prefix と連結する。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `Color` を `Serialize`/`Deserialize` 可能な形に橋渡しするローカル enum。
/// `rshogi_core::Color` は `serde` 非対応のため (workspace の core crate に
/// serde 依存を増やしたくない)、本モジュール内のみで変換する。Workers の
/// `lobby.rs::ColorTag` と同じ流儀。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorTag {
    Black,
    White,
}

impl ColorTag {
    pub fn from_core(c: Color) -> Self {
        match c {
            Color::Black => Self::Black,
            Color::White => Self::White,
        }
    }
    pub fn to_core(self) -> Color {
        match self {
            Self::Black => Color::Black,
            Self::White => Color::White,
        }
    }
}

/// challenge 1 件分の登録情報。永続データのみを持ち、TCP の runtime session
/// (cancel notify / transport responder) は別 map で管理する。
///
/// `inviter` / `opponent` は `String` で持つ (newtype `PlayerName` は serde 非対応で、
/// Workers `PersistedConfig.black_handle: String` と同じ慣習)。`inviter_color` は
/// [`ColorTag`] でラップして serde 対応にしている。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeEntry {
    /// 招待者 (発行者) handle。`%%CHALLENGE` 発行時の認証済 LOGIN handle、
    /// または `CHALLENGE_LOBBY` の引数で明示された handle (Workers self-claim)。
    pub inviter: String,
    /// 招待された相手の handle。LOGIN 時の照合キー。
    pub opponent: String,
    /// 招待者が `%%CHALLENGE` で指定した希望色。`None` は `+free` 指定で
    /// サーバが両者揃った時点で乱択。
    pub inviter_color: Option<ColorTag>,
    /// 対局時計設定。`clock_presets` 名で指定された preset を上位層が解決して
    /// ここに格納する。
    pub clock_spec: ClockSpec,
    /// 開始局面 SFEN。`None` は平手。
    pub initial_sfen: Option<String>,
    /// 期限切れ時刻 (UNIX epoch ミリ秒)。`created_at + ttl` を保持せず本フィールド
    /// だけで purge 判定する (二重管理を避ける)。Workers 既存
    /// `PersistedConfig.matched_at_ms` 等と単位を揃えて serialize 形式を簡潔に保つ
    /// (`chrono` の `serde` feature を要求しない)。
    pub expires_at_ms: u64,
    /// **Workers 専用**の永続フィールド: 片側 LOGIN 済の WS attachment id を
    /// handle (String) 単位で保持する。stale handle race を回避するため、purge /
    /// unmark は attachment id 単位で行う。
    /// TCP は本フィールドを使わず、frontend 側に別途 `TcpChallengePending`
    /// runtime map (`Arc<Notify>` / `oneshot::Sender<TcpTransport>`) を持つ
    /// (TCP の runtime 型は serialize 不能のため)。
    #[serde(default)]
    pub pending_ws_attachment_ids: HashMap<String, String>,
}

/// `ChallengeRegistry::issue` のエラー。clock_spec / sfen / opponent 存在の
/// 検証は上位 protocol/server 層の責務 (`CHALLENGE:incorrect <reason>` を
/// frontend が組み立てる)。core は受け取った値を信頼する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueError {
    /// `inviter == opponent` (case-sensitive exact match)。
    SelfChallenge,
}

/// challenge token → entry の登録簿。永続データのみを保持。Workers では DO
/// storage に丸ごと put/get、TCP では `Arc<Mutex<ChallengeRegistry>>` で in-memory
/// 共有する。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ChallengeRegistry {
    entries: HashMap<ChallengeToken, ChallengeEntry>,
}

impl ChallengeRegistry {
    /// 空の登録簿を作る。
    pub fn new() -> Self {
        Self::default()
    }

    /// challenge を登録して新しい token を返す。
    ///
    /// `inviter == opponent` は [`IssueError::SelfChallenge`] で弾く。
    /// 万が一 token が既存と衝突したら CSPRNG で再生成して retry する
    /// (96-bit エントロピーで birthday 衝突 2^48 並列まで耐えるが防御的に実装)。
    /// clock_spec / initial_sfen / opponent 存在の妥当性は上位層が事前検証
    /// 済の前提 (本 API は受け取った値を信頼する)。
    ///
    /// `now_ms` は UNIX epoch ミリ秒。`expires_at_ms = now_ms + ttl.as_millis()`
    /// で計算。`u128` から `u64` への切り詰めは `try_from` で安全側にサチュレート
    /// (実用上の TTL は秒〜時間オーダーで `u64::MAX` ms 到達は無いが防御的に書く)。
    pub fn issue(
        &mut self,
        inviter: PlayerName,
        opponent: PlayerName,
        inviter_color: Option<Color>,
        clock_spec: ClockSpec,
        initial_sfen: Option<String>,
        ttl: Duration,
        now_ms: u64,
    ) -> Result<ChallengeToken, IssueError> {
        if inviter.as_str() == opponent.as_str() {
            return Err(IssueError::SelfChallenge);
        }
        let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        let inviter = inviter.into_string();
        let opponent = opponent.into_string();
        let inviter_color = inviter_color.map(ColorTag::from_core);
        let token = loop {
            let candidate = ChallengeToken::generate();
            if !self.entries.contains_key(&candidate) {
                break candidate;
            }
        };
        let entry = ChallengeEntry {
            inviter,
            opponent,
            inviter_color,
            clock_spec,
            initial_sfen,
            expires_at_ms,
            pending_ws_attachment_ids: HashMap::new(),
        };
        self.entries.insert(token.clone(), entry);
        Ok(token)
    }

    /// 期限切れでない entry を返す。期限切れ entry は `None` (lookup 経由では
    /// 復活させない、purge は別途 [`Self::purge_expired`] で行う)。
    /// 境界: `expires_at_ms == now_ms` は期限切れ扱い ([`Self::purge_expired`]
    /// が `<=` で判定するのと対称)。
    pub fn lookup(&self, token: &ChallengeToken, now_ms: u64) -> Option<&ChallengeEntry> {
        let entry = self.entries.get(token)?;
        if entry.expires_at_ms > now_ms {
            Some(entry)
        } else {
            None
        }
    }

    /// **Workers 専用**: WS attachment id を handle に紐付ける。token が無効か
    /// 期限切れなら no-op。TCP は本 API を呼ばず、frontend 側 runtime map を
    /// 直接更新する。
    pub fn mark_ws_logged_in(
        &mut self,
        token: &ChallengeToken,
        handle: PlayerName,
        ws_attachment_id: String,
    ) {
        if let Some(entry) = self.entries.get_mut(token) {
            entry.pending_ws_attachment_ids.insert(handle.into_string(), ws_attachment_id);
        }
    }

    /// **Workers 専用**: 切断時に attachment id ごと unmark。指定 handle の
    /// 現在値が `ws_attachment_id` と一致する場合のみ削除 (stale handle race
    /// 回避: 別セッションが上書きしていたら触らない)。
    pub fn unmark_ws_logged_in(
        &mut self,
        token: &ChallengeToken,
        handle: &PlayerName,
        ws_attachment_id: &str,
    ) {
        if let Some(entry) = self.entries.get_mut(token) {
            let same = entry
                .pending_ws_attachment_ids
                .get(handle.as_str())
                .map(|s| s.as_str() == ws_attachment_id)
                .unwrap_or(false);
            if same {
                entry.pending_ws_attachment_ids.remove(handle.as_str());
            }
        }
    }

    /// マッチ成立時に entry を取り出して登録簿から削除する。期限切れ後の
    /// 呼び出しは `None` を返す (purge_expired で削除済の場合も同様)。
    /// 境界: `expires_at_ms == now_ms` は期限切れ扱いで `None` を返す
    /// ([`Self::lookup`] / [`Self::purge_expired`] と対称)。
    pub fn consume(&mut self, token: &ChallengeToken, now_ms: u64) -> Option<ChallengeEntry> {
        let valid = self.entries.get(token).map(|e| e.expires_at_ms > now_ms)?;
        if valid {
            self.entries.remove(token)
        } else {
            None
        }
    }

    /// 期限切れ entry を一括削除し、削除した entry の `Vec` を返す。呼び出し
    /// 側は戻り値の `pending_ws_attachment_ids` (Workers) や、TCP では別途
    /// 管理する runtime pending map から、先行 LOGIN 済 session を切断する
    /// 責務を持つ。
    pub fn purge_expired(&mut self, now_ms: u64) -> Vec<ChallengeEntry> {
        let expired_tokens: Vec<ChallengeToken> = self
            .entries
            .iter()
            .filter_map(|(t, e)| {
                if e.expires_at_ms <= now_ms {
                    Some(t.clone())
                } else {
                    None
                }
            })
            .collect();
        let mut removed = Vec::with_capacity(expired_tokens.len());
        for token in expired_tokens {
            if let Some(entry) = self.entries.remove(&token) {
                removed.push(entry);
            }
        }
        removed
    }

    /// 全 entry のうち最も近い `expires_at_ms` を返す。Workers の DO Alarm を
    /// 次回 purge 用に setAlarm するときに使う。空なら `None`。
    pub fn earliest_expiry_ms(&self) -> Option<u64> {
        self.entries.values().map(|e| e.expires_at_ms).min()
    }

    /// 登録件数 (テスト用)。
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 登録 entry が無いかどうか (テスト用、`len` と対称的に提供)。
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::clock::ClockSpec;

    fn fixed_clock() -> ClockSpec {
        ClockSpec::Countdown {
            total_time_sec: 600,
            byoyomi_sec: 10,
        }
    }

    /// テスト用基準時刻 (UNIX epoch ミリ秒): 2026-04-30T12:00:00Z 相当の固定値。
    const NOW_MS: u64 = 1_777_896_000_000;

    /// inviter と opponent が同一 handle の `%%CHALLENGE` は SelfChallenge で弾く。
    #[test]
    fn issue_rejects_self_challenge() {
        let mut reg = ChallengeRegistry::new();
        let err = reg
            .issue(
                PlayerName::new("alice"),
                PlayerName::new("alice"),
                Some(Color::Black),
                fixed_clock(),
                None,
                Duration::from_secs(3600),
                NOW_MS,
            )
            .unwrap_err();
        assert_eq!(err, IssueError::SelfChallenge);
    }

    /// 別 handle の `%%CHALLENGE` は token を返し、登録簿に entry が積まれる。
    #[test]
    fn issue_returns_token_for_distinct_handles() {
        let mut reg = ChallengeRegistry::new();
        let token = reg
            .issue(
                PlayerName::new("alice"),
                PlayerName::new("bob"),
                Some(Color::Black),
                fixed_clock(),
                None,
                Duration::from_secs(3600),
                NOW_MS,
            )
            .unwrap();
        assert_eq!(token.as_str().len(), 24);
        assert!(reg.lookup(&token, NOW_MS).is_some());
    }

    /// `consume` の境界: `now_ms == expires_at_ms` ちょうどは `None` を返す
    /// (`lookup` / `purge_expired` と対称、半開区間 `(expires_at_ms, ∞)` のみ生存)。
    #[test]
    fn consume_at_boundary_returns_none() {
        let mut reg = ChallengeRegistry::new();
        let token = reg
            .issue(
                PlayerName::new("alice"),
                PlayerName::new("bob"),
                None,
                fixed_clock(),
                None,
                Duration::from_secs(60),
                NOW_MS,
            )
            .unwrap();
        let boundary = NOW_MS + 60_000;
        assert!(reg.consume(&token, boundary).is_none());
        // 境界では entry が登録簿に残ったままで lookup も None を返す。
        assert!(reg.lookup(&token, boundary).is_none());
    }

    /// `consume` 後の lookup は `None`。
    #[test]
    fn consume_removes_entry() {
        let mut reg = ChallengeRegistry::new();
        let token = reg
            .issue(
                PlayerName::new("alice"),
                PlayerName::new("bob"),
                None,
                fixed_clock(),
                None,
                Duration::from_secs(3600),
                NOW_MS,
            )
            .unwrap();
        let consumed = reg.consume(&token, NOW_MS).expect("entry must exist");
        assert_eq!(consumed.inviter, "alice");
        assert_eq!(consumed.opponent, "bob");
        assert!(reg.lookup(&token, NOW_MS).is_none());
    }

    /// `purge_expired` は `expires_at_ms <= now_ms` の entry を削除し、削除済 entry を返す。
    /// 境界条件: `now_ms == expires_at_ms` は purge 対象 (`<=` で判定)。
    #[test]
    fn purge_expired_drops_entries_at_and_past_deadline() {
        let mut reg = ChallengeRegistry::new();
        let t = reg
            .issue(
                PlayerName::new("alice"),
                PlayerName::new("bob"),
                None,
                fixed_clock(),
                None,
                Duration::from_secs(60),
                NOW_MS,
            )
            .unwrap();
        // 60 秒前なら生存
        assert!(reg.lookup(&t, NOW_MS + 59_000).is_some());
        // 境界: now_ms + 60_000 ちょうど → purge 対象
        let boundary = NOW_MS + 60_000;
        let removed = reg.purge_expired(boundary);
        assert_eq!(removed.len(), 1);
        assert!(reg.lookup(&t, boundary).is_none());
    }

    /// `earliest_expiry_ms` は複数 entry の `min` を返す。
    #[test]
    fn earliest_expiry_returns_min_across_entries() {
        let mut reg = ChallengeRegistry::new();
        reg.issue(
            PlayerName::new("alice"),
            PlayerName::new("bob"),
            None,
            fixed_clock(),
            None,
            Duration::from_secs(3600),
            NOW_MS,
        )
        .unwrap();
        reg.issue(
            PlayerName::new("alice"),
            PlayerName::new("carol"),
            None,
            fixed_clock(),
            None,
            Duration::from_secs(60),
            NOW_MS,
        )
        .unwrap();
        let earliest = reg.earliest_expiry_ms().expect("two entries");
        assert_eq!(earliest, NOW_MS + 60_000);
    }

    /// `mark_ws_logged_in` / `unmark_ws_logged_in` の対称性。同じ handle に
    /// 別 attachment id が紐付いていたら削除しない (stale race 回避)。
    #[test]
    fn ws_attachment_id_unmark_is_session_scoped() {
        let mut reg = ChallengeRegistry::new();
        let t = reg
            .issue(
                PlayerName::new("alice"),
                PlayerName::new("bob"),
                None,
                fixed_clock(),
                None,
                Duration::from_secs(3600),
                NOW_MS,
            )
            .unwrap();
        reg.mark_ws_logged_in(&t, PlayerName::new("alice"), "ws-1".to_owned());
        let entry = reg.lookup(&t, NOW_MS).unwrap();
        assert_eq!(entry.pending_ws_attachment_ids.get("alice").map(String::as_str), Some("ws-1"));

        // 別 attachment id では unmark しない
        reg.unmark_ws_logged_in(&t, &PlayerName::new("alice"), "ws-other");
        assert!(reg.lookup(&t, NOW_MS).unwrap().pending_ws_attachment_ids.contains_key("alice"),);

        // 一致した attachment id では unmark する
        reg.unmark_ws_logged_in(&t, &PlayerName::new("alice"), "ws-1");
        assert!(!reg.lookup(&t, NOW_MS).unwrap().pending_ws_attachment_ids.contains_key("alice"),);
    }

    /// 連続 issue で異なる token が返る (token 衝突時 retry の周辺契約: 実用上
    /// 衝突は発生しないが API として保証する)。
    #[test]
    fn issue_returns_distinct_tokens_for_each_call() {
        let mut reg = ChallengeRegistry::new();
        let t1 = reg
            .issue(
                PlayerName::new("alice"),
                PlayerName::new("bob"),
                None,
                fixed_clock(),
                None,
                Duration::from_secs(3600),
                NOW_MS,
            )
            .unwrap();
        let t2 = reg
            .issue(
                PlayerName::new("alice"),
                PlayerName::new("carol"),
                None,
                fixed_clock(),
                None,
                Duration::from_secs(3600),
                NOW_MS,
            )
            .unwrap();
        assert_ne!(t1, t2);
        assert_eq!(reg.len(), 2);
    }
}
