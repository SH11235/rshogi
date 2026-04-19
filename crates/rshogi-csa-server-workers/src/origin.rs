//! Origin 許可リストの判定ロジック（純粋関数）。
//!
//! Cloudflare Workers から WebSocket Upgrade を受ける際の Origin ヘッダ検査を、
//! ランタイムから分離して単体テストできるようにする。
//!
//! **方針**:
//! - 完全一致のみ許可（ワイルドカードや部分一致を認めない）。
//! - 許可リストが空の場合は **安全側に倒して全拒否** する。運用ではデプロイ設定で
//!   `CORS_ORIGINS` を明示する前提。
//! - Origin ヘッダが欠落しているブラウザ以外のクライアント（CSA 互換クライアント等）を
//!   許可する場合は、別の認可経路（LOGIN パスワード等）で守る想定。このモジュールでは
//!   Origin の欠落を**拒否**として扱う。

/// Origin 判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginDecision {
    /// 許可リストに完全一致。
    Allow,
    /// Origin ヘッダ自体が付与されていない。
    Missing,
    /// Origin は付与されているが許可リストに存在しない。
    NotAllowed,
}

/// Origin ヘッダ値と許可リストを照合する。
///
/// # 引数
/// - `origin`: リクエストの `Origin` ヘッダ値（`Some("https://example.com")` など）。
/// - `allowed`: 許可する Origin の列（ホワイトリスト）。空なら全拒否。
///
/// # 戻り値
/// - [`OriginDecision::Allow`] なら Upgrade を許可してよい。
/// - それ以外は Upgrade を `403` 等で拒否する。
pub fn evaluate<'a, I>(origin: Option<&str>, allowed: I) -> OriginDecision
where
    I: IntoIterator<Item = &'a str>,
{
    let Some(origin) = origin else {
        return OriginDecision::Missing;
    };
    for entry in allowed {
        if entry == origin {
            return OriginDecision::Allow;
        }
    }
    OriginDecision::NotAllowed
}

/// カンマ区切り文字列 (`"https://a.example,https://b.example"`) を
/// 許可リストとして分解する。Cloudflare Workers の `[vars]` から単一文字列で
/// 設定値を渡す運用を想定した補助関数。前後空白はトリムする。
pub fn parse_allow_list(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_exact_match() {
        let allowed = ["https://a.example", "https://b.example"];
        assert_eq!(
            evaluate(Some("https://a.example"), allowed.iter().copied()),
            OriginDecision::Allow
        );
    }

    #[test]
    fn reject_non_matching_origin() {
        let allowed = ["https://a.example"];
        assert_eq!(
            evaluate(Some("https://evil.example"), allowed.iter().copied()),
            OriginDecision::NotAllowed
        );
    }

    #[test]
    fn reject_missing_origin_header() {
        let allowed = ["https://a.example"];
        assert_eq!(evaluate(None, allowed.iter().copied()), OriginDecision::Missing);
    }

    #[test]
    fn empty_allow_list_rejects_everything() {
        let allowed: [&str; 0] = [];
        assert_eq!(
            evaluate(Some("https://a.example"), allowed.iter().copied()),
            OriginDecision::NotAllowed
        );
    }

    #[test]
    fn scheme_and_host_must_match_exactly() {
        // プロトコル違いを拒否する (http vs https)。
        let allowed = ["https://a.example"];
        assert_eq!(
            evaluate(Some("http://a.example"), allowed.iter().copied()),
            OriginDecision::NotAllowed
        );
        // ポート違いを拒否する。
        assert_eq!(
            evaluate(Some("https://a.example:8443"), allowed.iter().copied()),
            OriginDecision::NotAllowed
        );
        // サブドメインの部分一致を拒否する（完全一致主義）。
        assert_eq!(
            evaluate(Some("https://sub.a.example"), allowed.iter().copied()),
            OriginDecision::NotAllowed
        );
    }

    #[test]
    fn parse_allow_list_splits_and_trims() {
        assert_eq!(
            parse_allow_list("https://a.example, https://b.example ,https://c.example"),
            vec![
                "https://a.example".to_owned(),
                "https://b.example".to_owned(),
                "https://c.example".to_owned(),
            ]
        );
    }

    #[test]
    fn parse_allow_list_ignores_empty_segments() {
        assert_eq!(parse_allow_list(",,https://a.example,,"), vec!["https://a.example".to_owned()]);
    }

    #[test]
    fn parse_allow_list_empty_input() {
        let result: Vec<String> = parse_allow_list("");
        assert!(result.is_empty());
    }
}
