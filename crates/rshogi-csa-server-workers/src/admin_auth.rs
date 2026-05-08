//! `ADMIN_API_TOKEN` ベースの admin 認可 helper (Floodgate audit
//! [#560](https://github.com/SH11235/rshogi/issues/560) で導入)。
//!
//! Worker 側で admin 権限を要求する経路 (将来の HTTP admin endpoint /
//! [#621](https://github.com/SH11235/rshogi/issues/621) の WS 内 `%%ADMIN <token>`
//! 等) が共通で踏む token 検証ロジックを 1 か所に集約する。token は Cloudflare
//! secret (`wrangler secret put ADMIN_API_TOKEN`) として配置し、Worker は
//! `env.var(ConfigKeys::ADMIN_API_TOKEN)` で読み出す (Cloudflare 仕様で
//! var / secret は同 namespace)。
//!
//! # 設計の論点
//!
//! - **HMAC は採用しない**: replay 対策や canonical string 設計を伴うと運用
//!   レビューコストが膨らむ一方で、Cloudflare 側で TLS / IP 制限 / Cloudflare
//!   Access (Zero Trust) を併用すれば static token + constant-time 比較で十分。
//! - **constant-time 比較**: token は攻撃者が当てにいく対象なので、長さ一致時の
//!   byte 比較は短絡せず [`subtle::ConstantTimeEq`] で xor を畳み込む。長さ自体
//!   は公開情報として扱う (秘密の長さを `len()` で leak しない契約に依存しない)。
//! - **fail-closed 既定**: secret 未設定 (Cloudflare secret 未配置、または
//!   local dev で意図的に `ADMIN_API_TOKEN` を空文字へ書き換え) は
//!   [`AdminAuthError::TokenNotConfigured`] を返し、呼び出し側は 404 / 拒否で
//!   fail-closed する。攻撃者に「admin 機能は存在するが token 未設定」を
//!   知らせない方針。`wrangler.toml.example` の placeholder
//!   (`local-dev-admin-token-placeholder`) は **local dev で friction なく
//!   admin 経路を通電させる** ための既知 dev token であり、production / staging
//!   へは絶対に持ち込まない (`tests/wrangler_environment_toml_consistency.rs`
//!   が混入を gate)。
//!
//! # ホスト target テスト方針
//!
//! [`verify_token_str`] は `worker::Env` 依存を持たない pure helper として実装し、
//! ホスト target でも `cargo test` から到達できる。Worker runtime に閉じる
//! [`verify_admin_token_str`] は wasm32 でのみコンパイルし、env 取得経路の
//! 配線確認は `wrangler dev` / staging deploy 経由で行う。

use std::fmt;

use subtle::ConstantTimeEq;

#[cfg(target_arch = "wasm32")]
use worker::Env;

#[cfg(target_arch = "wasm32")]
use crate::config::ConfigKeys;

/// admin 認可が失敗する原因を網羅する error enum。
///
/// 各 variant は呼び出し側の surface (HTTP endpoint / WS admin command 等) で
/// 適切な拒否応答に翻訳される想定。本 crate 内では Worker `Response` への変換
/// helper を提供しない (HTTP admin endpoint が登場した時点で呼び出し側に書く)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminAuthError {
    /// `ADMIN_API_TOKEN` secret が未設定 (Cloudflare secret 未登録、または
    /// local dev で placeholder が空文字)。fail-closed 経路で扱う。
    TokenNotConfigured,
    /// 提供された credential が空 (Authorization header 欠落 / `%%ADMIN` の
    /// token 部が空)。
    MissingCredential,
    /// token 比較が一致しなかった (長さ不一致 / 内容不一致)。
    TokenMismatch,
}

impl fmt::Display for AdminAuthError {
    /// 運用ログ向けの簡潔表記。token 値や提供長など秘密に依存する情報は
    /// 含めない (logging 経路で leak しない契約)。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::TokenNotConfigured => "admin token not configured",
            Self::MissingCredential => "admin credential missing",
            Self::TokenMismatch => "admin token mismatch",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for AdminAuthError {}

/// 提供 token と secret token を constant-time 比較する pure helper。
///
/// 判定順:
/// 1. `secret` が空 → [`AdminAuthError::TokenNotConfigured`] (秘密未設定)。
/// 2. `provided` が空 → [`AdminAuthError::MissingCredential`] (credential 欠落)。
/// 3. 長さ不一致 → [`AdminAuthError::TokenMismatch`] (長さは公開情報扱い)。
/// 4. 同長 → [`subtle::ConstantTimeEq`] で 1 byte ごと xor を畳み込み、
///    `Choice` の bool 化まで定数時間で完了する。
///
/// 両者空のケースは「秘密未設定」を優先 error として返す (404 fail-closed を
/// 意図する呼び出し側で credential 欠落と区別したい場合に備える)。
pub fn verify_token_str(provided: &str, secret: &str) -> Result<(), AdminAuthError> {
    if secret.is_empty() {
        return Err(AdminAuthError::TokenNotConfigured);
    }
    if provided.is_empty() {
        return Err(AdminAuthError::MissingCredential);
    }
    let p = provided.as_bytes();
    let s = secret.as_bytes();
    if p.len() != s.len() {
        return Err(AdminAuthError::TokenMismatch);
    }
    let eq: bool = p.ct_eq(s).into();
    if eq {
        Ok(())
    } else {
        Err(AdminAuthError::TokenMismatch)
    }
}

/// `%%ADMIN [<token>]` 形式の WS 行から token 部を抽出する pure helper
/// ([#621](https://github.com/SH11235/rshogi/issues/621))。
///
/// 戻り値:
/// - `None`: scheme 不一致 (`%%ADMINFOO` 等の prefix 衝突)、または非
///   `%%ADMIN` 行。呼び出し側はこの場合に通常コマンド経路へ落とす。
/// - `Some("")`: `%%ADMIN` 単体 / `%%ADMIN` の直後が whitespace のみ
///   (token 部欠落)。呼び出し側は本値を `verify_admin_token_str` に渡せば、
///   `MissingCredential` 経路で uniform に `PERMISSION_DENIED` が返る (
///   admin command を識別したことが silent vs response の差で leak しないよう、
///   `%%ADMIN <wrong>` と同じ応答を返す契約)。
/// - `Some(<token>)`: token 部 (前後 whitespace trim 済み)。
///
/// 共有 [`rshogi_csa_server::protocol::command::parse_command`] には乗せず
/// Workers 内で完結させる Workers 固有プロトコル拡張。共有 enum を増やすと
/// TCP frontend の match expression 全箇所に手を入れる必要があるため、本
/// コマンドは Workers のみで受理する設計を採る。TCP frontend には別途
/// `admin_handles` ベースの認可機構が既に存在する。
pub fn parse_admin_line(line: &str) -> Option<&str> {
    let trimmed = line.trim_end();
    let rest = trimmed.strip_prefix("%%ADMIN")?;
    // `%%ADMIN` 単体 (line 全体が `%%ADMIN` または `%%ADMIN` + 末尾 whitespace
    // のみ → trim_end で空) は token 部欠落として `Some("")` を返し、
    // 呼び出し側で uniform PERMISSION_DENIED 応答に統一する (Copilot review
    // 指摘: silent ignore は `%%ADMIN` vs `%%ADMIN <wrong>` の挙動差から
    // admin command の存在を leak する)。
    if rest.is_empty() {
        return Some("");
    }
    // `%%ADMIN` の直後が whitespace でなければ prefix 衝突 (`%%ADMINFOO` 等)
    // として `None` を返し、admin command として扱わない。
    let after_scheme = rest.strip_prefix(|c: char| c.is_ascii_whitespace())?;
    Some(after_scheme.trim())
}

/// Worker 環境変数から `ADMIN_API_TOKEN` を読み出し、提供 token と
/// constant-time 比較する。WS 内 admin command (`%%ADMIN <token>` 等) を
/// 想定した Worker hook。
///
/// secret 未登録時は [`AdminAuthError::TokenNotConfigured`]、`provided` が空文字
/// 時は [`AdminAuthError::MissingCredential`] を返す。詳細は
/// [`verify_token_str`] の判定順を参照。
#[cfg(target_arch = "wasm32")]
pub fn verify_admin_token_str(provided: &str, env: &Env) -> Result<(), AdminAuthError> {
    // `Err(_)` は「キー未設定」と Worker ランタイム側内部エラーを両方含むが、
    // どちらも fail-closed の `TokenNotConfigured` 経路に集約してよい (admin
    // 機能の存在 leak を防ぐ)。後続で worker::console_error! や tracing を
    // 挟みたい場合は `Err(e)` arm で hook できるよう match 形式で固定する。
    let secret = match env.var(ConfigKeys::ADMIN_API_TOKEN) {
        Ok(v) => v.to_string(),
        Err(_) => String::new(),
    };
    verify_token_str(provided, &secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_when_provided_equals_secret() {
        assert!(verify_token_str("rshogi-ops-abcd1234", "rshogi-ops-abcd1234").is_ok());
    }

    #[test]
    fn rejects_one_byte_diff_as_mismatch() {
        assert_eq!(
            verify_token_str("rshogi-ops-abcd1234", "rshogi-ops-abcd1235"),
            Err(AdminAuthError::TokenMismatch),
        );
    }

    #[test]
    fn rejects_length_diff_as_mismatch() {
        // 短い provided / 長い provided どちらも TokenMismatch にする (長さ leak は
        // 仕様で許容)。
        assert_eq!(
            verify_token_str("short", "rshogi-ops-abcd1234"),
            Err(AdminAuthError::TokenMismatch),
        );
        assert_eq!(
            verify_token_str("rshogi-ops-abcd1234-extra", "rshogi-ops-abcd1234"),
            Err(AdminAuthError::TokenMismatch),
        );
    }

    #[test]
    fn empty_secret_yields_token_not_configured() {
        assert_eq!(
            verify_token_str("rshogi-ops-abcd1234", ""),
            Err(AdminAuthError::TokenNotConfigured),
        );
    }

    #[test]
    fn empty_provided_yields_missing_credential_when_secret_set() {
        assert_eq!(
            verify_token_str("", "rshogi-ops-abcd1234"),
            Err(AdminAuthError::MissingCredential),
        );
    }

    #[test]
    fn both_empty_prefers_token_not_configured() {
        // 両者空時は「秘密未設定」を優先。404 fail-closed 経路で「endpoint 自体
        // を隠す」運用を素直に実現できるようにする。
        assert_eq!(verify_token_str("", ""), Err(AdminAuthError::TokenNotConfigured));
    }

    #[test]
    fn whitespace_provided_is_compared_verbatim() {
        // trim はしない。secret 側に空白を含む値を許す運用余地を残す
        // (上位レイヤで trim したい場合は呼び出し側で行う契約)。
        assert!(verify_token_str("  spaced  ", "  spaced  ").is_ok());
        assert_eq!(verify_token_str("spaced", "  spaced  "), Err(AdminAuthError::TokenMismatch),);
    }

    #[test]
    fn display_omits_token_or_credential_values() {
        // 運用ログで Display を経由して error を文字列化したときに、token 値や
        // credential 長さなど秘密に依存する情報が含まれないことを固定する。
        assert_eq!(format!("{}", AdminAuthError::TokenNotConfigured), "admin token not configured",);
        assert_eq!(format!("{}", AdminAuthError::MissingCredential), "admin credential missing");
        assert_eq!(format!("{}", AdminAuthError::TokenMismatch), "admin token mismatch");
    }

    #[test]
    fn parse_admin_line_accepts_canonical() {
        assert_eq!(parse_admin_line("%%ADMIN abc123"), Some("abc123"));
    }

    #[test]
    fn parse_admin_line_strips_trailing_crlf() {
        // WS 受信は通常 `trim_end_matches(['\r', '\n'])` 済だが、念のため pure
        // helper 側でも trailing whitespace を吸収する。
        assert_eq!(parse_admin_line("%%ADMIN abc123\r\n"), Some("abc123"));
        assert_eq!(parse_admin_line("%%ADMIN abc123  "), Some("abc123"));
    }

    #[test]
    fn parse_admin_line_handles_extra_inner_whitespace() {
        // %%ADMIN と token の間が複数 whitespace でも token は単一。
        assert_eq!(parse_admin_line("%%ADMIN   abc123"), Some("abc123"));
        assert_eq!(parse_admin_line("%%ADMIN\tabc123"), Some("abc123"));
    }

    #[test]
    fn parse_admin_line_returns_empty_for_missing_token() {
        // token 部欠落は `Some("")` を返し、呼び出し側で uniform PERMISSION_DENIED
        // に統一する。silent ignore は `%%ADMIN` vs `%%ADMIN <wrong>` の挙動差で
        // admin command の存在を leak するため避ける。
        assert_eq!(parse_admin_line("%%ADMIN"), Some(""));
        assert_eq!(parse_admin_line("%%ADMIN "), Some(""));
        assert_eq!(parse_admin_line("%%ADMIN   \t  "), Some(""));
        // CRLF も trim_end で削られて空扱いになる。
        assert_eq!(parse_admin_line("%%ADMIN\r\n"), Some(""));
    }

    #[test]
    fn parse_admin_line_rejects_prefix_collision() {
        // `%%ADMINFOO` のような prefix 衝突を弾く。
        assert_eq!(parse_admin_line("%%ADMINFOO bar"), None);
    }

    #[test]
    fn parse_admin_line_rejects_other_commands() {
        assert_eq!(parse_admin_line("%%CHAT abc"), None);
        assert_eq!(parse_admin_line("%%SETBUOY name"), None);
        assert_eq!(parse_admin_line(""), None);
        assert_eq!(parse_admin_line("ADMIN abc"), None);
    }
}
