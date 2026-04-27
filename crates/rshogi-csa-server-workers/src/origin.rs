//! Origin 許可リストの判定ロジック（純粋関数）。
//!
//! Cloudflare Workers から WebSocket Upgrade を受ける際の Origin ヘッダ検査を、
//! ランタイムから分離して単体テストできるようにする。
//!
//! **方針**:
//! - Origin ヘッダが付与されている場合は **完全一致のみ許可**（ワイルドカードや
//!   部分一致を認めない）。これはブラウザ経由のリクエストに対する CSRF 防御層として
//!   機能する。
//! - Origin ヘッダが欠落しているリクエスト（ネイティブ CSA クライアント等、
//!   ブラウザではない経路）は許可する。Origin はブラウザがフェッチ規格に従って
//!   自動付与するヘッダなので、欠落 = ブラウザ起源ではないシグナル。これらの経路は
//!   LOGIN ハンドル / パスワード等の別レイヤで認可する前提。
//! - 許可リストが空の場合、Origin 付きリクエストはすべて拒否される（ブラウザ経由は
//!   全拒否）。Origin 欠落リクエストは引き続き素通しになる。

/// Origin 判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginDecision {
    /// 許可リストに完全一致した、または Origin ヘッダが欠落していて素通しを許可。
    Allow,
    /// Origin は付与されているが許可リストに存在しない。
    NotAllowed,
}

/// Origin ヘッダ値と許可リストを照合する。
///
/// # 引数
/// - `origin`: リクエストの `Origin` ヘッダ値（`Some("https://example.com")` など）。
///   `None` はネイティブ CSA クライアント等の非ブラウザ経路として許可する。
/// - `allowed`: 許可する Origin の列（ホワイトリスト）。空のときも Origin 欠落は
///   素通し、Origin 付きはすべて拒否。
///
/// # 戻り値
/// - [`OriginDecision::Allow`] なら Upgrade を許可してよい。
/// - [`OriginDecision::NotAllowed`] は Upgrade を `403` 等で拒否する。
pub fn evaluate<'a, I>(origin: Option<&str>, allowed: I) -> OriginDecision
where
    I: IntoIterator<Item = &'a str>,
{
    let Some(origin) = origin else {
        return OriginDecision::Allow;
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
    fn allow_missing_origin_header() {
        // ネイティブ CSA クライアント等 Origin ヘッダを送らない経路は素通しにする。
        let allowed = ["https://a.example"];
        assert_eq!(evaluate(None, allowed.iter().copied()), OriginDecision::Allow);
    }

    #[test]
    fn allow_missing_origin_with_empty_allow_list() {
        // 許可リストが空でも Origin 欠落は素通しになる（CSRF は Origin 付きにのみ効く）。
        let allowed: [&str; 0] = [];
        assert_eq!(evaluate(None, allowed.iter().copied()), OriginDecision::Allow);
    }

    #[test]
    fn empty_allow_list_rejects_origin_present() {
        // Origin が付いているのに許可リストが空 → ブラウザ経由全拒否。
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
