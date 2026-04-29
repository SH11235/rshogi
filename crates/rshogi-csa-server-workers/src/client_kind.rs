//! `X-Client` ヘッダ値を運用ログ向けに正規化する純粋ロジック。
//!
//! Issue #564 設計 v4 §3 に対応。viewer 配信 API のレスポンスログに
//! `client_kind=<kind>` として埋め込むため、任意文字列が logfmt を破壊しない
//! よう kebab-case ASCII (`[a-z0-9-]`) のみを許可するホワイトリスト方式で
//! 正規化する。
//!
//! # フォーマット契約
//! `<client>/<version>` 形式の `<client>` 部分のみを抽出する。`/` を含まない
//! 場合は全体を `<client>` とみなす。
//!
//! # 戻り値
//! - `Some(raw)` で kebab-case ASCII かつ 1..=64 文字 → 抽出した kind
//! - `Some(raw)` で正規化規則に違反 → `"invalid"`
//! - `None` (ヘッダ欠落) → `"unknown"`
//!
//! 戻り値はそのまま logfmt の値として埋め込める ASCII 文字列であることを保証
//! する。`viewer_api.rs` の `extract_client_kind` から呼ばれる。
//!
//! Workers ランタイム (`worker::Request`) は wasm32 専用だが、本モジュールは
//! 純粋関数のためホスト target でもテスト可能。`worker::Request` 依存部分は
//! `viewer_api::extract_client_kind` 側に留める。

/// `client_kind` の最大長。logfmt にそのまま流すため過大な値を弾く。
pub const MAX_CLIENT_KIND_LEN: usize = 64;

/// `Some("ramu-shogi-web/0.1.0")` 等の `X-Client` ヘッダ値を `client_kind`
/// 文字列に正規化する。
///
/// - `None` → `"unknown"` (ヘッダ未送信)
/// - `<kind>` 部分 (`/` までの prefix) が空 / 65 文字以上 / kebab-case ASCII
///   外を含む → `"invalid"`
/// - それ以外 → `<kind>` 部分をそのまま返す
pub fn normalize_client_kind(raw: Option<&str>) -> String {
    let Some(raw) = raw else {
        return "unknown".to_owned();
    };
    // `<kind>/<version>` の `<kind>` 側のみを抽出。`/` が無い場合は全体が `<kind>`。
    let kind = raw.split('/').next().unwrap_or("");
    if kind.is_empty() || kind.len() > MAX_CLIENT_KIND_LEN {
        return "invalid".to_owned();
    }
    // kebab-case ASCII のみ許可。logfmt 破壊文字 (`=` `空白` `\n` `\r` 等) を排除。
    if !kind.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return "invalid".to_owned();
    }
    kind.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_missing_returns_unknown() {
        assert_eq!(normalize_client_kind(None), "unknown");
    }

    #[test]
    fn lowercase_ascii_kind_is_passed_through() {
        assert_eq!(normalize_client_kind(Some("ramu-shogi-web")), "ramu-shogi-web");
        assert_eq!(normalize_client_kind(Some("ramu-shogi-desktop")), "ramu-shogi-desktop");
        assert_eq!(normalize_client_kind(Some("rshogi-csa-cli")), "rshogi-csa-cli");
        assert_eq!(normalize_client_kind(Some("client123")), "client123");
    }

    #[test]
    fn kind_version_form_extracts_only_kind() {
        assert_eq!(normalize_client_kind(Some("ramu-shogi-web/0.1.0")), "ramu-shogi-web");
        assert_eq!(
            normalize_client_kind(Some("ramu-shogi-desktop/1.2.3-beta")),
            "ramu-shogi-desktop"
        );
        // `/` 直後が空でも kind が有効なら通す。
        assert_eq!(normalize_client_kind(Some("ramu-shogi-web/")), "ramu-shogi-web");
    }

    #[test]
    fn uppercase_letters_are_invalid() {
        assert_eq!(normalize_client_kind(Some("Ramu-Shogi-Web")), "invalid");
        assert_eq!(normalize_client_kind(Some("RAMU-SHOGI")), "invalid");
        assert_eq!(normalize_client_kind(Some("Ramu/0.1.0")), "invalid");
    }

    #[test]
    fn whitespace_equals_and_newline_are_invalid() {
        // logfmt 破壊文字をすべて invalid にする。
        assert_eq!(normalize_client_kind(Some("ramu shogi")), "invalid");
        assert_eq!(normalize_client_kind(Some("a=b")), "invalid");
        assert_eq!(normalize_client_kind(Some("ramu\nshogi")), "invalid");
        assert_eq!(normalize_client_kind(Some("ramu\rshogi")), "invalid");
        assert_eq!(normalize_client_kind(Some("ramu\tshogi")), "invalid");
    }

    #[test]
    fn over_max_length_is_invalid() {
        // 65 文字 (MAX_CLIENT_KIND_LEN + 1) は invalid。
        let too_long = "a".repeat(MAX_CLIENT_KIND_LEN + 1);
        assert_eq!(normalize_client_kind(Some(&too_long)), "invalid");
        // ちょうど 64 文字は許可。
        let just_fit = "a".repeat(MAX_CLIENT_KIND_LEN);
        assert_eq!(normalize_client_kind(Some(&just_fit)), just_fit);
        // 65 文字目以降が `/` で切り落とされても kind 側が 65 文字なら invalid。
        let long_with_version = format!("{}/0.1.0", "a".repeat(MAX_CLIENT_KIND_LEN + 1));
        assert_eq!(normalize_client_kind(Some(&long_with_version)), "invalid");
    }

    #[test]
    fn empty_kind_is_invalid() {
        // ヘッダ自体は存在するが値が空 / `/` 始まりで kind 部分が空。
        assert_eq!(normalize_client_kind(Some("")), "invalid");
        assert_eq!(normalize_client_kind(Some("/0.1.0")), "invalid");
    }

    #[test]
    fn non_ascii_or_special_punctuation_is_invalid() {
        // 日本語やマルチバイトは invalid (logfmt safety)。
        assert_eq!(normalize_client_kind(Some("らむ将棋")), "invalid");
        // `_` `.` `:` 等の他の punctuation も kebab-case 外なので invalid。
        assert_eq!(normalize_client_kind(Some("ramu_shogi")), "invalid");
        assert_eq!(normalize_client_kind(Some("ramu.shogi")), "invalid");
        assert_eq!(normalize_client_kind(Some("ramu:shogi")), "invalid");
    }
}
