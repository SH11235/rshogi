//! `WORKERS_HANDLE_AUTH` ベースの LOGIN handle 自称防止 helper (Floodgate audit
//! [#664](https://github.com/SH11235/rshogi/issues/664)、親
//! [#621](https://github.com/SH11235/rshogi/issues/621))。
//!
//! Workers の LOGIN / LOGIN_LOBBY 受理経路で「特定の handle を名乗る session に
//! 限って password SHA256 を要求する」whitelist を提供する。一般対局者 /
//! Floodgate 互換 client は whitelist に登録されていない handle で接続し、
//! 従来通り self-claim で素通しする (backward compat 最優先)。
//!
//! # 設計の論点
//!
//! - **whitelist 形式**: env (`WORKERS_HANDLE_AUTH`) に JSON 配列文字列で
//!   `[{"handle":"...","password_sha256":"..."}, ...]` を渡す。`password_sha256`
//!   は **lowercase hex 64 chars** に固定して入力サーフェスを狭くする (base64 や
//!   uppercase hex 等の別形式を増やすと parser バグや誤設定リスクが増えるため)。
//! - **constant-time 比較**: password hash は攻撃者が当てにいく対象なので、
//!   [`subtle::ConstantTimeEq`] で byte 単位の xor を畳み込む比較を使う。
//!   ハッシュ自体は固定長 (SHA256 = 32 bytes) なので長さ leak は仕様で許容。
//! - **fail-closed 既定**: env JSON parse 失敗 / 不正 entry / 重複 handle / hash
//!   形式不正は **全 LOGIN reject** で fail-closed する。設定不正で admin 自称が
//!   通る経路を絶つ。env が空 (`None` / `""` / `"[]"`) のみが「whitelist 未宣言」
//!   として self-claim 既定挙動を維持する。
//! - **private 経路は対象外**: `LOGIN_LOBBY <handle>+private-<token>+free` は
//!   challenge token 経由で `inviter` / `opponent` が決定論的に固定されるため、
//!   self-claim 自体が発生しない。本 module の whitelist 対象は **公開 LOGIN
//!   経路 (`LOGIN` / 公開 `LOGIN_LOBBY`) のみ**に絞る。
//!
//! # ホスト target テスト方針
//!
//! [`HandleAuthRegistry`] と [`HandleAuthRegistry::verify`] は `worker::Env`
//! 依存を持たない pure helper として実装し、ホスト target でも `cargo test`
//! から到達できる。Worker runtime に閉じる [`load_handle_auth_registry`] は
//! wasm32 でのみコンパイルし、env 取得経路の配線確認は `wrangler dev` /
//! staging deploy 経由で行う。

use std::collections::HashMap;
use std::fmt;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[cfg(target_arch = "wasm32")]
use worker::Env;

#[cfg(target_arch = "wasm32")]
use crate::config::ConfigKeys;

/// `password_sha256` の文字列形式 (lowercase hex 64 chars = SHA256 32 bytes)。
const PASSWORD_SHA256_HEX_LEN: usize = 64;
const PASSWORD_SHA256_BYTES: usize = 32;

/// LOGIN handle 自称防止 whitelist の 1 entry。
///
/// `handle` は LOGIN の `<handle>` 部 (例: `alice+game-eval+black` の `alice`)。
/// `password_sha256` は password を SHA256 でハッシュした **lowercase hex 64
/// 文字** 表現。env JSON に書く前提なので、`echo -n "<password>" | sha256sum` で
/// 生成した値をそのまま貼り付けられる入力サーフェスを採る。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct HandleAuthEntryRaw {
    handle: String,
    password_sha256: String,
}

/// 解決済 whitelist (handle → 期待 hash バイト列)。
///
/// `HashMap` に直して `verify` を O(1) に倒す。entry 数は admin operator 1〜2
/// 名想定で 1 桁の規模、handle 重複は parse 段階で error として弾く。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HandleAuthRegistry {
    entries: HashMap<String, [u8; PASSWORD_SHA256_BYTES]>,
}

/// whitelist 構築・検証時の error 網羅。
///
/// `Display` 実装は password / hash 値を含めず、handle 名のみを出す (運用ログで
/// 誤って hash 値が leak しないよう、admin_auth と同じ流儀)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleAuthError {
    /// env 値が JSON として parse できない、または期待 schema (配列 of object)
    /// にマッチしない。fail-closed で全 LOGIN reject。
    InvalidJson(String),
    /// 同一 handle が 2 件以上宣言された。意図的か誤設定か判別できないので
    /// fail-closed で全 LOGIN reject。
    DuplicateHandle(String),
    /// `password_sha256` が lowercase hex 64 chars 以外。
    InvalidHashFormat { handle: String },
    /// `verify` で提供 password の SHA256 が登録 hash と一致しなかった。
    PasswordMismatch,
    /// `requires_auth` が true だが env が未設定 (= 呼び出し側のロジックエラー、
    /// 通常経路では到達しない)。fail-closed で reject。
    NotConfigured,
}

impl fmt::Display for HandleAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(e) => write!(f, "handle_auth: invalid JSON: {e}"),
            Self::DuplicateHandle(h) => write!(f, "handle_auth: duplicate handle: {h:?}"),
            Self::InvalidHashFormat { handle } => {
                write!(f, "handle_auth: invalid hash format for handle: {handle:?}")
            }
            Self::PasswordMismatch => f.write_str("handle_auth: password mismatch"),
            Self::NotConfigured => f.write_str("handle_auth: registry not configured"),
        }
    }
}

impl std::error::Error for HandleAuthError {}

impl HandleAuthRegistry {
    /// env 文字列から whitelist を構築する。
    ///
    /// 受理する入力:
    /// - `None` / `Some("")` / `Some("   ")` / `Some("[]")` → 空 registry。
    ///   whitelist 未宣言モードとして self-claim 既定挙動を維持する。
    /// - `Some("[{...}, ...]")` → 各 entry の handle 重複 / hash 形式を検証して
    ///   `HashMap` に詰める。
    ///
    /// fail-closed 規約:
    /// - JSON parse 失敗 / schema 不一致 → [`HandleAuthError::InvalidJson`]
    /// - handle 重複 → [`HandleAuthError::DuplicateHandle`]
    /// - hash が lowercase hex 64 chars 以外 → [`HandleAuthError::InvalidHashFormat`]
    pub fn parse(raw: Option<&str>) -> Result<Self, HandleAuthError> {
        let trimmed = raw.unwrap_or("").trim();
        if trimmed.is_empty() {
            return Ok(Self::default());
        }
        let raw_entries: Vec<HandleAuthEntryRaw> = serde_json::from_str(trimmed)
            .map_err(|e| HandleAuthError::InvalidJson(e.to_string()))?;
        let mut entries: HashMap<String, [u8; PASSWORD_SHA256_BYTES]> =
            HashMap::with_capacity(raw_entries.len());
        for entry in raw_entries {
            if entry.handle.is_empty() {
                return Err(HandleAuthError::InvalidJson("empty handle entry".to_owned()));
            }
            let hash_bytes = parse_hex_sha256(&entry.password_sha256).ok_or_else(|| {
                HandleAuthError::InvalidHashFormat {
                    handle: entry.handle.clone(),
                }
            })?;
            if entries.contains_key(&entry.handle) {
                return Err(HandleAuthError::DuplicateHandle(entry.handle));
            }
            entries.insert(entry.handle, hash_bytes);
        }
        Ok(Self { entries })
    }

    /// whitelist が 1 件も登録されていないか。`true` のとき呼び出し側は
    /// `requires_auth` を呼ばずに self-claim 既定挙動へ落としてよい。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 指定 handle が whitelist に登録されているか。
    ///
    /// `true` を返したときに限り [`Self::verify`] で password 検証を行う契約。
    /// `false` の handle は self-claim で素通しする (一般対局者 / Floodgate
    /// 互換 client の backward compat 最優先)。
    pub fn requires_auth(&self, handle: &str) -> bool {
        self.entries.contains_key(handle)
    }

    /// 指定 handle + password を検証する。
    ///
    /// 判定順:
    /// 1. `requires_auth(handle) == false` → [`HandleAuthError::NotConfigured`]
    ///    (呼び出し側で `requires_auth` を先に確認するべき。fail-closed の安全網)
    /// 2. SHA256(password) を計算し、登録 hash と [`subtle::ConstantTimeEq`] で
    ///    constant-time 比較。
    /// 3. 一致 → `Ok(())`、不一致 → [`HandleAuthError::PasswordMismatch`]。
    pub fn verify(&self, handle: &str, password: &str) -> Result<(), HandleAuthError> {
        let Some(expected) = self.entries.get(handle) else {
            return Err(HandleAuthError::NotConfigured);
        };
        let mut hasher = Sha256::new();
        hasher.update(password.as_bytes());
        let actual = hasher.finalize();
        // SHA256 出力は固定長 32 bytes なので長さ leak は仕様で許容。
        let eq: bool = actual.as_slice().ct_eq(expected.as_slice()).into();
        if eq {
            Ok(())
        } else {
            Err(HandleAuthError::PasswordMismatch)
        }
    }
}

/// lowercase hex 64 chars を `[u8; 32]` に decode する。
///
/// `0-9 a-f` 以外の文字 (uppercase A-F 含む) と 64 chars 以外の長さは reject。
/// 入力サーフェスを狭めて env 値の typo を弾きやすくする目的。
fn parse_hex_sha256(s: &str) -> Option<[u8; PASSWORD_SHA256_BYTES]> {
    if s.len() != PASSWORD_SHA256_HEX_LEN {
        return None;
    }
    let mut out = [0u8; PASSWORD_SHA256_BYTES];
    let bytes = s.as_bytes();
    for (i, chunk) in bytes.chunks_exact(2).enumerate() {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        out[i] = (high << 4) | low;
    }
    Some(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// Worker 環境変数から `WORKERS_HANDLE_AUTH` を読み出し、[`HandleAuthRegistry`]
/// を構築する。WS 内 LOGIN 受理経路から都度呼ばれる前提で、cache はしない
/// (`clock_presets` / rate_limit thresholds と同じ流儀: env 取得 1 回 + parse
/// 1 回。LOGIN は人手駆動で頻度が低くホットパスではない)。
///
/// secret 未登録時は空 registry を返し、whitelist 未宣言モードとして self-claim
/// 既定挙動に落とす。secret は登録済だが内容が不正な場合は
/// [`HandleAuthError`] を返し、呼び出し側は fail-closed で全 LOGIN reject する。
#[cfg(target_arch = "wasm32")]
pub fn load_handle_auth_registry(env: &Env) -> Result<HandleAuthRegistry, HandleAuthError> {
    let raw = env.var(ConfigKeys::WORKERS_HANDLE_AUTH).ok().map(|v| v.to_string());
    HandleAuthRegistry::parse(raw.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `echo -n "correct_horse_battery_staple" | sha256sum` 相当。
    /// fixture をテストに直接書く (環境依存を避ける)。
    const PASSWORD: &str = "correct_horse_battery_staple";
    const PASSWORD_HASH: &str = "6e9b54475e7e568f848f7c302c6d899d85c1118dd39b7b46272ba0b1d9b10c43";

    #[test]
    fn parse_none_yields_empty_registry() {
        let reg = HandleAuthRegistry::parse(None).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn parse_empty_string_yields_empty_registry() {
        assert!(HandleAuthRegistry::parse(Some("")).unwrap().is_empty());
        assert!(HandleAuthRegistry::parse(Some("   ")).unwrap().is_empty());
        assert!(HandleAuthRegistry::parse(Some("[]")).unwrap().is_empty());
    }

    #[test]
    fn parse_single_entry_round_trips() {
        let raw = format!(r#"[{{"handle":"alice","password_sha256":"{PASSWORD_HASH}"}}]"#);
        let reg = HandleAuthRegistry::parse(Some(&raw)).unwrap();
        assert!(!reg.is_empty());
        assert!(reg.requires_auth("alice"));
        assert!(!reg.requires_auth("bob"));
    }

    #[test]
    fn parse_rejects_invalid_json() {
        let err = HandleAuthRegistry::parse(Some("not json")).unwrap_err();
        assert!(matches!(err, HandleAuthError::InvalidJson(_)));
    }

    #[test]
    fn parse_rejects_non_array_schema() {
        // 配列ではなく単一 object のような誤設定も InvalidJson で弾く。
        let err = HandleAuthRegistry::parse(Some(r#"{"handle":"a","password_sha256":"00"}"#))
            .unwrap_err();
        assert!(matches!(err, HandleAuthError::InvalidJson(_)));
    }

    #[test]
    fn parse_rejects_duplicate_handle() {
        let raw = format!(
            r#"[
                {{"handle":"alice","password_sha256":"{PASSWORD_HASH}"}},
                {{"handle":"alice","password_sha256":"{PASSWORD_HASH}"}}
            ]"#
        );
        let err = HandleAuthRegistry::parse(Some(&raw)).unwrap_err();
        match err {
            HandleAuthError::DuplicateHandle(h) => assert_eq!(h, "alice"),
            other => panic!("expected DuplicateHandle, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_empty_handle() {
        let raw = format!(r#"[{{"handle":"","password_sha256":"{PASSWORD_HASH}"}}]"#);
        let err = HandleAuthRegistry::parse(Some(&raw)).unwrap_err();
        assert!(matches!(err, HandleAuthError::InvalidJson(_)));
    }

    #[test]
    fn parse_rejects_short_hash() {
        // 32 chars hex は短すぎ。
        let raw = r#"[{"handle":"alice","password_sha256":"deadbeef"}]"#;
        let err = HandleAuthRegistry::parse(Some(raw)).unwrap_err();
        match err {
            HandleAuthError::InvalidHashFormat { handle } => assert_eq!(handle, "alice"),
            other => panic!("expected InvalidHashFormat, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_uppercase_hash() {
        // lowercase hex 固定。uppercase は形式違反として弾く。
        let upper = PASSWORD_HASH.to_uppercase();
        let raw = format!(r#"[{{"handle":"alice","password_sha256":"{upper}"}}]"#);
        let err = HandleAuthRegistry::parse(Some(&raw)).unwrap_err();
        assert!(matches!(err, HandleAuthError::InvalidHashFormat { .. }));
    }

    #[test]
    fn parse_rejects_non_hex_chars() {
        let raw = format!(
            r#"[{{"handle":"alice","password_sha256":"{}"}}]"#,
            "z".repeat(PASSWORD_SHA256_HEX_LEN)
        );
        let err = HandleAuthRegistry::parse(Some(&raw)).unwrap_err();
        assert!(matches!(err, HandleAuthError::InvalidHashFormat { .. }));
    }

    #[test]
    fn verify_accepts_correct_password() {
        let raw = format!(r#"[{{"handle":"alice","password_sha256":"{PASSWORD_HASH}"}}]"#);
        let reg = HandleAuthRegistry::parse(Some(&raw)).unwrap();
        assert_eq!(reg.verify("alice", PASSWORD), Ok(()));
    }

    #[test]
    fn verify_rejects_wrong_password() {
        let raw = format!(r#"[{{"handle":"alice","password_sha256":"{PASSWORD_HASH}"}}]"#);
        let reg = HandleAuthRegistry::parse(Some(&raw)).unwrap();
        assert_eq!(reg.verify("alice", "wrong"), Err(HandleAuthError::PasswordMismatch),);
    }

    #[test]
    fn verify_rejects_unknown_handle() {
        // `requires_auth(bob) == false` を呼び出し側が見落として verify を呼んだ
        // 場合は NotConfigured で fail-closed する。
        let raw = format!(r#"[{{"handle":"alice","password_sha256":"{PASSWORD_HASH}"}}]"#);
        let reg = HandleAuthRegistry::parse(Some(&raw)).unwrap();
        assert_eq!(reg.verify("bob", PASSWORD), Err(HandleAuthError::NotConfigured));
    }

    #[test]
    fn requires_auth_distinguishes_registered_handles() {
        let raw = format!(
            r#"[
                {{"handle":"alice","password_sha256":"{PASSWORD_HASH}"}},
                {{"handle":"floodgate","password_sha256":"{PASSWORD_HASH}"}}
            ]"#
        );
        let reg = HandleAuthRegistry::parse(Some(&raw)).unwrap();
        assert!(reg.requires_auth("alice"));
        assert!(reg.requires_auth("floodgate"));
        assert!(!reg.requires_auth("bob"));
        assert!(!reg.requires_auth(""));
    }

    #[test]
    fn parse_hex_sha256_round_trip() {
        let bytes = parse_hex_sha256(PASSWORD_HASH).unwrap();
        // 既知の固定 hash の最初の 2 byte を sanity check (typo 検出のため)。
        assert_eq!(bytes[0], 0x6e);
        assert_eq!(bytes[1], 0x9b);
    }

    #[test]
    fn display_omits_password_or_hash_values() {
        // 運用ログで Display を経由して error を文字列化したとき、password 値や
        // hash の生バイトが含まれないこと。handle 名は出してよい (運用者が
        // 誰の設定ミスかを特定するために必要)。
        let err = HandleAuthError::PasswordMismatch;
        let s = format!("{err}");
        assert!(s.contains("password mismatch"));
        assert!(!s.contains(PASSWORD));
        assert!(!s.contains(PASSWORD_HASH));
    }

    /// SHA256 計算ロジックが test fixture の hash 文字列と整合することを 1 件で
    /// 固定する。`PASSWORD_HASH` を後で書き換えるときに silent に通らないよう
    /// invariant として残す。
    #[test]
    fn sha256_of_fixture_password_matches_hash_constant() {
        let raw = format!(r#"[{{"handle":"alice","password_sha256":"{PASSWORD_HASH}"}}]"#);
        let reg = HandleAuthRegistry::parse(Some(&raw)).unwrap();
        // verify が成功するということは `SHA256(PASSWORD) == PASSWORD_HASH (bytes)`。
        assert!(reg.verify("alice", PASSWORD).is_ok());
    }
}
