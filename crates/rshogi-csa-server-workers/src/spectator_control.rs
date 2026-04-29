//! 観戦セッション制御の純粋ヘルパ。
//!
//! room 固定の WebSocket route と、CSA x1 由来の `game_id` 指定コマンドの対応を
//! ここに閉じ込める。host target の単体テストで `%%MONITOR2ON/OFF` の判定を
//! 固定し、wasm32 専用の `GameRoom` 側は結果の送信だけに集中させる。

/// `%%MONITOR2ON/OFF` 解決結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorDecision<'a> {
    /// 現在の観戦対象として受理する。
    Accept {
        /// 応答に載せる識別子。active な `game_id` があればそれを優先し、無ければ
        /// `room_id` を返す。
        monitor_id: &'a str,
    },
    /// 指定 ID が現在の room / game と一致しない。
    NotFound {
        /// クライアントが要求した識別子。
        requested: &'a str,
    },
}

/// 観戦コマンドの対象 ID を解決する。
pub fn resolve_monitor_target<'a>(
    room_id: &'a str,
    active_game_id: Option<&'a str>,
    requested: &'a str,
) -> MonitorDecision<'a> {
    if requested == room_id || active_game_id == Some(requested) {
        MonitorDecision::Accept {
            monitor_id: active_game_id.unwrap_or(room_id),
        }
    } else {
        MonitorDecision::NotFound { requested }
    }
}

/// 終局済 DO への観戦アクセスを許可する拡張ラッパー。
///
/// `active_game_id` のみを参照する [`resolve_monitor_target`] に対し、終局済
/// (`KEY_FINISHED` set) DO の `cfg.game_id` も `Accept` 対象として加える。
///
/// 引数:
/// - `finished_game_id`: 終局済 DO の場合の `cfg.game_id` を `Some` で渡す。
///   active な対局が無く finished も無い (そもそも対局していない) DO では `None`。
///
/// monitor_id の優先順位は `active_game_id` → `finished_game_id` → `room_id`。
pub fn resolve_monitor_target_with_finished<'a>(
    room_id: &'a str,
    active_game_id: Option<&'a str>,
    finished_game_id: Option<&'a str>,
    requested: &'a str,
) -> MonitorDecision<'a> {
    if requested == room_id
        || active_game_id == Some(requested)
        || finished_game_id == Some(requested)
    {
        MonitorDecision::Accept {
            monitor_id: active_game_id.or(finished_game_id).unwrap_or(room_id),
        }
    } else {
        MonitorDecision::NotFound { requested }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_id_is_accepted_before_match() {
        assert_eq!(
            resolve_monitor_target("room-1", None, "room-1"),
            MonitorDecision::Accept {
                monitor_id: "room-1",
            }
        );
    }

    #[test]
    fn active_game_id_is_accepted_after_match() {
        assert_eq!(
            resolve_monitor_target("room-1", Some("room-1-1712345678"), "room-1-1712345678"),
            MonitorDecision::Accept {
                monitor_id: "room-1-1712345678",
            }
        );
    }

    #[test]
    fn unrelated_id_is_rejected() {
        assert_eq!(
            resolve_monitor_target("room-1", Some("room-1-1712345678"), "other"),
            MonitorDecision::NotFound { requested: "other" }
        );
    }

    #[test]
    fn with_finished_accepts_finished_game_id() {
        // active が無くなっても finished_game_id にマッチすれば Accept。
        // monitor_id はフォールバックチェイン (active → finished → room) で
        // finished の値が採用される。
        assert_eq!(
            resolve_monitor_target_with_finished(
                "room-1",
                None,
                Some("room-1-1712345678"),
                "room-1-1712345678",
            ),
            MonitorDecision::Accept {
                monitor_id: "room-1-1712345678",
            }
        );
    }

    #[test]
    fn with_finished_active_takes_precedence_when_both_match() {
        // active と finished の両方が設定されている特異ケース (実運用では起きない
        // が、API 契約として優先順位を固定する)。
        assert_eq!(
            resolve_monitor_target_with_finished(
                "room-1",
                Some("active-id"),
                Some("finished-id"),
                "active-id",
            ),
            MonitorDecision::Accept {
                monitor_id: "active-id",
            }
        );
    }

    #[test]
    fn with_finished_room_id_match_returns_active_when_present() {
        // room_id 一致の Accept でも monitor_id は active を優先する。これは
        // 既存 `resolve_monitor_target` と同じ意味論を踏襲する。
        assert_eq!(
            resolve_monitor_target_with_finished(
                "room-1",
                Some("room-1-1712345678"),
                None,
                "room-1",
            ),
            MonitorDecision::Accept {
                monitor_id: "room-1-1712345678",
            }
        );
    }

    #[test]
    fn with_finished_unrelated_id_is_rejected() {
        assert_eq!(
            resolve_monitor_target_with_finished(
                "room-1",
                None,
                Some("room-1-1712345678"),
                "other",
            ),
            MonitorDecision::NotFound { requested: "other" }
        );
    }

    #[test]
    fn with_finished_no_active_no_finished_only_room_match() {
        // active も finished も無い (fetch だけ通って LOGIN 前の DO) で room_id
        // 直指定された場合、monitor_id は room_id にフォールバックする。
        assert_eq!(
            resolve_monitor_target_with_finished("room-1", None, None, "room-1"),
            MonitorDecision::Accept {
                monitor_id: "room-1",
            }
        );
    }
}
