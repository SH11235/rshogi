//! Floodgate 運用機能の gate 定義。
//!
//! Floodgate 系の運用機能（定期スケジューラ、レート差ベースペアリング、履歴
//! 永続化、`players.yaml` 互換レート保存、駒落ち対局、重複ログイン方針）を
//! 起動設定で明示的に opt-in させるための共通 validation を提供する。
//! 各 frontend は要求中の機能を [`FloodgateFeatureIntent`] に組み立て、
//! [`validate_floodgate_feature_gate`] に通してから機能を有効化する。

/// 起動時に Floodgate 系機能のうち何を有効化したいかを表す意図。
///
/// 現時点では各 frontend から `Default::default()` を渡すだけだが、Floodgate
/// 機能実装時に該当フラグを true にして gate へ接続する。実配線されていない
/// 機能はここに placeholder として足さず、接続するタイミングで追加する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FloodgateFeatureIntent {
    pub enable_scheduler: bool,
    pub use_non_direct_pairing: bool,
    pub enable_duplicate_login_policy: bool,
    /// Ruby shogi-server 互換 `players.yaml` 形式でレートを永続化する経路を
    /// 有効化する意図。`PlayersYamlRateStorage` を起動時に読み込み、終局時に
    /// 書き戻す経路を本フラグで gate する。
    pub enable_persistent_player_rates: bool,
}

/// 真偽文字列から Floodgate 機能 gate を解決する。
///
/// TCP frontend は clap が直接 `bool` にパースするため本関数を経由しない。
/// 環境変数など文字列経由で読む経路（Workers frontend など）で利用する。
/// 入力は前後空白を `trim` してから判定するため、`"true\n"` や `" 1 "` も
/// 許容する。
pub fn parse_allow_floodgate_features(raw: Option<&str>) -> Result<bool, String> {
    let normalized = raw.unwrap_or("false").trim();
    if normalized.eq_ignore_ascii_case("true")
        || normalized.eq_ignore_ascii_case("yes")
        || normalized.eq_ignore_ascii_case("on")
        || normalized == "1"
    {
        return Ok(true);
    }
    if normalized.eq_ignore_ascii_case("false")
        || normalized.eq_ignore_ascii_case("no")
        || normalized.eq_ignore_ascii_case("off")
        || normalized == "0"
    {
        return Ok(false);
    }
    Err(format!(
        "allow_floodgate_features: expected true|false|1|0|yes|no|on|off, got {normalized:?}"
    ))
}

/// Floodgate 機能が要求されているかを検証する。
///
/// フロントエンドに依らず、`allow_floodgate_features` が `false` のまま
/// Floodgate 系機能を要求した場合にエラーを返す。エラーメッセージは特定の
/// 環境変数名や CLI フラグ名に依存しない汎用形で返す（呼び出し側で
/// `ALLOW_FLOODGATE_FEATURES` / `--allow-floodgate-features` 等に読み替える）。
pub fn validate_floodgate_feature_gate(
    allow_floodgate_features: bool,
    intent: FloodgateFeatureIntent,
) -> Result<(), String> {
    let mut requested = Vec::new();
    if intent.enable_scheduler {
        requested.push("scheduler");
    }
    if intent.use_non_direct_pairing {
        requested.push("non_direct_pairing");
    }
    if intent.enable_duplicate_login_policy {
        requested.push("duplicate_login_policy");
    }
    if intent.enable_persistent_player_rates {
        requested.push("persistent_player_rates");
    }
    if requested.is_empty() || allow_floodgate_features {
        return Ok(());
    }
    Err(format!(
        "floodgate features require allow_floodgate_features=true: {}",
        requested.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_allow_floodgate_features_defaults_to_false() {
        assert!(!parse_allow_floodgate_features(None).unwrap());
    }

    #[test]
    fn parse_allow_floodgate_features_accepts_true() {
        assert!(parse_allow_floodgate_features(Some("true")).unwrap());
    }

    #[test]
    fn parse_allow_floodgate_features_rejects_unknown_value() {
        let err = parse_allow_floodgate_features(Some("weird")).unwrap_err();
        assert!(err.contains("allow_floodgate_features"));
    }

    #[test]
    fn parse_allow_floodgate_features_trims_surrounding_whitespace() {
        // 環境変数経由で `"true\n"` や `" 1 "` が渡ることがあるため、前後
        // 空白を吸収できる必要がある。
        assert!(parse_allow_floodgate_features(Some(" true ")).unwrap());
        assert!(parse_allow_floodgate_features(Some("true\n")).unwrap());
        assert!(parse_allow_floodgate_features(Some("\tYES\t")).unwrap());
        assert!(!parse_allow_floodgate_features(Some(" 0 ")).unwrap());
    }

    #[test]
    fn floodgate_gate_rejects_requested_feature_when_disabled() {
        let err = validate_floodgate_feature_gate(
            false,
            FloodgateFeatureIntent {
                enable_scheduler: true,
                ..FloodgateFeatureIntent::default()
            },
        )
        .unwrap_err();
        assert!(err.contains("scheduler"));
    }

    #[test]
    fn floodgate_gate_allows_requested_feature_when_enabled() {
        validate_floodgate_feature_gate(
            true,
            FloodgateFeatureIntent {
                enable_scheduler: true,
                ..FloodgateFeatureIntent::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn floodgate_gate_allows_all_defaults_when_disabled() {
        // gate off + 何も要求していない状態が既定の通常起動経路。この分岐で
        // Err を返すと全起動が失敗するため、明示的なカバレッジを維持する。
        validate_floodgate_feature_gate(false, FloodgateFeatureIntent::default()).unwrap();
    }

    #[test]
    fn floodgate_gate_error_lists_all_requested_features() {
        let err = validate_floodgate_feature_gate(
            false,
            FloodgateFeatureIntent {
                enable_scheduler: true,
                use_non_direct_pairing: true,
                enable_duplicate_login_policy: true,
                enable_persistent_player_rates: true,
            },
        )
        .unwrap_err();
        assert!(err.contains("scheduler"));
        assert!(err.contains("non_direct_pairing"));
        assert!(err.contains("duplicate_login_policy"));
        assert!(err.contains("persistent_player_rates"));
    }

    #[test]
    fn floodgate_gate_rejects_persistent_player_rates_when_disabled() {
        let err = validate_floodgate_feature_gate(
            false,
            FloodgateFeatureIntent {
                enable_persistent_player_rates: true,
                ..FloodgateFeatureIntent::default()
            },
        )
        .unwrap_err();
        assert!(err.contains("persistent_player_rates"));
    }
}
