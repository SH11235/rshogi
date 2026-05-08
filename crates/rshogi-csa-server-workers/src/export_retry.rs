//! 終局時 R2 export PUT 失敗時の retry policy (Issue #623)。
//!
//! 終局時に CSA 本文 / by-id / meta / games-index の 4 オブジェクトを R2 に
//! 書き出すが、Cloudflare の transient 失敗で 1 つでも PUT に失敗すると以前は
//! 棋譜が恒久欠損していた。本モジュールは:
//!
//! - DO storage に [`ExportPendingState`](crate::persistence::ExportPendingState)
//!   を残して再 PUT に必要な body / key を保持する
//! - [`PendingAlarmKind::ExportRetry`](crate::reconnect::PendingAlarmKind::ExportRetry)
//!   タグで `state.alarm()` 経路を分岐する (`KEY_FINISHED` ガードよりも前で受ける)
//! - 本モジュール内の純粋ロジック ([`next_retry_delay_ms`] / [`is_exhausted`])
//!   で次回 alarm 遅延と打ち切り判定を一元化する
//!
//! 純粋ロジックのみ置く方針なので I/O (DO storage / R2 binding) は呼び出し側
//! ([`crate::game_room`]) が担う。

/// retry の遅延列 (秒)。要素長 = retry の最大回数。
///
/// `attempt = 0` (初回 finalize 直後の最初の retry alarm) は
/// `RETRY_DELAYS_SEC[0] = 30` 秒後。`attempt = 4` まで使い切ると 5 回目の
/// `next_retry_delay_ms` 呼び出しで `None` を返し、呼び出し側は
/// `KEY_PENDING_ALARM_KIND` を消して alarm を停止する (= pending entry のみ
/// 残置して観測性を保つ運用へ移行する)。
///
/// 30 / 60 / 120 / 300 / 600 秒の指数寄り。R2 の数分単位の transient 障害を
/// 拾いやすく、かつ Workers 側の wall-clock を浪費しすぎない範囲。
pub const RETRY_DELAYS_SEC: [u64; 5] = [30, 60, 120, 300, 600];

/// 次回 retry alarm までの遅延 (ミリ秒) を返す。
///
/// `attempt` は **これから実行する** retry の試行番号 (0 始まり)。`None` を
/// 返すケースは呼び出し側で打ち切り (= alarm 停止 + pending 残置) する契約。
pub fn next_retry_delay_ms(attempt: u32) -> Option<u64> {
    RETRY_DELAYS_SEC.get(attempt as usize).map(|sec| sec.saturating_mul(1000))
}

/// 全 retry を消費しきっているか (= 次回 alarm を張れない)。
///
/// `next_retry_delay_ms(attempt).is_none()` と同値だが、呼び出し側で
/// 「打ち切り判定」セマンティクスを明示するために別名で公開する。
pub fn is_exhausted(attempt: u32) -> bool {
    next_retry_delay_ms(attempt).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delays_have_expected_sequence() {
        // Codex 設計レビュー (v2 COMMENT) で承認された 30/60/120/300/600 秒列。
        // 値を変更すると wall-clock 上の retry 挙動が変わるため、契約として固定する。
        assert_eq!(RETRY_DELAYS_SEC, [30, 60, 120, 300, 600]);
    }

    #[test]
    fn next_retry_delay_ms_returns_some_for_in_range_attempts() {
        assert_eq!(next_retry_delay_ms(0), Some(30_000));
        assert_eq!(next_retry_delay_ms(1), Some(60_000));
        assert_eq!(next_retry_delay_ms(2), Some(120_000));
        assert_eq!(next_retry_delay_ms(3), Some(300_000));
        assert_eq!(next_retry_delay_ms(4), Some(600_000));
    }

    #[test]
    fn next_retry_delay_ms_returns_none_when_exhausted() {
        assert_eq!(next_retry_delay_ms(5), None);
        assert_eq!(next_retry_delay_ms(u32::MAX), None);
    }

    #[test]
    fn is_exhausted_matches_next_retry_delay_ms_none() {
        assert!(!is_exhausted(0));
        assert!(!is_exhausted(4));
        assert!(is_exhausted(5));
        assert!(is_exhausted(u32::MAX));
    }
}
