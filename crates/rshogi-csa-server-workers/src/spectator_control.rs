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
}
