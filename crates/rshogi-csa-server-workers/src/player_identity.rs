//! 公開用 player ID と versioned HMAC keyring の純粋ロジック。

use std::collections::BTreeMap;

use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const ID_BYTES: usize = 20;
pub const MIN_SECRET_BYTES: usize = 32;
/// D1 alias lookup/batch の bind・statement 数を一定に保つ運用上限。
pub const MAX_KEYRING_KEYS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerIdentity {
    pub player_id: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyringError {
    Missing,
    InvalidJson,
    InvalidVersion,
    ActiveVersionMissing,
    SecretNotString,
    SecretWhitespace,
    SecretTooShort,
    TooManyKeys,
}

impl KeyringError {
    /// secret 値を含まない構造化ログ用 reason。
    pub fn reason(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::InvalidJson => "invalid_json",
            Self::InvalidVersion => "invalid_version",
            Self::ActiveVersionMissing => "active_version_missing",
            Self::SecretNotString => "secret_not_string",
            Self::SecretWhitespace => "secret_whitespace",
            Self::SecretTooShort => "secret_too_short",
            Self::TooManyKeys => "too_many_keys",
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawKeyring {
    active_version: String,
    keys: BTreeMap<String, serde_json::Value>,
}

/// 単一 secret（旧形式）または versioned JSON keyring から ID と alias 群を導出する。
///
/// 旧 string 形式は既存 `p_<digest>` を canonical のまま維持する。JSON 形式は
/// `p_<version>_<digest>` を canonical とし、keyring 内の全旧 version ID と、v1
/// secret から導出できる旧 `p_<digest>` を alias として返す。
pub fn derive_player_identity(
    handle: &str,
    password: &str,
    raw: Option<&str>,
) -> Result<PlayerIdentity, KeyringError> {
    let raw = raw.ok_or(KeyringError::Missing)?;
    if !raw.trim_start().starts_with('{') {
        validate_secret(raw)?;
        return Ok(PlayerIdentity {
            player_id: credential_id(handle, password, raw, None),
            aliases: Vec::new(),
        });
    }

    let parsed: RawKeyring = serde_json::from_str(raw).map_err(|_| KeyringError::InvalidJson)?;
    if !valid_version(&parsed.active_version) {
        return Err(KeyringError::InvalidVersion);
    }
    if parsed.keys.len() > MAX_KEYRING_KEYS {
        return Err(KeyringError::TooManyKeys);
    }
    let mut keys = BTreeMap::new();
    for (version, value) in parsed.keys {
        if !valid_version(&version) {
            return Err(KeyringError::InvalidVersion);
        }
        let secret = value.as_str().ok_or(KeyringError::SecretNotString)?;
        validate_secret(secret)?;
        keys.insert(version, secret.to_owned());
    }
    let active_secret =
        keys.get(&parsed.active_version).ok_or(KeyringError::ActiveVersionMissing)?;
    let player_id = credential_id(handle, password, active_secret, Some(&parsed.active_version));
    let mut aliases = Vec::new();
    for (version, secret) in &keys {
        let id = credential_id(handle, password, secret, Some(version));
        if id != player_id {
            aliases.push(id);
        }
        if version == "v1" {
            // 旧単一 string secret が生成した unversioned ID の移行 alias。
            let old = credential_id(handle, password, secret, None);
            if old != player_id {
                aliases.push(old);
            }
        }
    }
    aliases.sort();
    aliases.dedup();
    Ok(PlayerIdentity { player_id, aliases })
}

pub fn legacy_player_id(handle: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"rshogi-player-id:legacy:v1\0");
    hasher.update(handle.as_bytes());
    let digest = hasher.finalize();
    format!("legacy_{}", hex_prefix(&digest, ID_BYTES))
}

fn validate_secret(secret: &str) -> Result<(), KeyringError> {
    if secret.trim() != secret {
        return Err(KeyringError::SecretWhitespace);
    }
    // `str::len` is the UTF-8 byte length, matching the workflow's
    // `utf8bytelength` validation.
    if secret.len() < MIN_SECRET_BYTES {
        return Err(KeyringError::SecretTooShort);
    }
    Ok(())
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 16
        && version.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn credential_id(handle: &str, password: &str, secret: &str, version: Option<&str>) -> String {
    // Stream credential segments into HMAC so password bytes are never copied
    // into an intermediate heap allocation. Segment boundaries exactly match
    // the original wire-ID message and therefore preserve existing IDs.
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 accepts any key length");
    mac.update(b"rshogi-player-id:v1\0");
    mac.update(handle.as_bytes());
    mac.update(&[0]);
    mac.update(password.as_bytes());
    let digest: [u8; 32] = mac.finalize().into_bytes().into();
    match version {
        Some(version) => format!("p_{version}_{}", hex_prefix(&digest, ID_BYTES)),
        None => format!("p_{}", hex_prefix(&digest, ID_BYTES)),
    }
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(len * 2);
    for byte in bytes.iter().take(len) {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &str = "0123456789abcdef0123456789abcdef";
    const V2: &str = "abcdef0123456789abcdef0123456789";

    #[test]
    fn legacy_string_secret_keeps_unversioned_id() {
        let id = derive_player_identity("alice", "password", Some(V1)).unwrap();
        assert_eq!(id.player_id, "p_ec682f038394accea905a4f5863aba9b4295c08f");
        assert_eq!(id.player_id.len(), 42);
        assert!(id.aliases.is_empty());
    }

    #[test]
    fn rotation_maps_versioned_and_old_unversioned_ids_to_active() {
        let before = derive_player_identity("alice", "password", Some(V1)).unwrap();
        let raw = format!(r#"{{"active_version":"v2","keys":{{"v1":"{V1}","v2":"{V2}"}}}}"#);
        let after = derive_player_identity("alice", "password", Some(&raw)).unwrap();
        assert!(after.player_id.starts_with("p_v2_"));
        assert!(after.aliases.iter().any(|id| id == &before.player_id));
        assert!(after.aliases.iter().any(|id| id.starts_with("p_v1_")));
    }

    #[test]
    fn rejects_missing_short_whitespace_and_invalid_keyrings_without_exposing_values() {
        assert_eq!(derive_player_identity("a", "p", None), Err(KeyringError::Missing));
        assert_eq!(
            derive_player_identity("a", "p", Some("short")),
            Err(KeyringError::SecretTooShort)
        );
        let padded = format!(" {V1}");
        assert_eq!(
            derive_player_identity("a", "p", Some(&padded)),
            Err(KeyringError::SecretWhitespace)
        );
        for raw in [
            r#"{"active_version":"V1","keys":{}}"#,
            r#"{"active_version":"v2","keys":{"v1":"0123456789abcdef0123456789abcdef"}}"#,
            r#"{"active_version":"v1","keys":{"v1":123}}"#,
            r#"{"active_version":"v1","keys":{"v1":"0123456789abcdef0123456789abcdef","v0":"short"}}"#,
            r#"{"active_version":"v1","keys":{"v1":"0123456789abcdef0123456789abcdef "}}"#,
        ] {
            assert!(derive_player_identity("a", "p", Some(raw)).is_err());
        }

        let keys: BTreeMap<String, serde_json::Value> = (0..=MAX_KEYRING_KEYS)
            .map(|index| (format!("v{index}"), serde_json::Value::String(V1.to_owned())))
            .collect();
        let too_many = serde_json::json!({"active_version": "v0", "keys": keys}).to_string();
        assert_eq!(
            derive_player_identity("a", "p", Some(&too_many)),
            Err(KeyringError::TooManyKeys)
        );
    }
}
